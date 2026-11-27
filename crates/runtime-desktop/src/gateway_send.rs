// hooks libdiscord_gateway's SocketSink::start_send vtable slot for phoenix.gateway.onSend

use std::{
   ffi::c_void,
   panic::{self, AssertUnwindSafe},
   ptr, slice, str,
   sync::OnceLock,
};

use phoenix_hook::image::Image;
use phoenix_runtime::gateway::{SendAction, observe_send};
use phoenix_sigs::gateway_send::{DesktopGatewaySend, WS_MESSAGE_TEXT_TAG};

use crate::discord::{HostAllocFn, HostDeallocFn, RawVec, host_allocator};

const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

#[repr(C)]
struct RawWsMessage {
   tag: u64,
   payload: RawVec,
}

// sret in arg 0, self in arg 1, indirect WsMessage* in arg 2 on both Win64 and SysV
type StartSendFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut RawWsMessage) -> *mut c_void;

struct Installed {
   start_send: StartSendFn,
   ok_tag: u64,
   host_alloc: Option<HostAllocFn>,
   host_dealloc: Option<HostDeallocFn>,
}

static INSTALLED: OnceLock<Installed> = OnceLock::new();

pub fn install(image: &Image<'static>) {
   let found = match DesktopGatewaySend::locate(image) {
      Ok(found) => found,
      Err(err) => {
         log::warn!("gateway send: {err}, phoenix.gateway.onSend inactive");
         return;
      },
   };

   let (host_alloc, host_dealloc) = host_allocator();
   // SAFETY: start_send_fn is the verified SocketSink::start_send entry from the binary's vtable.
   let start_send = unsafe { std::mem::transmute::<usize, StartSendFn>(found.start_send_fn) };
   let _ = INSTALLED.set(Installed {
      start_send,
      ok_tag: found.ok_tag,
      host_alloc,
      host_dealloc,
   });

   let writes = found
      .start_send_slots
      .iter()
      .map(|&slot| (slot, start_send_detour as *const () as usize))
      .collect::<Vec<_>>();
   // SAFETY: each slot is a verified .rdata vtable slot holding start_send_fn.
   match unsafe { image.swap_words(&writes) } {
      Ok(_) => log::info!(
         "gateway send: hooked {} SocketSink::start_send slots",
         found.start_send_slots.len(),
      ),
      Err(err) => log::error!("gateway send: failed to swap vtable slots: {err}"),
   }
}

unsafe extern "C" fn start_send_detour(
   sret: *mut c_void,
   this: *mut c_void,
   item: *mut RawWsMessage,
) -> *mut c_void {
   let installed = INSTALLED
      .get()
      .expect("start_send_detour only runs after INSTALLED is set");

   let mut dropped = false;
   if !item.is_null()
      && panic::catch_unwind(AssertUnwindSafe(|| {
         // SAFETY: item is an owned WsMessage passed indirectly to start_send.
         let msg = unsafe { &mut *item };
         if msg.tag != WS_MESSAGE_TEXT_TAG
            || msg.payload.ptr.is_null()
            || msg.payload.len == 0
            || msg.payload.len > msg.payload.cap
            || msg.payload.len > MAX_FRAME_LEN
         {
            return;
         }
         // SAFETY: msg.payload is a valid host String for msg.payload.len bytes.
         let bytes = unsafe { slice::from_raw_parts(msg.payload.ptr, msg.payload.len) };
         let Ok(text) = str::from_utf8(bytes) else {
            return;
         };
         if !text.trim_start().starts_with('{') {
            return;
         }
         match observe_send(text) {
            SendAction::Pass => {},
            SendAction::Drop => {
               // SAFETY: msg.payload is owned by start_send and will not be forwarded.
               unsafe { free_host_vec(&mut msg.payload, installed) };
               dropped = true;
            },
            SendAction::Replace(new_json) => {
               // SAFETY: msg.payload is an owned host String.
               unsafe { replace_host_string(&mut msg.payload, new_json.as_bytes(), installed) };
            },
         }
      }))
      .is_err()
   {
      log::error!("gateway send: start_send_detour panicked");
   }

   if dropped {
      if !sret.is_null() {
         // SAFETY: sret points to caller's Result<(), WsError>; writing ok_tag constructs Ok(()).
         unsafe { sret.cast::<u64>().write(installed.ok_tag) };
      }
      return sret;
   }

   // SAFETY: forwards to original SocketSink::start_send with the original or rewritten message.
   unsafe { (installed.start_send)(sret, this, item) }
}

unsafe fn free_host_vec(vec: &mut RawVec, installed: &Installed) {
   if vec.cap > 0
      && !vec.ptr.is_null()
      && let Some(dealloc) = installed.host_dealloc
   {
      // SAFETY: buffer came from host's global allocator with align 1.
      unsafe { dealloc(vec.ptr, vec.cap, 1) };
   }
   vec.cap = 0;
   vec.ptr = ptr::null_mut();
   vec.len = 0;
}

unsafe fn replace_host_string(vec: &mut RawVec, new_bytes: &[u8], installed: &Installed) {
   if new_bytes.len() <= vec.cap && !vec.ptr.is_null() {
      // SAFETY: existing host allocation has enough capacity.
      unsafe { ptr::copy_nonoverlapping(new_bytes.as_ptr(), vec.ptr, new_bytes.len()) };
      vec.len = new_bytes.len();
      return;
   }
   let (Some(alloc), Some(dealloc)) = (installed.host_alloc, installed.host_dealloc) else {
      log::warn!("gateway.onSend: cannot grow outgoing payload without host allocator");
      return;
   };
   let new_len = new_bytes.len();
   // SAFETY: host __rust_alloc(new_len, 1).
   let new_ptr = unsafe { alloc(new_len, 1) };
   if new_ptr.is_null() {
      return;
   }
   // SAFETY: freshly allocated buffer of new_len bytes.
   unsafe { ptr::copy_nonoverlapping(new_bytes.as_ptr(), new_ptr, new_len) };
   if vec.cap > 0 && !vec.ptr.is_null() {
      // SAFETY: frees old host buffer with matching align 1.
      unsafe { dealloc(vec.ptr, vec.cap, 1) };
   }
   vec.cap = new_len;
   vec.ptr = new_ptr;
   vec.len = new_len;
}
