//! Locators for macOS Phoenix, the arm64 desktop client.

use std::collections::{
   BTreeMap,
   HashMap,
};

use phoenix_hook::{
   arm64::{
      self,
      Insn,
      cond::{
         HI,
         NE,
      },
      decode_at,
   },
   image::Image,
   pattern::Pattern,
};

use crate::{
   Found,
   desktop::{
      ButtonLayout,
      DivLayout,
      EmojiCheck,
      Gpui,
      GpuiLayout,
      MenuLayout,
      Section,
      SettingsLabel,
      SwitchLayout,
      layout,
   },
   http::{
      DesktopHttpTransport,
      RequestLayout,
      ResponseLayout,
      find_vtable_method_slots,
   },
   one,
};

/// `Some(out)` for the first arm `value` matches, else `None`, or without
/// `in value` a closure doing that to its argument.
macro_rules! pick {
   (in $value:expr, $($pat:pat $(if $guard:expr)? => $out:expr),+ $(,)?) => {
      match $value {
         $($pat $(if $guard)? => Some($out),)+
         _ => None,
      }
   };
   ($($arms:tt)+) => {
      |value| pick!(in value, $($arms)+)
   };
}

/// The function `ClientAccessGate` reads through (1 blocked, 2 no gate).
///
/// The runtime makes it answer "no gate", as on x86. Its small caller is
/// `signed_in && access(..) != 2`: `ldr x8, [x0, #state]; cmp x8, #3; b.ne;
/// stp; mov x29, sp; bl access; bl stub; cset w0, ne`, the stub being
/// `and w8, w0, #0xff; cmp w8, #2; ret`. Those two may also be inline.
pub fn access_gate(image: &Image<'_>) -> Found<usize> {
   const CMP_X8_3: u32 = 0xF100_0D1F;
   const AND_W8_W0_FF: Insn = Insn::Other(0x1200_1C08);
   const CMP_W8_2: Insn = Insn::CmpImm {
      rn:   8,
      imm:  2,
      wide: false,
   };
   const CSET_W0_NE: Insn = Insn::Cset { rd: 0, cond: NE };
   const REACH: usize = 8;

   let compares_with_2 =
      |stub: usize| decoded(image, stub, 3) == [AND_W8_W0_FF, CMP_W8_2, Insn::Ret];
   let gates = code_words(image)
      .filter(|&(_, word)| word == CMP_X8_3)
      .filter_map(|(cmp, _)| {
         let insns = decoded(image, cmp, REACH + 4);
         let [_, Insn::BCond { cond: NE, .. }, ref rest @ ..] = insns[..] else {
            return None;
         };
         let at = rest[..rest.len().saturating_sub(2)]
            .iter()
            .position(|insn| matches!(insn, Insn::Bl { .. }))?;
         let access = match rest[at..] {
            [
               Insn::Bl { target },
               Insn::Bl { target: stub },
               CSET_W0_NE,
               ..,
            ] if compares_with_2(stub) => target,
            [Insn::Bl { target }, AND_W8_W0_FF, CMP_W8_2, CSET_W0_NE, ..] => target,
            _ => return None,
         };
         image.is_function(access).then_some(access)
      });
   one("access gate", &sorted(gates.collect()))
}

/// The emoji check, which opens like x86's: `ldrb wN, [x1, #intention];
/// orr w8, wN, #2; cmp w8, #0xe; b.ne; mov w0, #3; ret`.
pub fn emoji_check(image: &Image<'_>) -> Found<EmojiCheck> {
   const CHECK: Pattern =
      Pattern::new("?? ?? 40 39 ?? ?? 1F 32 1F 39 00 71 ?? ?? 00 54 60 00 80 52 C0 03 5F D6");
   let func = one("emoji check", &image.scan(CHECK))?;
   decode_at(image, func)
      .and_then(pick!(Insn::Load { rn: 1, size: 1, offset, .. } => offset))
      .filter(|_| image.is_function(func))
      .map(|intention| EmojiCheck { func, intention })
      .ok_or_else(|| format!("emoji check at {func:#x} reads no context byte"))
}

/// The shell script Discord's updater runs to start Discord again once it has
/// quit: it waits for the old pid, opens the app bundle and removes the old
/// build.
pub const RELAUNCH_SCRIPT: &[u8] =
   br#"while kill -0 "$0" 2>/dev/null; do sleep 0.2; done; /usr/bin/open -n "$1"; rm -rf "$2""#;

pub fn relaunch_script(image: &Image<'_>) -> Found<usize> {
   one("relaunch script", &image.find_bytes(RELAUNCH_SCRIPT))
}

/// Up to `count` instructions at `at`, none if they can't all be read.
fn decoded(image: &Image<'_>, at: usize, count: usize) -> Vec<Insn> {
   let code = image.read(at, count * 4).unwrap_or_default();
   arm64::decode_all(code, at).map(|(_, insn)| insn).collect()
}

fn words(code: &[u8]) -> impl Iterator<Item = u32> {
   code
      .as_chunks::<4>()
      .0
      .iter()
      .map(|&word| u32::from_le_bytes(word))
}

fn code_words(image: &Image<'_>) -> impl Iterator<Item = (usize, u32)> {
   let segments = image.segments.iter().filter(|segment| segment.executable);
   segments.flat_map(|segment| {
      words(&segment.bytes)
         .enumerate()
         .map(|(index, word)| (segment.addr + index * 4, word))
   })
}

fn sorted<T: Ord>(mut values: Vec<T>) -> Vec<T> {
   values.sort_unstable();
   values.dedup();
   values
}

fn refs_to(image: &Image<'_>, anchors: &[usize]) -> Vec<(usize, usize)> {
   let mut ranges = anchors.iter().map(|&at| at..at + 1).collect::<Vec<_>>();
   ranges.sort_unstable_by_key(|range| range.start);
   arm64::code_refs(image, &ranges)
}

/// The functions that load or call any of `anchors`, each once, in address
/// order.
fn loaders(image: &Image<'_>, anchors: &[usize]) -> Vec<usize> {
   let sites = refs_to(image, anchors).into_iter();
   sorted(
      sites
         .filter_map(|(site, _)| image.function_containing(site))
         .collect(),
   )
}

fn callees(image: &Image<'_>, func: usize) -> Vec<usize> {
   sorted(calls(&flattened(image, func)).collect())
}

/// `libdiscord_media::fetch` on arm64, the one function that loads its
/// tracing callsite.
pub fn media_fetch(image: &Image<'_>) -> Found<usize> {
   let callsite = crate::desktop::media_fetch_callsite(image)?;
   one("libdiscord_media::fetch", &loaders(image, &[callsite]))
}

/// `Store::delete_message` on arm64, the delete a client runs on its own
/// message before Discord answers.
///
/// Its sibling is found as on x86: the one other caller of callees it shares
/// with the `MESSAGE_DELETE` handler, sharing at least 4 of them, taking the
/// store's lock with an atomic in its head, and reading its two ids through
/// `args`. The sibling never runs on a delete from the message menu.
pub fn optimistic_delete(image: &Image<'_>, message_delete: usize) -> Found<usize> {
   let shared = callees(image, message_delete);
   // Every caller of each shared callee, from one pass over the code.
   let mut parents = HashMap::<usize, Vec<usize>>::new();
   for (site, target) in refs_to(image, &shared) {
      if let Some(parent) = image.function_containing(site) {
         parents.entry(target).or_default().push(parent);
      }
   }
   for callers in parents.values_mut() {
      callers.sort_unstable();
      callers.dedup();
   }
   // LSE `cas`/`swp`/`ldadd` families or an exclusive load-acquire.
   let atomic = |word: u32| {
      word & 0x3F20_7C00 == 0x0820_7C00
         || word & 0x3B20_0C00 == 0x3820_0000
         || word & 0x3FFF_FC00 == 0x085F_FC00
   };
   let head_locks = |at: usize| {
      image
         .read(at, 0x80)
         .is_some_and(|code| words(code).any(atomic))
   };
   let locks = |func: usize| {
      let first = callees(image, func).first().copied();
      head_locks(func) || first.is_some_and(head_locks)
   };
   let siblings = parents
      .values()
      .filter(|parents| parents.len() == 2 && parents.contains(&message_delete))
      .flatten()
      .filter(|&&parent| parent != message_delete);
   let mut found = sorted(siblings.copied().collect());
   found.retain(|&other| {
      let others = callees(image, other);
      let overlap = others
         .iter()
         .filter(|callee| shared.binary_search(callee).is_ok());
      overlap.count() >= 4 && locks(other) && takes_id_pair(image, other)
   });
   let sibling = one("optimistic delete sibling", &found)?;

   // It shares the sibling's lock and the handler's map delete, the common
   // callee with the fewest callers beyond those two, and deletes right away:
   // its third call, with no branch before it.
   let lock = calls(&flattened(image, sibling)).next();
   let (remove, mut deletes) = callees(image, sibling)
      .into_iter()
      .filter(|callee| shared.binary_search(callee).is_ok())
      .filter_map(|callee| Some((callee, parents.remove(&callee)?)))
      .filter(|(_, callers)| callers.len() > 2)
      .min_by_key(|(_, callers)| callers.len())
      .ok_or("optimistic delete: no map delete shared with the handler")?;
   deletes.retain(|&other| {
      let head = flattened(image, other);
      let until = head
         .iter()
         .position(|&insn| insn == Insn::Bl { target: remove })
         .map_or(&head[..0], |at| &head[..at]);
      let branches = until.iter().any(|insn| {
         matches!(
            insn,
            Insn::BCond { .. } | Insn::Cbz { .. } | Insn::Tbz { .. }
         )
      });
      other != message_delete
         && takes_id_pair(image, other)
         && calls(until).count() == 2
         && calls(until).next() == lock
         && !branches
   });
   one("optimistic delete", &deletes)
}

/// Whether `func` reads `args` as `[&channel_id, &message_id]`: `mov xA, x1
/// ... ldp xC, xM, [xA]`. The bulk delete beside it takes a slice and a count
/// instead.
fn takes_id_pair(image: &Image<'_>, func: usize) -> bool {
   let mut head = flattened(image, func);
   head.truncate(32);
   let args = copy_of(&head, 1);
   head
      .iter()
      .any(|&insn| matches!(pair(insn), Some((true, _, _, rn, 0)) if Some(rn) == args))
}

/// `mov w0, #2; ret`: the access function's "no gate".
pub const NO_GATE: [u8; 8] = [0x40, 0x00, 0x80, 0x52, 0xC0, 0x03, 0x5F, 0xD6];

/// `mov w0, #0; ret`: `enforced` answering false.
pub const NOT_ENFORCED: [u8; 8] = [0x00, 0x00, 0x80, 0x52, 0xC0, 0x03, 0x5F, 0xD6];

/// [`DesktopHttpTransport::locate`] for arm64.
///
/// It finds `send` by the anchor string it loads, its `dyn Transport` slots by
/// value, and the `SendFuture` vtable by the `adrp` pair that loads it in
/// `send` or a small callee that boxes the future. The host allocator is not
/// located, so response bodies keep Discord's own buffers.
pub fn http_transport(image: &Image<'_>) -> Found<DesktopHttpTransport> {
   let anchors = image.find_bytes(crate::http::HOST_FILE_ANCHOR);
   let anchor = one("transport anchor", &anchors)?;
   let sites = refs_to(image, &[anchor])
      .into_iter()
      .filter_map(|(site, _)| image.function_containing(site))
      .collect::<Vec<_>>();
   let send_fn = one("transport anchor load", &sites)?;
   let send_slots = find_vtable_method_slots(image, send_fn, 1..=0x2000);
   if send_slots.is_empty() {
      return Err("no Transport vtable holds ReqwestTransport::send".to_owned());
   }
   let (_, body) = arm64::function(image, send_fn).ok_or("send has no bounds")?;
   let send_bounds = image.function_bounds(send_fn).ok_or("send has no bounds")?;
   let small = |at: usize| {
      image
         .function_bounds(at)
         .filter(|&(start, end)| start == at && end - start <= 0x80)
   };
   let boxers = body
      .iter()
      .filter_map(pick!(&(_, Insn::Bl { target }) => small(target)?))
      .collect::<Vec<_>>();
   let mut found = future_vtables(image, &boxers);
   if found.is_empty() {
      found = future_vtables(image, &[send_bounds]);
   }
   let [(vtable, poll_fn)] = found[..] else {
      return Err(format!("{} SendFuture vtable candidates", found.len()));
   };
   let mut poll_slots = find_vtable_method_slots(image, poll_fn, 0x100..=0x4000);
   if !poll_slots.contains(&(vtable + 24)) {
      poll_slots.push(vtable + 24);
   }
   Ok(DesktopHttpTransport {
      send_slots,
      send_fn,
      request: MACOS_REQUEST,
      poll_slots,
      poll_fn,
      response: Ok(ResponseLayout::Streamed { status: 0x68 }),
      host_alloc: None,
      host_dealloc: None,
   })
}

/// `send`'s request on build 2233, read off live requests and `send`'s drop
/// glue, with three more words after the headers and six timeout words before
/// the method.
const MACOS_REQUEST: RequestLayout = RequestLayout {
   body:   0x78,
   url:    0xB0,
   method: 0xF8,
};

/// `(vtable, poll_fn)` for each address one of `bounds` builds that reads as a
/// boxed future's vtable: `[drop, size, align 8, poll]`.
fn future_vtables(image: &Image<'_>, bounds: &[(usize, usize)]) -> Vec<(usize, usize)> {
   let mut found = Vec::new();
   for &(start, end) in bounds {
      let code = image.read(start, end - start).unwrap_or_default();
      for (_, vtable) in loaded(&arm64::decode_all(code, start).collect::<Vec<_>>())
         .into_iter()
         .flatten()
      {
         let read = |at: usize| image.read_usize(vtable + at);
         if let (Some(drop_fn), Some(size), Some(8), Some(poll_fn)) =
            (read(0), read(8), read(16), read(24))
            && image.is_function(drop_fn)
            && image.is_function(poll_fn)
            && (0x100..=0x4000).contains(&size)
         {
            found.push((vtable, poll_fn));
         }
      }
   }
   sorted(found)
}

/// [`crate::desktop::settings_label_parts`] for arm64: the packed literal
/// `PhoenixDESKTOP_PHOENIX` whose name and id one function loads.
pub fn settings_label_parts(image: &Image<'_>) -> Found<SettingsLabel> {
   const NAME: usize = b"Phoenix".len();
   let mut found = image.find_bytes(b"PhoenixDESKTOP_PHOENIX");
   found.retain(|&name| {
      let names = loaders(image, &[name]);
      loaders(image, &[name + NAME])
         .iter()
         .any(|func| names.contains(func))
   });
   let name = one("settings label loader", &found)?;
   Ok(SettingsLabel {
      name,
      id: name + NAME,
   })
}

/// Every copy of `is_page_visible`'s page-range guard, which the runtime
/// lowers so page 24 is always shown.
///
/// Pages 9 to 24 take a switch on flags and the rest are visible. The function
/// is also inlined, into the sidebar among others, so each copy is matched by
/// what it does: `sub wA, wB, #9; cmp wA, #0xf; b.hi visible`, where `visible`
/// is `cmp wB, #0; cset wN, ne`. The `cmp` addresses are returned.
pub fn settings_visibility(image: &Image<'_>) -> Found<Vec<usize>> {
   // `sub wA, wB, #9`.
   let subs = code_words(image).filter(|&(_, word)| word & 0xFFFF_FC00 == 0x5100_2400);
   let found = subs
      .filter_map(|(sub_at, sub)| {
         let (page, range) = (((sub >> 5) & 0x1F) as u8, (sub & 0x1F) as u8);
         let guard = (decode_at(image, sub_at + 4)?, decode_at(image, sub_at + 8)?);
         let visible = pick!(in guard,
            (Insn::CmpImm { rn, imm: 0xF, wide: false }, Insn::BCond { cond: HI, target })
               if rn == range => target
         )?;
         let answer = (decode_at(image, visible)?, decode_at(image, visible + 4)?);
         let returns_nonzero = matches!(
            answer,
            (Insn::CmpImm { rn, imm: 0, wide: false }, Insn::Cset { cond: NE, .. }) if rn == page
         );
         returns_nonzero.then_some(sub_at + 4)
      })
      .collect::<Vec<_>>();
   match found.len() {
      1..=4 => Ok(found),
      count => Err(format!("settings visibility guard matched {count} times")),
   }
}

/// `cmp wN, #0xe`, written over the visibility guard's `cmp wN, #0xf`.
#[must_use]
pub const fn lowered_guard(cmp: u32) -> u32 {
   (cmp & !(0xFFF << 10)) | (0xE << 10)
}

/// [`crate::desktop::settings_section`] for arm64.
///
/// `host::section` loads `account-loading`, and early on reads the active page
/// off its first argument, through whichever register it copied that into:
/// `mov xS, x0 ... ldrb wN, [xS, #page]`.
pub fn settings_section(image: &Image<'_>) -> Found<Section> {
   let anchor = one("account-loading", &image.find_bytes(b"account-loading"))?;
   let func = one("account-loading load", &loaders(image, &[anchor]))?;
   let (_, body) = arm64::function(image, func).ok_or("settings section has no bounds")?;
   let mut state = vec![0];
   let page_offset = body
      .iter()
      .take(0x60)
      .find_map(|&(_, insn)| {
         state.extend(pick!(in insn, Insn::MovReg { rd, rm: 0, wide: true } => rd));
         pick!(in insn,
            Insn::Load { rn, offset, size: 1, .. }
               if state.contains(&rn) && (0x100..0x2000).contains(&offset) => offset
         )
      })
      .ok_or("settings section active page load not found")?;
   Ok(Section { func, page_offset })
}

/// What a register holds while a function body is read: an offset into the
/// function's first argument, an absolute address, or a constant.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Held {
   SelfAt(usize),
   Addr(usize),
   Imm(u64),
}

type Regs = [Option<Held>; 32];

fn step(regs: &mut Regs, insn: Insn) {
   match insn {
      Insn::Adrp { rd, page } => regs[usize::from(rd)] = Some(Held::Addr(page)),
      Insn::AddImm { rd, rn, imm } => {
         regs[usize::from(rd)] = pick!(in regs[usize::from(rn)],
            Some(Held::SelfAt(at)) => Held::SelfAt(at + imm),
            Some(Held::Addr(at)) => Held::Addr(at + imm),
         );
      },
      Insn::MovReg { rd, rm, wide: true } => regs[usize::from(rd)] = regs[usize::from(rm)],
      Insn::MovZ { rd, imm, .. } => regs[usize::from(rd)] = Some(Held::Imm(imm)),
      // Calls keep only the callee-saved registers.
      Insn::Bl { .. } | Insn::Blr { .. } => regs[..19].fill(None),
      Insn::Load { rt, .. } | Insn::Cset { rd: rt, .. } | Insn::MovN { rd: rt, .. } => {
         regs[usize::from(rt)] = None;
      },
      Insn::MovReg { rd, .. } => regs[usize::from(rd)] = None,
      Insn::Writeback { rt, rn, imm, load } => {
         regs[usize::from(rn)] = match regs[usize::from(rn)] {
            Some(Held::SelfAt(at)) => at.checked_add_signed(imm).map(Held::SelfAt),
            _ => None,
         };
         if load {
            regs[usize::from(rt)] = None;
         }
      },
      // Anything else that writes a register forgets it.
      Insn::Other(word) => regs[(word & 0x1F) as usize] = None,
      _ => {},
   }
}

/// Every call in `body` with the registers that reach it on every path, from
/// `entry` at the start. Branches are followed, and where paths meet a
/// register keeps a value only when they agree.
fn calls_with_regs(body: &[(usize, Insn)], entry: &Regs) -> Vec<(usize, usize, Regs)> {
   let mut seen: HashMap<usize, Regs> = HashMap::new();
   let mut calls = BTreeMap::new();
   let mut work = vec![(0usize, *entry)];
   // Bounded, since each revisit only forgets registers.
   let mut budget = body.len() * 8;
   while let Some((mut index, mut regs)) = work.pop() {
      loop {
         if budget == 0 {
            return Vec::new();
         }
         budget -= 1;
         let Some(&(pc, insn)) = body.get(index) else {
            break;
         };
         if let Some(known) = seen.get(&index) {
            let merged =
               std::array::from_fn(|at| (known[at] == regs[at]).then_some(known[at]).flatten());
            if merged == *known {
               break;
            }
            regs = merged;
         }
         seen.insert(index, regs);
         if let Insn::Bl { target } = insn {
            calls.insert(pc, (target, regs));
         }
         let branch = match insn {
            Insn::BCond { target, .. } | Insn::Cbz { target, .. } | Insn::Tbz { target, .. } => {
               Some((target, true))
            },
            Insn::B { target } => Some((target, false)),
            Insn::Ret | Insn::Br { .. } => break,
            _ => None,
         };
         step(&mut regs, insn);
         if let Some((target, falls_through)) = branch {
            if let Ok(to) = body.binary_search_by_key(&target, |&(pc, _)| pc) {
               work.push((to, regs));
            }
            if !falls_through {
               break;
            }
         }
         index += 1;
      }
   }
   calls
      .into_iter()
      .map(|(pc, (target, regs))| (pc, target, regs))
      .collect()
}

/// Field name to offset in a `Debug::fmt` that calls `DebugStruct::field`
/// with `x1`/`w2` the name, `x3` `self + offset` and `x4` the field's `Debug`
/// vtable, with nested structs as `outer.inner`, followed through the
/// vtable's `fmt` `depth` deep. The `field` call is the one most calls in
/// the function go to with that shape.
fn debug_offsets(image: &Image<'_>, func: usize, depth: usize) -> HashMap<String, usize> {
   let mut out = HashMap::new();
   let Some((_, body)) = arm64::function(image, func) else {
      return out;
   };
   let entry: Regs = std::array::from_fn(|at| (at == 0).then_some(Held::SelfAt(0)));
   let calls = calls_with_regs(&body, &entry)
      .into_iter()
      .filter_map(|(_, target, regs)| {
         let (name, len, offset) = pick!(in (regs[1], regs[2], regs[3]),
            (Some(Held::Addr(name)), Some(Held::Imm(len)), Some(Held::SelfAt(offset)))
               => (name, len, offset)
         )?;
         let bytes = usize::try_from(len)
            .ok()
            .filter(|bytes| (1..64).contains(bytes))?;
         let vtable = regs[4].and_then(pick!(Held::Addr(vtable) => vtable));
         Some((target, name, bytes, offset, vtable))
      })
      .collect::<Vec<_>>();
   let Some(field_fn) = most_common(calls.iter().map(|&(target, ..)| target), 1) else {
      return out;
   };
   for (target, name_at, bytes, offset, vtable) in calls {
      let text = image
         .read(name_at, bytes)
         .and_then(|text| std::str::from_utf8(text).ok());
      let Some(name) = text.filter(|name| target == field_fn && !out.contains_key(*name)) else {
         continue;
      };
      out.insert(name.to_owned(), offset);
      let fmt = vtable.and_then(|vtable| image.read_usize(vtable + 0x18));
      if depth > 0
         && let Some(fmt) = fmt.filter(|&fmt| image.is_function(fmt))
      {
         for (inner, at) in debug_offsets(image, fmt, depth - 1) {
            out.entry(format!("{name}.{inner}")).or_insert(offset + at);
         }
      }
   }
   out
}

/// `<StyleRefinement as Debug>::fmt`'s fields, from the one function that
/// loads the bare name and prints `padding` and `gap`.
pub fn style_fields(image: &Image<'_>) -> Found<HashMap<String, usize>> {
   let mut found = loaders(image, &image.find_bytes(b"StyleRefinement"))
      .into_iter()
      .map(|func| debug_offsets(image, func, 1))
      .filter(|fields| fields.contains_key("padding.top") && fields.contains_key("gap.width"))
      .collect::<Vec<_>>();
   match found.len() {
      1 => Ok(found.remove(0)),
      count => Err(format!("StyleRefinement Debug matched {count} functions")),
   }
}

/// [`crate::desktop::staff`] for arm64.
///
/// `flag_override: Option<u64>` in the store state, read by the staff check
/// `(user.flags | override) & 1` as
/// `ldr wT, [state, #tag]; tbz wT, #0; ldr xV, [state, #tag + 8]; ...; orr;
/// tbnz #0`. Every reader must agree on the offset, counted from before an
/// `add xB, xS, #page` that sets the base.
pub fn staff(image: &Image<'_>) -> Found<crate::StaffField> {
   let mut tags = Vec::new();
   // `ldr wT, [xN, #imm]`, matched raw before decoding.
   let loads_w = code_words(image).filter(|&(_, word)| word & 0xFFC0_0000 == 0xB940_0000);
   for (pc, word) in loads_w {
      let Some((rt, rn, tag)) = pick!(in arm64::decode(word, pc),
         Insn::Load { rt, rn, offset, size: 4 } if rn != 31 && offset >= 0x100 => (rt, rn, offset)
      ) else {
         continue;
      };
      let raw = |delta: isize| image.read_u32(pc.wrapping_add_signed(delta * 4));
      let insn = |delta: isize| decode_at(image, pc.wrapping_add_signed(delta * 4));
      // `Some` is tag 1, tested as `tbz/tbnz wT, #0` straight after.
      if !matches!(insn(1), Some(Insn::Tbz { rt: tested, bit: 0, .. }) if tested == rt) {
         continue;
      }
      let Some((value_at, value)) = (-6..6).find_map(|at| {
         pick!(in insn(at)?,
            Insn::Load { rt: value, rn: base, offset, size: 8 } if base == rn && offset == tag + 8
               => (at, u32::from(value))
         )
      }) else {
         continue;
      };
      // `(flags | override) & 1`: an `orr xD, xN, xM` with the value as an
      // operand, then a bit-0 test or mask, shortly after. `mov` is the same
      // encoding from `xzr`, which is not an OR of two values.
      let ors = |word: u32| {
         let (rn, rm) = ((word >> 5) & 0x1F, (word >> 16) & 0x1F);
         word & 0xFF20_0000 == 0xAA00_0000 && rn != 31 && (rn == value || rm == value)
      };
      let tests_bit0 = |at: isize| {
         raw(at).is_some_and(|test| {
            matches!(insn(at), Some(Insn::Tbz { bit: 0, .. }))
               || test & 0xFFFF_FC00 == 0x9240_0000
               || test & 0xFFFF_FC1F == 0xF240_001F
         })
      };
      let ors_then_tests_bit0 = (value_at + 1..value_at + 16)
         .any(|at| raw(at).is_some_and(ors) && (at + 1..at + 6).any(tests_bit0));
      if !ors_then_tests_bit0 {
         continue;
      }
      // The state is huge, so the base is often `add xB, xS, #page, lsl #12`
      // just before; the offset counts from `xS`.
      let base = (-8..0).rev().find_map(|at| {
         let add = insn(at)?;
         pick!(in add, Insn::AddImm { rd, rn: source, imm } if rd == rn && source != 31 => imm)
      });
      tags.push(base.unwrap_or(0) + tag);
   }
   let found = sorted(tags);
   let [tag] = found[..] else {
      return Err(format!("staff flag reads disagree: {found:#x?}"));
   };
   Ok(crate::StaffField {
      tag,
      value: tag + 8,
   })
}

/// What `phoenix.ui.addMessageAction` needs from `MessageContextMenu` on
/// arm64: the builder, where its input holds the message and channel ids,
/// and the `MenuItem` constructors its items are made with.
#[derive(Debug, Clone, Copy)]
pub struct MessageMenu {
   /// `fn(x0 input, x1 cx, x2..x5, d0) -> Vec<MenuItem>` through `x8`.
   pub build_fn:          usize,
   /// The input field holding `&Message`.
   pub message:           usize,
   pub message_id_offset: usize,
   pub channel_id_offset: usize,
   /// `props_new(x8 out, x0 &SharedString, w1, w2)`.
   pub props_new:         usize,
   /// `with_id(x8 out, x0 item, x1 ptr, x2 len)`.
   pub with_id:           usize,
   /// `with_icon(x8 out, x0 item, w1 icon)`.
   pub with_icon:         usize,
   /// `Vec<MenuItem>::push(x0 vec, x1 item)`.
   pub vec_push:          usize,
   /// `MenuItem::Separator`'s tag, from a push of an item that is only a tag.
   pub separator_tag:     Option<u64>,
   pub layout:            MenuLayout,
}

fn flattened(image: &Image<'_>, func: usize) -> Vec<Insn> {
   flattened_at(image, func)
      .into_iter()
      .map(|(_, insn)| insn)
      .collect()
}

/// `fn`'s instructions with each outlined stub inlined: a `bl` to a short
/// frameless stub becomes the stub's instructions, and its tail `b T` a `bl T`.
/// A stub's instructions carry its call's address.
#[must_use]
pub fn flattened_at(image: &Image<'_>, func: usize) -> Vec<(usize, Insn)> {
   let Some((_, body)) = arm64::function(image, func) else {
      return Vec::new();
   };
   let mut out = Vec::with_capacity(body.len());
   for (pc, insn) in body {
      let Some((prefix, tail)) = calls(&[insn]).next().and_then(|target| stub(image, target))
      else {
         out.push((pc, insn));
         continue;
      };
      out.extend(prefix.into_iter().map(|insn| (pc, insn)));
      out.extend(tail.map(|tail| (pc, Insn::Bl { target: tail })));
   }
   out
}

/// A stub's instructions before its `b T` (with `T`) or `ret` (without).
fn stub(image: &Image<'_>, at: usize) -> Option<(Vec<Insn>, Option<usize>)> {
   const MOST: usize = 6;
   let mut prefix = Vec::new();
   for (_, insn) in arm64::decode_all(image.read(at, MOST * 4)?, at) {
      match insn {
         Insn::B { target } => return Some((prefix, Some(target))),
         Insn::Ret => return Some((prefix, None)),
         Insn::Bl { .. }
         | Insn::BCond { .. }
         | Insn::Cbz { .. }
         | Insn::Tbz { .. }
         | Insn::Br { .. }
         | Insn::Blr { .. }
         | Insn::Writeback { .. } => return None,
         other => prefix.push(other),
      }
   }
   None
}

/// `stp`/`ldp` of 64-bit registers at a signed offset: `(load, rt, rt2, rn,
/// offset)`.
fn pair(insn: Insn) -> Option<(bool, u8, u8, u8, isize)> {
   let Insn::Other(word) = insn else {
      return None;
   };
   let load = match word & 0xFFC0_0000 {
      0xA900_0000 => false,
      0xA940_0000 => true,
      _ => return None,
   };
   let field = |shift: u32| ((word >> shift) & 31) as u8;
   let imm = (((word >> 15) & 0x7F) << 25).cast_signed() >> 25;
   Some((load, field(0), field(10), field(5), imm as isize * 8))
}

/// The most common value occurring at least `least` times.
fn most_common(values: impl IntoIterator<Item = usize>, least: usize) -> Option<usize> {
   let mut counts = HashMap::<usize, usize>::new();
   for value in values {
      *counts.entry(value).or_default() += 1;
   }
   counts
      .into_iter()
      .filter(|&(_, count)| count >= least)
      .max_by_key(|&(value, count)| (count, std::cmp::Reverse(value)))
      .map(|(value, _)| value)
}

/// [`MessageMenu`], from the builder that loads the bare `MessageContextMenu`
/// name.
///
/// Its items are built `i18n label -> MenuItem::new::<F> -> with_id ->
/// [with_icon] -> push`, every step behind outlined stubs, and each
/// `MenuItem::new::<F>` ends in a core that runs `props_new`, boxes the
/// closure in an `Rc`, copies the props in and writes the variant tag.
#[expect(clippy::too_many_lines, reason = "one pass per located part")]
pub fn message_context_menu(image: &Image<'_>) -> Found<MessageMenu> {
   let builders = loaders(image, &image.find_bytes(b"MessageContextMenu"));
   let build_fn = one("MessageContextMenu builder", &builders)?;
   let body = flattened(image, build_fn);

   // The input arrives in `x0` and is kept in a callee-saved register. The
   // builder loads the ids as one pair, the channel's then the message's.
   let head = &body[..body.len().min(48)];
   let input = head
      .iter()
      .find_map(pick!(&Insn::MovReg { rd, rm: 0, wide: true } if rd >= 19 => rd))
      .ok_or("MessageContextMenu input register not found")?;
   let channel_id_offset = head
      .iter()
      .find_map(|&insn| {
         pair(insn).and_then(pick!(
            (true, _, _, rn, offset) if rn == input && (0x20..=0x400).contains(&offset)
               => usize::try_from(offset).ok()?
         ))
      })
      .ok_or("MessageContextMenu id load not found")?;
   // The builder clones the message out of the input first, passing the field
   // holding `&Message` as `add x0, x0, #message`, then a store, then `bl
   // clone`.
   let message = head
      .windows(3)
      .find_map(pick!(
         &[Insn::AddImm { rd: 0, rn, imm }, Insn::Other(_), Insn::Bl { .. }]
            if (rn == 0 || rn == input) && (0x20..0x400).contains(&imm) => imm
      ))
      .ok_or("MessageContextMenu message field not found")?;

   let calls_after = |want: fn(&Insn) -> bool| {
      let calls = body
         .windows(2)
         .filter_map(pick!(&[before, Insn::Bl { target }] if want(&before) => target));
      calls.collect::<Vec<_>>()
   };
   let id_calls = calls_after(|insn| matches!(insn, Insn::MovZ { rd: 2, .. }));
   let with_id = most_common(id_calls, 3).ok_or("MenuItem::with_id not found")?;

   // `with_icon` keeps the icon as a `u16` on its stack: `sturh w1, [..]`.
   let stores_icon = |func: usize| {
      let code = image.read(func, 8 * 4).unwrap_or_default();
      words(code).any(|word| word & 0xFFE0_0C1F == 0x7800_0001)
   };
   let mut icon_calls = calls_after(|insn| matches!(insn, Insn::MovZ { rd: 1, .. }));
   icon_calls.retain(|&target| stores_icon(target));
   let with_icon = most_common(icon_calls, 2).ok_or("MenuItem::with_icon not found")?;

   // `push` strides by the item size: `mov w9, #size; madd x0, len, x9, ptr`.
   let push_stride = |func: usize| {
      flattened(image, func).windows(2).find_map(pick!(
         &[Insn::MovZ { rd: 9, imm, .. }, Insn::Other(word)]
            if word & 0xFFE0_8000 == 0x9B00_0000 && (word >> 16) & 31 == 9
               => usize::try_from(imm).ok()?
      ))
   };
   let pushes = calls(&body).filter(|&target| push_stride(target).is_some());
   let vec_push = most_common(pushes, 3).ok_or("Vec<MenuItem>::push not found")?;
   let size = push_stride(vec_push).ok_or("Vec<MenuItem>::push stride not found")?;

   // Each `MenuItem::new::<F>` reaches its core through
   // `mov x3, x1; mov w1, #0; b core`.
   let mut cores = Vec::new();
   for ctor in sorted(calls(&body).collect::<Vec<_>>()) {
      let code = flattened(image, ctor);
      cores.extend(code.windows(3).filter_map(pick!(
         &[Insn::MovReg { rd: 3, rm: 1, .. }, Insn::MovZ { rd: 1, imm: 0, .. }, Insn::Bl { target }]
            => target
      )));
   }
   let read_core = |core: usize| -> Option<(usize, MenuLayout)> {
      let code = flattened(image, core);
      let props_new = code
         .windows(2)
         .find_map(pick!(&[Insn::AddImm { rd: 8, rn: 31, .. }, Insn::Bl { target }] => target))?;
      let (props, props_size) = code.windows(4).find_map(pick!(
         &[
            Insn::AddImm { rd: 0, rn, imm: props },
            Insn::AddImm { rd: 1, rn: 31, .. },
            Insn::MovZ { rd: 2, imm: props_size, .. },
            Insn::Bl { .. },
         ] if rn != 31 => (props, usize::try_from(props_size).ok()?)
      ))?;
      let mut tagged = code.windows(3).filter_map(pick!(
         &[handler, Insn::MovZ { rd: 8, imm: tag, .. }, variant] => (handler, tag, variant)
      ));
      let (on_click, action_tag) = tagged.find_map(|(handler, tag, variant)| {
         let ((false, _, 8, base, on_click), (false, 8, 31, variant_base, 0)) =
            (pair(handler)?, pair(variant)?)
         else {
            return None;
         };
         (base == variant_base).then_some((usize::try_from(on_click).ok()?, tag))
      })?;
      Some((props_new, MenuLayout {
         size,
         props,
         props_size,
         action_tag,
         on_click,
         on_click_tag: on_click.checked_sub(8)?,
      }))
   };
   let decoded = sorted(cores)
      .into_iter()
      .filter_map(read_core)
      .collect::<Vec<_>>();
   let Some(&(props_new, layout)) = decoded.first() else {
      return Err("no MenuItem::new core decodes".to_owned());
   };
   let key = |at: &MenuLayout| (at.props, at.props_size, at.action_tag, at.on_click);
   let agree = decoded
      .iter()
      .all(|(other, at)| *other == props_new && key(at) == key(&layout));
   if decoded.len() < 2 || !agree {
      return Err(format!("{} MenuItem::new cores disagree", decoded.len()));
   }
   if layout.props + layout.props_size > layout.size
      || layout.on_click + 16 > layout.props
      || layout.size > layout::SCRATCH_CAP
   {
      return Err(format!("MenuItem layout doesn't fit: {layout:x?}"));
   }
   Ok(MessageMenu {
      build_fn,
      message,
      message_id_offset: channel_id_offset + 8,
      channel_id_offset,
      props_new,
      with_id,
      with_icon,
      vec_push,
      separator_tag: separator_tag(image, vec_push, layout.action_tag),
      layout,
   })
}

/// The tag of the payload-less `MenuItem` other menus push. Just before the
/// call, `mov wT, #tag; str xT, [sp, #slot]; ...; add x1, sp, #slot` with
/// nothing else stored in the slot. Every such push must agree.
fn separator_tag(image: &Image<'_>, vec_push: usize, action_tag: u64) -> Option<u64> {
   let tags = refs_to(image, &[vec_push])
      .into_iter()
      .filter_map(|(site, _)| {
         let insns = decoded(image, site.checked_sub(8 * 4)?, 8);
         let slot = insns.iter().rev().find_map(|insn| {
            match *insn {
               Insn::AddImm { rd: 1, rn: 31, imm } => Some(imm),
               _ => None,
            }
         })?;
         let stores: Vec<_> = insns
            .iter()
            .filter_map(|insn| {
               match *insn {
                  Insn::Store {
                     rt, rn: 31, offset, ..
                  } if (slot..slot + 0x200).contains(&offset) => Some((rt, offset)),
                  _ => None,
               }
            })
            .collect();
         let &[(rt, offset)] = stores.as_slice() else {
            return None;
         };
         if offset != slot {
            return None;
         }
         insns
            .iter()
            .rev()
            .find_map(|insn| {
               match *insn {
                  Insn::MovZ { rd, imm, .. } if rd == rt => Some(imm),
                  _ => None,
               }
            })
            .filter(|&tag| tag < 0x10 && tag != action_tag)
      });
   match sorted(tags.collect()).as_slice() {
      &[tag] => Some(tag),
      _ => None,
   }
}

/// An address loaded by an `adrp`/`add` pair, at each instruction that
/// completes one, in a [`flattened_at`] stream.
fn loaded(insns: &[(usize, Insn)]) -> Vec<Option<(u8, usize)>> {
   let mut pages = [None::<usize>; 32];
   let completes = |&(_, insn): &(usize, Insn)| {
      if let Insn::Adrp { rd, page } = insn {
         pages[usize::from(rd)] = Some(page);
      }
      pick!(in insn,
         Insn::AddImm { rd, rn, imm } if rn != 31 => (rd, pages[usize::from(rn)]? + imm)
      )
   };
   insns.iter().map(completes).collect()
}

/// `<Message as Serialize>::serialize` into a `serde_json` writer, and the
/// allocator's free for the bytes it writes.
#[derive(Debug, Clone, Copy)]
pub struct MessageWriter {
   /// `fn(x0 &Message, x1 &mut Serializer<&mut Vec<u8>>) -> x0
   /// Option<Box<Error>>`.
   pub serialize:  usize,
   /// `__rust_dealloc(x0 ptr, x1 size, x2 align)`.
   pub dealloc:    usize,
   /// The message's id, the first field it writes.
   pub message_id: usize,
}

/// [`MessageWriter`].
///
/// The serializer is the one function loading `"mention_everyone"` whose
/// first strings are `{` and the `id` key, with `channel_id` and `guild_id`
/// keys and no `kind`. It moves its base once (`ldr xT, [xB, #w]!`)
/// and passes `&message.id` as `add x3, xB, #id`. The free is the call
/// `String` drops end in, `mov w2, #1; b dealloc`.
pub fn message_writer(image: &Image<'_>) -> Found<MessageWriter> {
   let found = loaders(image, &image.find_bytes(b"mention_everyone"))
      .into_iter()
      .filter_map(|func| {
         let code = flattened_at(image, func);
         let loads = loaded(&code);
         let strings = loads
            .iter()
            .flatten()
            .filter_map(|&(_, addr)| image.read(addr, 16))
            .collect::<Vec<_>>();
         let [open, id, ..] = strings.as_slice() else {
            return None;
         };
         let has = |key: &[u8]| strings.iter().any(|text| text.starts_with(key));
         if !(open.starts_with(b"{")
            && id.starts_with(b"id")
            && has(b"channel_id")
            && has(b"guild_id")
            && !has(b"kind"))
         {
            return None;
         }
         let (moved, base) = code.iter().take(16).find_map(pick!(
            &(_, Insn::Writeback { rn, imm, load: true, .. }) => (usize::try_from(imm).ok()?, rn)
         ))?;
         let (id_at, _) = loads
            .iter()
            .enumerate()
            .filter(|(_, load)| load.is_some())
            .nth(1)?;
         let field = code[id_at..]
            .iter()
            .take(6)
            .find_map(pick!(&(_, Insn::AddImm { rd: 3, rn, imm }) if rn == base => imm))?;
         Some((func, moved + field))
      })
      .collect::<Vec<_>>();
   let &[(serialize, message_id)] = found.as_slice() else {
      return Err(format!("{} Message serializers", found.len()));
   };

   let mut tails = HashMap::<usize, usize>::new();
   // `mov w2, #1` or `mov x2, #1`.
   for (pc, _) in code_words(image).filter(|&(_, word)| word & 0x7FFF_FFFF == 0x5280_0022) {
      if let Some(Insn::B { target }) = decode_at(image, pc + 4) {
         *tails.entry(target).or_default() += 1;
      }
   }
   let mut ranked = tails.into_iter().collect::<Vec<_>>();
   ranked.sort_unstable_by_key(|&(_, count)| std::cmp::Reverse(count));
   // 613 against 60 for the runner-up on 2233.
   let dealloc = match ranked[..] {
      [(dealloc, most), (_, next), ..] if most >= next * 4 => dealloc,
      _ => return Err("__rust_dealloc not found".to_owned()),
   };
   Ok(MessageWriter {
      serialize,
      dealloc,
      message_id,
   })
}

/// A call that returns a value through a stack slot: `x8` was pointed at the
/// stack since the call before.
#[derive(Clone, Copy)]
struct Built {
   at:     usize,
   target: usize,
}

/// The [`Built`] calls of a [`flattened_at`] stream, besides the i18n lookups,
/// which load a 64-bit key hash into `x2` first.
fn built(insns: &[(usize, Insn)]) -> Vec<Built> {
   let mut out = Vec::new();
   let (mut sret, mut hashed) = (false, false);
   for (index, &(_, insn)) in insns.iter().enumerate() {
      match insn {
         Insn::AddImm { rd: 8, rn: 31, .. } => sret = true,
         Insn::MovZ {
            rd: 2, wide: true, ..
         } => hashed = true,
         Insn::Bl { target } => {
            if sret && !hashed {
               out.push(Built { at: index, target });
            }
            (sret, hashed) = (false, false);
         },
         _ => {},
      }
   }
   out
}

/// The function holding the one code reference to the exact bytes `name`,
/// flattened, and the index the reference completes at.
fn anchored(image: &Image<'_>, name: &[u8], what: &str) -> Found<(Vec<(usize, Insn)>, usize)> {
   let refs = refs_to(image, &image.find_bytes(name)).into_iter();
   let sites = sorted(
      refs
         .filter_map(|(site, target)| Some((image.function_containing(site)?, target)))
         .collect(),
   );
   let &[(func, target)] = sites.as_slice() else {
      return Err(format!("{what}: {} references", sites.len()));
   };
   let insns = flattened_at(image, func);
   let index = loaded(&insns)
      .iter()
      .position(|load| load.is_some_and(|(_, addr)| addr == target))
      .ok_or_else(|| format!("{what} load not found"))?;
   Ok((insns, index))
}

fn calls(code: &[Insn]) -> impl Iterator<Item = usize> + '_ {
   code
      .iter()
      .filter_map(pick!(&Insn::Bl { target } => target))
}

/// [`Gpui`] for arm64, from the same two call sites as x86.
///
/// They are the Developer Mode switch row of `host::section`, and the
/// `TesterUpdates` sign-in button and the `ActionRow` it goes into. The
/// order of calls holds once i18n lookups, copies and getters, the calls that
/// return nothing through a stack slot, are left out.
pub fn gpui(image: &Image<'_>) -> Found<Gpui> {
   let (dev, anchor) = anchored(image, b"set-devmodeDeveloper Mode", "set-devmode")?;
   let calls_built = built(&dev);
   let (before, after): (Vec<&Built>, Vec<&Built>) = calls_built
      .iter()
      .filter(|call| call.at != anchor)
      .partition(|call| call.at < anchor);
   let [.., new_call, flex, flex_col] = before[..] else {
      return Err("Developer Mode row has no Div before it".to_owned());
   };
   let caller_loc = loaded(&dev)[..new_call.at]
      .iter()
      .rev()
      .take(8)
      .find_map(|load| load.and_then(pick!((0, addr) => addr)))
      .ok_or("div_new Location not found")?;
   let [switch, label, desc, on_change, into_div, ..] = after[..] else {
      return Err(format!("{} Developer Mode row calls", after.len()));
   };
   let alloc_or_oom = allocs(&flattened(image, on_change.target))
      .next()
      .map(|(.., target)| target)
      .ok_or("alloc_or_oom not found in Switch::on_change")?;

   let (tester, anchor) = anchored(image, b"tester-updates-sign-in", "tester-updates-sign-in")?;
   let mut after_btn = built(&tester);
   after_btn.retain(|call| call.at > anchor);
   let button_new = after_btn.first().ok_or("Button::new not found")?.target;
   // `ActionRow` opens with `div_new` and `flex`.
   let opens_row = |call: &Built| {
      let inner = built(&flattened_at(image, call.target));
      let mut head = inner.iter().take(4);
      inner.first().map(|first| first.target) == Some(new_call.target)
         && head.any(|early| early.target == flex.target)
   };
   let row_at = after_btn
      .iter()
      .position(opens_row)
      .filter(|&at| at >= 1)
      .ok_or("ActionRow no longer calls div_new and flex first")?;
   let action_row = after_btn[row_at].target;
   let button_on_click = after_btn[row_at - 1].target;
   let div_into_any = after_btn
      .get(row_at + 1)
      .ok_or("Div::into_any_element not found after ActionRow")?
      .target;

   let row = flattened_at(image, action_row);
   let row_built = built(&row);
   let second = row_built[1..]
      .iter()
      .position(|call| call.target == new_call.target)
      .ok_or("ActionRow builds one Div")?
      + 1;
   if second < 4 {
      return Err("ActionRow's text column moved".to_owned());
   }
   let div_child_div = row_built[second - 1].target;
   let text_block = row_built[second - 4].target;
   // Copies (`mov w2, #size; bl memcpy`) aside, the child added before the
   // last `div_child_div` is the button.
   let row_calls = row
      .windows(2)
      .filter_map(pick!(
         &[(_, before), (_, Insn::Bl { target })]
            if !matches!(before, Insn::MovZ { rd: 2, wide: false, .. }) => target
      ))
      .collect::<Vec<_>>();
   let last_child = row_calls
      .iter()
      .rposition(|&target| target == div_child_div)
      .filter(|&at| at >= 1)
      .ok_or("ActionRow no longer adds its children with div_child_div")?;
   let styled = row_built[..second]
      .iter()
      .filter(|call| call.target != new_call.target);
   let firsts = styled.filter_map(|call| calls(&flattened(image, call.target)).next());
   let div_style_mut = most_common(firsts, 3).ok_or("div_style_mut not found")?;

   Ok(Gpui {
      caller_loc,
      div_new: new_call.target,
      div_flex: flex.target,
      div_flex_col: flex_col.target,
      div_style_mut,
      div_child_div,
      div_child_button: row_calls[last_child - 1],
      div_into_any,
      div_push_any: children_push(&flattened(image, div_child_div)).map(|(_, push)| push),
      str_into_any: shared_string_into_any(image, div_child_div),
      text_block,
      switch_new: switch.target,
      switch_into_div: into_div.target,
      button_new,
      action_row,
      alloc_or_oom,
      switch_label: label.target,
      switch_desc: desc.target,
      switch_on_change: on_change.target,
      button_on_click,
   })
}

/// `<SharedString as IntoElement>::into_any_element`, which `Div::child` for
/// a `SharedString` (a neighbour of `div_child_div`) calls. It drops the
/// string it moved: `cmp w8, #0xff` (`None`), then `cmp w8, #0x19` (`Arc`).
fn shared_string_into_any(image: &Image<'_>, div_child_div: usize) -> Option<usize> {
   let drops_string = |func: usize| {
      flattened(image, func).windows(3).any(|window| {
         matches!(window, [Insn::CmpImm { imm: 0xFF, .. }, _, Insn::CmpImm {
            imm: 0x19,
            ..
         }])
      })
   };
   let mut at = image.function_bounds(div_child_div)?.1;
   for _ in 0..6 {
      let (start, end) = image.function_bounds(at)?;
      let found = calls(&flattened(image, start)).find(|&target| drops_string(target));
      if found.is_some() {
         return found;
      }
      at = end;
   }
   None
}

/// `div_child_div`'s push of the child onto `self.children`: `add x0, xS,
/// #children`, the element into `x1`, then the call. The offset and `push`.
fn children_push(code: &[Insn]) -> Option<(usize, usize)> {
   code.windows(3).find_map(pick!(
      &[Insn::AddImm { rd: 0, rn, imm }, Insn::Bl { target }, ..]
         | &[Insn::AddImm { rd: 0, rn, imm }, _, Insn::Bl { target }]
         if rn != 31 => (imm, target)
   ))
}

/// The first register `code` copies `source` into.
fn copy_of(code: &[Insn], source: u8) -> Option<u8> {
   code
      .iter()
      .find_map(pick!(&Insn::MovReg { rd, rm, .. } if rm == source => rd))
}

/// `(after, x0, x1, target)` for each `mov w0, #x0; mov w1, #x1; bl target`.
fn allocs(code: &[Insn]) -> impl Iterator<Item = (usize, u64, u64, usize)> + '_ {
   code.windows(3).enumerate().filter_map(|(at, window)| {
      pick!(in *window,
         [Insn::MovZ { rd: 0, imm: x0, .. }, Insn::MovZ { rd: 1, imm: x1, .. }, Insn::Bl { target }]
            => (at + 3, x0, x1, target)
      )
   })
}

/// The `Rc<dyn Fn>` a handler setter stores, and the size of what it returns:
/// it allocates the `Rc` (`mov w0, #8; mov w1, #size; bl alloc`), stores it
/// into its copy of `self`, and copies that out (`mov w2, #size; b memcpy`).
fn handler_store(image: &Image<'_>, setter: usize) -> Option<(usize, usize)> {
   let code = flattened(image, setter);
   let (alloc, ..) = allocs(&code).find(|&(_, size, ..)| size == 8)?;
   let tail = &code[alloc..];
   let boxed = copy_of(tail, 0)?;
   let offset = tail
      .iter()
      .find_map(pick!(&Insn::Store { rt, offset, size: 8, .. } if rt == boxed => offset))?;
   // An epilogue `ldp` may sit between the size and the tail call.
   let size = tail.windows(3).find_map(pick!(
      &[Insn::MovZ { rd: 2, imm, .. }, Insn::B { .. } | Insn::Bl { .. }, _]
         | &[Insn::MovZ { rd: 2, imm, .. }, _, Insn::B { .. } | Insn::Bl { .. }] => imm
   ))?;
   Some((offset, usize::try_from(size).ok()?))
}

/// Where a `Switch` setter keeps its `Option<SharedString>`: it reads the tag
/// with a writeback load off `self` (`ldrb w8, [x23, #off]!`) and checks it for
/// `None` (`cmp w8, #0xff`). Some monomorphizations wrap the setter of the
/// `Switch`'s leading field struct, which keeps the same offsets, so a setter
/// without the load is read through its first call.
fn option_string_field(image: &Image<'_>, setter: usize) -> Option<usize> {
   let code = flattened(image, setter);
   let tagged_load = |code: &[Insn]| {
      let mut selves = vec![0u8];
      for (index, &insn) in code.iter().enumerate() {
         selves.extend(pick!(in insn, Insn::MovReg { rd, rm, .. } if selves.contains(&rm) => rd));
         let Some((rt, imm)) = pick!(in insn,
            Insn::Writeback { rt, rn, imm, load: true } if selves.contains(&rn) && imm > 0
               => (rt, imm)
         ) else {
            continue;
         };
         let mut following = code[index + 1..].iter().take(2);
         if following.any(|next| matches!(*next, Insn::CmpImm { rn, imm: 0xff, .. } if rn == rt)) {
            return Some(imm.unsigned_abs());
         }
      }
      None
   };
   tagged_load(&code)
      .or_else(|| calls(&code).find_map(|target| tagged_load(&flattened(image, target))))
}

/// [`GpuiLayout`] for arm64, from the builders [`gpui`] found.
///
/// The `Button`'s variant and size fields are left past its end, so the
/// runtime never writes them and buttons keep Discord's default look.
pub fn gpui_layouts(image: &Image<'_>, gpui: &Gpui) -> Found<GpuiLayout> {
   // `div_child_div` pushes the child at `self + children`, then copies `self`
   // out through its `x8` copy field by field; the last store is the end.
   let child = flattened(image, gpui.div_child_div);
   let (children, _) = children_push(&child).ok_or("Div children SmallVec offset not found")?;
   let out = copy_of(&child, 8).ok_or("div_child_div result register not found")?;
   let div_size = child
      .iter()
      .filter_map(pick!(
         &Insn::Store { rn, offset, size, .. } if rn == out => offset + size,
         &Insn::Other(word)
            if word & 0xFFC0_0000 == 0xAD00_0000 && (word >> 5) & 31 == u32::from(out) => {
            let imm = (((word >> 15) & 0x7F) << 25).cast_signed() >> 25;
            usize::try_from(imm * 16 + 32).ok()?
         },
      ))
      .max()
      .ok_or("Div size not found")?;

   // `div_new` boxes `Interactivity` (`mov w0, #size; mov w1, #8; bl`) and
   // stores the box in its result with an `stp`.
   let new = flattened(image, gpui.div_new);
   let (interactivity_size, boxed_at) = allocs(&new)
      .find_map(pick!(
         (after, size, 8, _) if (0x100..0x2000).contains(&size)
            => (usize::try_from(size).ok()?, after)
      ))
      .ok_or("Div::new Interactivity allocation not found")?;
   let boxed = copy_of(&new[boxed_at..], 0).ok_or("Div::new Interactivity register not found")?;
   let interactivity = new[boxed_at..]
      .iter()
      .find_map(|&insn| {
         pair(insn).and_then(pick!(
            (false, rt, _, _, offset) if rt == boxed => usize::try_from(offset).ok()?,
            (false, _, rt2, _, offset) if rt2 == boxed => usize::try_from(offset + 8).ok()?,
         ))
      })
      .ok_or("Div::new Interactivity store not found")?;
   if children + 0x48 > div_size || interactivity + 8 > div_size || div_size > layout::DIV_CAP {
      return Err(format!("Div fields don't fit (size {div_size:#x})"));
   }

   // `Interactivity::default` boxes the `StyleRefinement`.
   let style_bytes = calls(&new)
      .find_map(|callee| {
         let mut head = flattened(image, callee);
         head.truncate(40);
         allocs(&head).find_map(pick!(
            (_, size, 8, _) if (0x200..=0x400).contains(&size) => usize::try_from(size).ok()?
         ))
      })
      .ok_or("Interactivity::default StyleRefinement allocation not found")?;
   let style = crate::desktop::style_from_fields(&style_fields(image)?, style_bytes)?;

   let (on_change, switch_size) =
      handler_store(image, gpui.switch_on_change).ok_or("Switch::on_change Rc store not found")?;
   let label = option_string_field(image, gpui.switch_label).ok_or("Switch::label not found")?;
   let description =
      option_string_field(image, gpui.switch_desc).ok_or("Switch::description not found")?;
   if label.max(description) + 24 > switch_size
      || on_change + 16 > switch_size
      || switch_size > layout::SCRATCH_CAP
   {
      return Err("Switch fields sit past the Switch".to_owned());
   }

   let (on_click, button_size) =
      handler_store(image, gpui.button_on_click).ok_or("Button::on_click Rc store not found")?;
   if on_click + 16 > button_size || button_size > layout::SCRATCH_CAP {
      return Err("Button on_click sits past the Button".to_owned());
   }

   Ok(GpuiLayout {
      style,
      div: DivLayout {
         size: div_size,
         children,
         interactivity,
         interactivity_size,
      },
      switch: SwitchLayout {
         size: switch_size,
         label,
         description,
         on_change,
      },
      button: ButtonLayout {
         size: button_size,
         on_click,
         variant: layout::SCRATCH_CAP,
         scale: layout::SCRATCH_CAP,
      },
   })
}

/// `build_message_body` on arm64, which `MessageRow::render` calls as
/// `fn(x0 view, x1 config, x2..x5) -> x0`.
#[derive(Debug, Clone)]
pub struct MessageBody {
   pub func:       usize,
   pub message_id: usize,
   /// Every `u32` color in the config the body picks its text colors from.
   pub colors:     Vec<usize>,
}

/// [`MessageBody`], the one function the row render calls that loads the
/// `message-edited-` template.
///
/// Its head picks one of four color triples by the message's state, `mov w8,
/// #c; mov w9, #b; mov w10, #a` each, read as `ldr wN, [config, xN]`, and
/// loads the id as the first word off `x0`.
pub fn message_body(image: &Image<'_>, render_row: usize) -> Found<MessageBody> {
   let mut found = loaders(image, &image.find_bytes(b"\x0fmessage-edited-"));
   found.retain(|&func| loaders(image, &[func]).contains(&render_row));
   let func = one("message body builder", &found)?;
   let mut head = flattened(image, func);
   head.truncate(0x60);
   let message_id = head
      .iter()
      .find_map(pick!(
         &Insn::Load { rn: 0, offset, size: 8, .. } if (0x100..0x800).contains(&offset) => offset
      ))
      .ok_or("message body id load not found")?;
   let offsets = head
      .windows(3)
      .filter_map(pick!(
         &[
            Insn::MovZ { rd: 8, imm: c, .. },
            Insn::MovZ { rd: 9, imm: b, .. },
            Insn::MovZ { rd: 10, imm: a, .. },
         ] => [a, b, c]
      ))
      .flatten()
      .map(|imm| usize::try_from(imm).map_err(|err| err.to_string()))
      .collect::<Found<Vec<_>>>()?;
   let colors = sorted(offsets);
   if colors.len() < 6 || colors.iter().any(|offset| !(0x100..0x400).contains(offset)) {
      return Err(format!("message body colors don't decode: {colors:x?}"));
   }
   Ok(MessageBody {
      func,
      message_id,
      colors,
   })
}

/// `MessageRow::render` on arm64, and the path from its row to the message id.
#[derive(Debug, Clone, Copy)]
pub struct MessageRow {
   /// `fn(x0 row, x1..x7, d0, one stack word) -> AnyElement` through `x8`.
   pub func:       usize,
   /// `row + rc` holds the message's `Rc`.
   pub rc:         usize,
   /// The message inside the `Rc` allocation.
   pub value:      usize,
   pub message_id: usize,
}

/// [`MessageRow`], the one caller of both `message-content-` helpers.
///
/// The helper takes the message in `x0`, and formats `&message.id` (`ldr x8,
/// [sp, #msg]; add x8, x8, #id`). The render passes `ldr x9, [sp, #slot]; add
/// x0, x9, #value`, with the slot filled from `ldr x8, [row, #rc]`.
pub fn message_row(image: &Image<'_>) -> Found<MessageRow> {
   // A format piece: its length byte, then the text.
   let mut refs = refs_to(image, &image.find_bytes(b"\x10message-content-"))
      .into_iter()
      .filter_map(|(site, target)| Some((image.function_containing(site)?, site, target)))
      .collect::<Vec<_>>();
   refs.sort_unstable();
   refs.dedup_by_key(|&mut (func, ..)| func);
   if refs.len() < 2 {
      return Err(format!("{} message-content template loaders", refs.len()));
   }
   let mut common = loaders(image, &[refs[0].0]);
   for &(helper, ..) in &refs[1..] {
      let parents = loaders(image, &[helper]);
      common.retain(|func| parents.contains(func));
   }
   let func = one("MessageRow::render", &common)?;

   // A helper whose template load is preceded by `add x8, xM, #id`, and
   // whose call in the render builds `x0` from a stack slot.
   let render = flattened_at(image, func);
   let decodes = |&(helper, site, _): &(usize, usize, usize)| {
      let insns = flattened_at(image, helper);
      let at = insns.iter().position(|&(pc, _)| pc >= site)?;
      let id = insns[at.saturating_sub(40)..at].iter().rev().find_map(pick!(
         &(_, Insn::AddImm { rd: 8, rn, imm }) if rn != 31 && (0x100..=0x800).contains(&imm) => imm
      ))?;
      let call = render
         .iter()
         .position(|&(_, insn)| insn == Insn::Bl { target: helper })?;
      let (slot, value) = render[call.saturating_sub(24)..call]
         .windows(2)
         .rev()
         .find_map(pick!(
            &[
               (_, Insn::Load { rt, rn: 31, offset: slot, size: 8 }),
               (_, Insn::AddImm { rd: 0, rn, imm }),
            ] if rn == rt && imm < 0x40 => (slot, imm)
         ))?;
      Some((id, slot, value))
   };
   let (message_id, slot, value) = refs
      .iter()
      .find_map(decodes)
      .ok_or("no message-content helper's message argument decodes")?;
   let rc = render
      .windows(2)
      .find_map(pick!(
         &[
            (_, Insn::Load { rt, offset: rc, size: 8, rn }),
            (_, Insn::Store { rt: stored, rn: 31, offset, size: 8 }),
         ] if stored == rt && offset == slot && rn != 31 => rc
      ))
      .ok_or("the row's message Rc not found")?;
   Ok(MessageRow {
      func,
      rc,
      value,
      message_id,
   })
}

/// The body of `<ClientAccessGate as ClientSelector>::read` after its
/// prologue: `mov x19, x2; mov x20, x0; bl enforced; cbz w0, open; ldr x1,
/// [x19, #ready]; add x8, sp, #8; mov x0, x20; mov w2, #1; bl access_at; ldrb
/// w20, [sp, #8]`, then the drop and `open: mov w20, #2; mov x0, x20`.
///
/// Read on build 2233. The `ldr`'s field offset and the calls are wildcards.
const ACCESS_GATE: Pattern = Pattern::new(
   "F3 03 02 AA F4 03 00 AA ?? ?? ?? ?? ?? ?? 00 34 61 ?? ?? F9 E8 23 00 91 E0 03 14 AA 22 00 80 \
    52 ?? ?? ?? ?? F4 23 40 39 E8 13 40 B9 ?? ?? ?? 36 E1 0F 40 F9 ?? ?? ?? ?? ?? ?? ?? 14 54 00 \
    80 52 E0 03 14 AA",
);

/// The prologue's four instructions before [`ACCESS_GATE`].
const PROLOGUE_LEN: usize = 16;

/// `<ClientAccessGate as ClientSelector>::read`, `u8 read(..)` where 2 is
/// "no gate", the value it answers when access is not enforced.
pub fn access_read(image: &Image<'_>) -> Found<usize> {
   let body = one("access read", &image.scan(ACCESS_GATE))?;
   let entry = body
      .checked_sub(PROLOGUE_LEN)
      .ok_or("access gate: the body has no prologue")?;
   // `sub sp, sp, #imm` opens the frame.
   match image.read(entry, 4) {
      Some(&[a, b, c, d]) if u32::from_le_bytes([a, b, c, d]) & 0xFFC0_03FF == 0xD100_03FF => {
         Ok(entry)
      },
      _ => Err(format!("access gate at {entry:#x} does not open a frame")),
   }
}

/// `client_access::enforced` after its frame: `ldr x8, [x0]; add x0, x8,
/// #state; bl; ldrb w8, [x0, #flag]; mov w9, #1; bic w0, w9, w8`, then the
/// frame's `ldp` and `ret`.
///
/// Read on build 2233. The two offsets and the call are wildcards.
const ENFORCED: Pattern = Pattern::new(
   "FD 7B BF A9 FD 03 00 91 08 00 40 F9 00 ?? ?? 91 ?? ?? ?? ?? 08 ?? ?? 39 29 00 80 52 20 01 28 \
    0A FD 7B C1 A8 C0 03 5F D6",
);

/// [`ENFORCED`]'s length in bytes.
const ENFORCED_LEN: usize = 40;

/// Offset of `bl enforced` in [`ACCESS_GATE`].
const ENFORCED_CALL: usize = 8;

/// `client_access::enforced`, `bool enforced(..)`, the first call of the
/// [`access_read`] function. `push_access` reads it too, without
/// going through `read`, and drops every message alert while it is true.
pub fn access_enforced(image: &Image<'_>, read: usize) -> Found<usize> {
   let call = read + PROLOGUE_LEN + ENFORCED_CALL;
   let enforced = image
      .read_u32(call)
      .filter(|bl| bl & 0xFC00_0000 == 0x9400_0000)
      .map(|bl| {
         // imm26 counts instructions, sign-extended.
         let words = ((bl << 6).cast_signed() >> 6) as isize;
         call.wrapping_add_signed(words * 4)
      })
      .ok_or_else(|| format!("access gate at {read:#x} does not call enforced"))?;
   match image.read(enforced, ENFORCED_LEN) {
      Some(body) if ENFORCED.find_in(body) == [0] => Ok(enforced),
      _ => Err(format!("enforced at {enforced:#x} has changed")),
   }
}
