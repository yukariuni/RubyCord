//! Build-independent discovery of Desktop Phoenix's `libdiscord/crates/http`
//! `ReqwestTransport` and `SendFuture` vtable slots.
//!
//! Desktop Phoenix routes all Discord REST traffic through a single-method
//! `dyn Transport` trait (`ReqwestTransport::send(&self, req) -> Pin<Box<dyn
//! Future>>`). Swapping the `send` and `poll` vtable slots in `.rdata`
//! intercepts every outgoing request and incoming response without any inline
//! code patching.

use memchr::memchr_iter;
use phoenix_hook::{
   image::Image,
   x86::{
      Abi,
      call_target,
      jump_target,
   },
};

use crate::Found;

/// Unique error string inside `ReqwestTransport::send` when handed a mobile
/// host-file body.
pub(crate) const HOST_FILE_ANCHOR: &[u8] = b"a host file body needs the host's transport";

/// The size of `http::HeaderMap`, which `send`'s request starts with.
pub const HEADER_MAP_SIZE: usize = 0x60;
/// The size of the request's body enum.
pub const BODY_SIZE: usize = 0x38;
/// The size of a `String`, and of an `http::Method`.
pub const STRING_SIZE: usize = 0x18;
/// The size of `poll`'s result while it carries the body in a `Vec`.
pub const BUFFERED_RESULT_SIZE: usize = 0x80;
/// The size of `poll`'s result once it carries the body as a stream, on
/// build 2233.
pub const STREAMED_RESULT_SIZE: usize = 0x1C8;
/// Where a streamed result keeps its body's kind, then the boxed
/// `dyn Stream`.
pub const STREAMED_BODY: usize = 0x178;

/// Where `send`'s request keeps the fields the runtime reads, after the
/// `http::HeaderMap` it starts with. Builds move them as fields are added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestLayout {
   /// The body enum's tag, followed by its `Bytes`.
   pub body:   usize,
   /// The URL `String`.
   pub url:    usize,
   /// The `http::Method`.
   pub method: usize,
}

/// How `poll` hands back a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseLayout {
   /// The whole body in a `Vec`, in a [`BUFFERED_RESULT_SIZE`] result.
   Buffered,
   /// The body as a stream at [`STREAMED_BODY`], read after `poll` returns.
   Streamed {
      /// The `u16` status.
      status: usize,
   },
}

/// Located vtable slots and host allocator entry points for Desktop Phoenix's
/// `libdiscord/crates/http` transport.
#[derive(Debug, Clone)]
pub struct DesktopHttpTransport {
   /// All `.rdata` vtable slots holding `ReqwestTransport::send` (may be
   /// duplicated across LLVM CGUs).
   pub send_slots:   Vec<usize>,
   /// Original `ReqwestTransport::send` function address.
   pub send_fn:      usize,
   /// Where `send`'s request keeps its fields.
   pub request:      RequestLayout,
   /// All `.rdata` vtable slots holding `SendFuture::poll`.
   pub poll_slots:   Vec<usize>,
   /// Original `SendFuture::poll` function address.
   pub poll_fn:      usize,
   /// How `poll`'s result holds the response.
   pub response:     Found<ResponseLayout>,
   /// Host's `__rust_alloc(size, align)` if located.
   pub host_alloc:   Option<usize>,
   /// Host's `__rust_dealloc(ptr, size, align)` if located.
   pub host_dealloc: Option<usize>,
}

impl DesktopHttpTransport {
   /// Scans a 64-bit PE or ELF `Image` for `ReqwestTransport::send`, its
   /// returned `SendFuture::poll` vtable, and the host's Rust allocator
   /// functions.
   pub fn locate(image: &Image<'_>) -> Found<Self> {
      if image.pointer_width != 8 {
         return Err("not a 64-bit image".to_owned());
      }
      let abi = Abi::of(image);

      // 1. The unique anchor string inside `ReqwestTransport::send`.
      let anchor = crate::one("transport anchor", &image.find_bytes(HOST_FILE_ANCHOR))?;
      let lea = *crate::desktop::lea_refs(image, anchor)
         .first()
         .ok_or("transport anchor is never loaded")?;
      let (send_fn, send_end) = image
         .function_bounds(lea)
         .ok_or("ReqwestTransport::send has no unwind entry")?;
      let request = request_layout(
         abi,
         image
            .read(send_fn, send_end - send_fn)
            .ok_or("send is unreadable")?,
         send_fn,
      )?;

      // 2. Every `dyn Transport` vtable slot in `.rdata` pointing to `send_fn`.
      let send_slots = find_vtable_method_slots(image, send_fn, 1..=0x2000);
      if send_slots.is_empty() {
         return Err("no Transport vtable holds ReqwestTransport::send".to_owned());
      }

      // 3. The `Box::pin` of the async state machine returns `(ptr,
      //    poll_vtable)`, from `send_fn` itself or a small helper it calls.
      let (poll_fn, poll_vtable, box_future) = find_poll_vtable(image, send_fn, send_end)?;
      let mut poll_slots = find_vtable_method_slots(image, poll_fn, 0x100..=0x4000);
      let response = image
         .function_bounds(poll_fn)
         .and_then(|(start, end)| image.read(start, end - start).map(|code| (start, code)))
         .ok_or_else(|| "SendFuture::poll is unreadable".to_owned())
         .and_then(|(start, code)| response_layout(abi, code, start));
      let direct_slot = poll_vtable + 24;
      if !poll_slots.contains(&direct_slot) {
         poll_slots.push(direct_slot);
      }

      // 4. The host's own `__rust_alloc` and `__rust_dealloc`, so response body
      //    growth stays allocator-compatible. Both hand the same global
      //    allocator static to its implementation.
      let host_alloc = box_future.and_then(|box_future| find_host_alloc(image, abi, box_future));
      let host_dealloc = find_host_dealloc(image, abi, send_fn, send_end)
         .or_else(|| host_alloc.and_then(|alloc| dealloc_sharing(image, abi, alloc)));
      let (host_alloc, host_dealloc) = match (host_alloc, host_dealloc) {
         (Some(alloc), Some(dealloc))
            if allocator_static(image, abi, alloc).is_none()
               || allocator_static(image, abi, alloc) != allocator_static(image, abi, dealloc) =>
         {
            (None, None)
         },
         pair => pair,
      };

      Ok(Self {
         send_slots,
         send_fn,
         request,
         poll_slots,
         poll_fn,
         response,
         host_alloc,
         host_dealloc,
      })
   }
}

/// Reads where `send` at `ip` keeps its request's fields. It moves the
/// headers out whole right after the call that starts the `reqwest` request
/// from the method and URL, and dispatches on the body's tag through a jump
/// table.
fn request_layout(abi: Abi, code: &[u8], ip: usize) -> Found<RequestLayout> {
   let used = phoenix_hook::x86::arg_use(abi, 1, code, ip);
   let copy = used
      .copies
      .iter()
      .find(|copy| copy.offset == 0 && copy.len == HEADER_MAP_SIZE)
      .ok_or("send never moves the request's headers out")?;
   let call = copy
      .after_calls
      .checked_sub(1)
      .and_then(|at| used.calls.get(at))
      .ok_or("no call before the headers move")?;
   let (Some(method), Some(url)) = (call[2], call[3]) else {
      return Err(format!(
         "the request call takes {call:?}, not a method and URL"
      ));
   };
   let mut dispatches = used.dispatches.clone();
   dispatches.sort_unstable();
   dispatches.dedup();
   let [body] = dispatches[..] else {
      return Err(format!("{dispatches:#x?} could each be the body's tag"));
   };
   // The runtime takes a reference to each field, so they must not overlap.
   let mut fields = [
      (0, HEADER_MAP_SIZE),
      (body, BODY_SIZE),
      (url, STRING_SIZE),
      (method, STRING_SIZE),
   ];
   fields.sort_unstable();
   if fields
      .windows(2)
      .any(|pair| pair[0].0 + pair[0].1 > pair[1].0)
      || fields.iter().any(|&(offset, _)| offset % 8 != 0)
   {
      return Err(format!(
         "body {body:#x}, url {url:#x} and method {method:#x} overlap"
      ));
   }
   Ok(RequestLayout { body, url, method })
}

/// Reads how `poll` at `ip` returns its response from the stores through its
/// result pointer.
fn response_layout(abi: Abi, code: &[u8], ip: usize) -> Found<ResponseLayout> {
   // shortcut: a result pointer `poll` keeps on the stack is not followed, so
   // this only catches a build that writes past the layout through a register,
   // as 2233 does.
   let extent = phoenix_hook::x86::arg_use(abi, 0, code, ip).extent;
   if extent <= BUFFERED_RESULT_SIZE {
      return Ok(ResponseLayout::Buffered);
   }
   let stores = phoenix_hook::x86::sret_stores(abi, code, ip);
   let body = [STREAMED_BODY, STREAMED_BODY + 8, STREAMED_BODY + 16]
      .iter()
      .all(|&offset| stores.contains(&(offset, 8)));
   let statuses = stores
      .iter()
      .filter(|&&(_, size)| size == 2)
      .map(|&(offset, _)| offset)
      .collect::<Vec<_>>();
   match statuses[..] {
      [status] if body && extent <= STREAMED_RESULT_SIZE => Ok(ResponseLayout::Streamed { status }),
      _ => Err(format!("poll writes {extent:#x} bytes of its result")),
   }
}

/// Finds all `.rdata` vtable method-0 slots (`vtable + 24`) holding `method_fn`
/// where `[drop_fn, size, align, method_fn]` passes Rust trait vtable checks.
pub(crate) fn find_vtable_method_slots(
   image: &Image<'_>,
   method_fn: usize,
   size_range: std::ops::RangeInclusive<usize>,
) -> Vec<usize> {
   let mut slots = Vec::new();
   for slot in image.pointers_to(method_fn) {
      let Some(vtable) = slot.checked_sub(24) else {
         continue;
      };
      let (Some(drop_fn), Some(size), Some(align)) = (
         image.read_usize(vtable),
         image.read_usize(vtable + 8),
         image.read_usize(vtable + 16),
      ) else {
         continue;
      };
      if image.is_function(drop_fn)
         && size_range.contains(&size)
         && align.is_power_of_two()
         && align <= 64
      {
         slots.push(slot);
      }
   }
   slots.sort_unstable();
   slots.dedup();
   slots
}

/// The functions called inside `[start, end)`.
fn calls(image: &Image<'_>, start: usize, end: usize) -> Vec<usize> {
   crate::desktop::callees(image, start, end)
      .into_iter()
      .filter(|&callee| image.is_function(callee))
      .collect()
}

/// The bounds of `func` when it is at most `max` bytes long.
fn small(image: &Image<'_>, func: usize, max: usize) -> Option<(usize, usize)> {
   image
      .function_bounds(func)
      .filter(|&(start, end)| end - start <= max)
}

/// The `SendFuture` vtable loaded with `lea rdx, [rip + disp32]` (`48 8D 15`),
/// the high half of the returned fat pointer, in a small callee of `send_fn`
/// that boxes the future, or else in `send_fn`. Returns `(poll_fn, vtable,
/// box_future)`, where `box_future` is the callee that boxes it, when one does.
///
/// `lea rdx` also passes an argument, so the vtable must be the only one in
/// whichever of the two places has any.
fn find_poll_vtable(
   image: &Image<'_>,
   send_fn: usize,
   send_end: usize,
) -> Found<(usize, usize, Option<usize>)> {
   let boxers: Vec<(usize, usize)> = calls(image, send_fn, send_end)
      .into_iter()
      .filter_map(|callee| small(image, callee, 0x80))
      .collect();
   for group in [boxers, vec![(send_fn, send_end)]] {
      let mut found: Vec<(usize, usize, usize)> = group
         .into_iter()
         .flat_map(|(cs, ce)| {
            future_vtables(image, cs, ce).map(move |(vtable, poll_fn)| (vtable, poll_fn, cs))
         })
         .collect();
      found.sort_unstable();
      found.dedup_by_key(|&mut (vtable, ..)| vtable);
      match found[..] {
         [] => {},
         [(vtable, poll_fn, cs)] => return Ok((poll_fn, vtable, (cs != send_fn).then_some(cs))),
         _ => return Err(format!("{} SendFuture vtable candidates", found.len())),
      }
   }
   Err("SendFuture vtable not found".to_owned())
}

/// `(vtable, poll_fn)` for each `lea rdx, [rip + vtable]` in `[cs, ce)` whose
/// target reads as a future's vtable.
fn future_vtables(image: &Image<'_>, cs: usize, ce: usize) -> impl Iterator<Item = (usize, usize)> {
   let code = image.read(cs, ce - cs).unwrap_or_default();
   memchr_iter(0x8D, code)
      .filter(move |&pos| {
         pos > 0 && pos + 6 <= code.len() && code[pos - 1] == 0x48 && code[pos + 1] == 0x15
      })
      .filter_map(move |pos| {
         let disp = i32::from_le_bytes(code[pos + 2..pos + 6].try_into().ok()?);
         let vtable = (cs + pos + 6).checked_add_signed(disp as isize)?;
         let (drop_fn, size, align, poll_fn) = (
            image.read_usize(vtable)?,
            image.read_usize(vtable + 8)?,
            image.read_usize(vtable + 16)?,
            image.read_usize(vtable + 24)?,
         );
         (image.is_function(drop_fn)
            && image.is_function(poll_fn)
            && (0x100..=0x4000).contains(&size)
            && align == 8)
            .then_some((vtable, poll_fn))
      })
      .collect::<Vec<_>>()
      .into_iter()
}

/// `__rust_alloc(size, align)`, reached from `box_future`.
///
/// Some call in `box_future` or a small callee passes the future's alignment,
/// a constant 8, in an argument register. Win64 builds make that the call to
/// `__rust_alloc`, System V ones a call to `Layout` wrappers that forward to
/// it, so small callees are followed breadth-first to the first function
/// shaped like `__rust_alloc`: its first call, whether the allocator is set
/// up, is followed by `test al, al`. Bool helpers share that shape, so the
/// first level with any must have exactly one.
fn find_host_alloc(image: &Image<'_>, abi: Abi, box_future: usize) -> Option<usize> {
   let (bs, be) = image.function_bounds(box_future)?;
   let align_8: Vec<Vec<u8>> = (0..2)
      .flat_map(|index| crate::desktop::imm_loads(abi.arg(index), 8))
      .collect();
   let mut frontier = Vec::new();
   for (ws, we) in std::iter::once((bs, be)).chain(
      calls(image, bs, be)
         .into_iter()
         .filter_map(|f| small(image, f, 0x60)),
   ) {
      let code = image.read(ws, we - ws)?;
      for pos in 0..code.len() {
         let Some((_, callee)) = call_target(image, &code[pos..], ws + pos) else {
            continue;
         };
         let before = &code[pos.saturating_sub(12)..pos];
         if align_8
            .iter()
            .any(|load| before.windows(load.len()).any(|w| w == load))
            && image.is_function(callee)
         {
            frontier.push(callee);
         }
      }
   }
   for _ in 0..3 {
      let mut found: Vec<usize> = frontier
         .iter()
         .copied()
         .filter(|&func| checks_allocator(image, func))
         .collect();
      found.sort_unstable();
      found.dedup();
      match found[..] {
         [] => {},
         [alloc] => return Some(alloc),
         _ => return None,
      }
      frontier = frontier
         .iter()
         .filter_map(|&func| small(image, func, 0x60))
         .flat_map(|(start, end)| calls(image, start, end))
         .collect();
   }
   None
}

/// Whether the first call in `func` is followed by `test al, al`. Linux
/// builds since 2026-11-26 put a one-byte `ret` shim right before
/// `__rust_alloc`, so the read stops at the function's end.
fn checks_allocator(image: &Image<'_>, func: usize) -> bool {
   let len = image
      .function_bounds(func)
      .map_or(0x20, |(_, end)| (end - func).min(0x20));
   let Some(code) = image.read(func, len) else {
      return false;
   };
   phoenix_hook::x86::disasm(code, func, 12)
      .iter()
      .find(|line| line.text.starts_with("call "))
      .is_some_and(|call| image.read(call.addr + call.len, 2) == Some(&[0x84, 0xC0]))
}

/// Follows `send_fn` → request builder helper → leaf `Method::drop`, a tail
/// call `__rust_dealloc(ptr, size, 1)` that sets only the alignment
/// argument. Leaf functions have no `.pdata` entry on Windows x64, so inner
/// call targets are checked via `is_code`. Other leaves end the same way, so
/// the target must also count the freed bytes down with `lock sub`.
fn find_host_dealloc(
   image: &Image<'_>,
   abi: Abi,
   send_fn: usize,
   send_end: usize,
) -> Option<usize> {
   let tails = crate::desktop::imm_loads(abi.arg(2), 1);
   let dealloc_in = |code: &[u8], ip: usize| {
      tails.iter().find_map(|tail| {
         memchr::memmem::find_iter(code, tail).find_map(|hit| {
            // A size-optimized leaf may load the pointer between the two.
            let after = hit + tail.len();
            (after..code.len().min(after + 8)).find_map(|at| {
               jump_target(image, &code[at..], ip + at)
                  .filter(|&dealloc| image.is_function(dealloc) && counts_down(image, dealloc))
            })
         })
      })
   };
   for callee in calls(image, send_fn, send_end) {
      let Some((cs, ce)) = small(image, callee, 0x400) else {
         continue;
      };
      let Some(callee_code) = image.read(cs, ce - cs) else {
         continue;
      };
      for pos in 0..callee_code.len() {
         let Some((_, inner)) = call_target(image, &callee_code[pos..], cs + pos) else {
            continue;
         };
         if let Some(code) = image.read(inner, 0x30).filter(|_| image.is_code(inner))
            && let Some(dealloc) = dealloc_in(code, inner)
         {
            return Some(dealloc);
         }
      }
   }
   None
}

/// Whether `func` counts freed bytes down with `lock sub` near its start.
fn counts_down(image: &Image<'_>, func: usize) -> bool {
   image.read(func, 0x40).is_some_and(|body| {
      body
         .windows(3)
         .any(|w| w == [0xF0, 0x48, 0x29] || w == [0xF0, 0x4C, 0x29])
   })
}

/// `__rust_dealloc`, from the other allocator shims that load `alloc`'s
/// global allocator static.
///
/// System V builds free request bodies through wrappers the leaf search in
/// [`find_host_dealloc`] does not reach. `alloc` hands the global allocator
/// static to its implementation with `lea rdi, [rip + disp32]`, and so does
/// `__rust_dealloc`, without `alloc`'s set-up check and counting the freed
/// bytes down after. Linux build 2233 emits the two 0x45 bytes apart, later
/// builds 0x2ce8.
fn dealloc_sharing(image: &Image<'_>, abi: Abi, alloc: usize) -> Option<usize> {
   let global = allocator_static(image, abi, alloc)?;
   let mut found: Vec<usize> = crate::desktop::lea_refs(image, global)
      .into_iter()
      .filter(|&lea| {
         image
            .read(lea, 7)
            .is_some_and(|code| lea_arg0(abi, code, lea).next().is_some())
      })
      .filter_map(|lea| Some(image.function_bounds(lea)?.0))
      .filter(|&func| func != alloc && !checks_allocator(image, func) && counts_down(image, func))
      .collect();
   found.dedup();
   match found[..] {
      [dealloc] => Some(dealloc),
      _ => None,
   }
}

/// The global allocator static an allocator shim hands its implementation:
/// the first `lea` of it into the first argument register.
fn allocator_static(image: &Image<'_>, abi: Abi, shim: usize) -> Option<usize> {
   lea_arg0(abi, image.read(shim, 0x40)?, shim)
      .next()
      .map(|(_, target)| target)
}

/// Each `lea <first argument>, [rip + disp32]` in `code` at `ip`, as
/// (instruction, target).
fn lea_arg0(abi: Abi, code: &[u8], ip: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
   let lea: &'static [u8] = match abi {
      Abi::Win64 => &[0x48, 0x8D, 0x0D],
      Abi::SysV => &[0x48, 0x8D, 0x3D],
   };
   memchr::memmem::find_iter(code, lea).filter_map(move |pos| {
      let disp = i32::from_le_bytes(code.get(pos + 3..pos + 7)?.try_into().ok()?);
      Some((ip + pos, (ip + pos + 7).checked_add_signed(disp as isize)?))
   })
}

#[cfg(test)]
mod tests {
   use super::*;

   /// The client call and headers move of `send` in the Linux build from
   /// 2026-11-23, around a body tag dispatch.
   const SEND: &[u8] = &[
      0x48, 0x89, 0xF3, // mov rbx, rsi
      0x48, 0x8B, 0x83, 0x08, 0x01, 0x00, 0x00, // mov rax, [rbx+0x108]
      0x48, 0x8D, 0x94, 0x24, 0x80, 0x09, 0x00, 0x00, // lea rdx, [rsp+0x980]
      0x48, 0x89, 0x42, 0x10, // mov [rdx+0x10], rax
      0x0F, 0x10, 0x83, 0xF8, 0x00, 0x00, 0x00, // movups xmm0, [rbx+0xF8]
      0x0F, 0x29, 0x02, // movaps [rdx], xmm0
      0x48, 0x8D, 0x8B, 0xB0, 0x00, 0x00, 0x00, // lea rcx, [rbx+0xB0]
      0xE8, 0x00, 0x00, 0x00, 0x00, // call
      0x6A, 0x60, // push 0x60
      0x59, // pop rcx
      0x48, 0x89, 0xDE, // mov rsi, rbx
      0xF3, 0xA4, // rep movsb
      0x48, 0x8B, 0x43, 0x78, // mov rax, [rbx+0x78]
      0x49, 0x63, 0x04, 0x87, // movsxd rax, [r15+rax*4]
   ];

   #[test]
   fn reads_the_request_layout_from_send() {
      assert_eq!(
         request_layout(Abi::SysV, SEND, 0x1000),
         Ok(RequestLayout {
            body:   0x78,
            url:    0xB0,
            method: 0xF8,
         })
      );
   }

   #[test]
   fn refuses_overlapping_fields() {
      // The body's tag read from inside the headers instead.
      let mut send = SEND.to_vec();
      let tag = send
         .windows(4)
         .position(|code| code == [0x48, 0x8B, 0x43, 0x78])
         .expect("SEND reads the tag");
      send[tag + 3] = 0x58;
      assert!(request_layout(Abi::SysV, &send, 0x1000).is_err());
   }
}
