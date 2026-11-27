//! Phoenix's Staff Settings page reads a local flag computed as
//! `(user.flags | flag_override) & 1`, where `flag_override` is Phoenix's own
//! client-side field. Setting its low bit shows the page.
//!
//! On the 64-bit targets the read is inlined in about 30 places off the gateway
//! handler's first argument, so the gateway detour writes `flag_override` in
//! that state while the reducer holds it. On armeabi-v7a the read is outlined
//! into one check function, which is hooked to answer staff while the unlock is
//! on.

use std::ffi::c_void;

use phoenix_hook::image::Image;

pub fn start(image: &'static Image<'static>) {
   imp::start(image);
}

/// Sets the staff bit in `store` on the targets that read it inline.
///
/// # Safety
///
/// `store` is the store state a gateway handler was called with, and the
/// caller runs inside that handler.
#[cfg_attr(
   target_arch = "arm",
   allow(clippy::missing_const_for_fn, reason = "only the arm32 one is a no-op")
)]
pub unsafe fn apply(store: *mut c_void) {
   // SAFETY: forwarded from the caller.
   unsafe { imp::apply(store) };
}

#[cfg(target_arch = "aarch64")]
mod imp {
   use std::{
      ffi::c_void,
      ptr,
      sync::OnceLock,
   };

   use phoenix_hook::image::Image;
   use phoenix_sigs::StaffField as Field;

   const STAFF: u32 = 1;

   static LOCATED: OnceLock<Option<Field>> = OnceLock::new();

   /// Finds the `flag_override` field off the main thread, since the scan
   /// reads most of the host library.
   pub fn start(image: &'static Image<'static>) {
      std::thread::spawn(|| {
         LOCATED.get_or_init(|| locate(image));
      });
   }

   fn locate(image: &Image<'_>) -> Option<Field> {
      phoenix_sigs::mobile::staff_aarch64(image)
         .inspect_err(|err| log::warn!("staff: {err}, leaving flag_override as is"))
         .ok()
   }

   /// # Safety
   ///
   /// `state` is the store state a gateway handler was called with, and the
   /// caller runs inside that handler.
   pub unsafe fn apply(state: *mut c_void) {
      let Some(Some(field)) = LOCATED.get() else {
         return;
      };
      // SAFETY: the flag read loads both fields from this state as u32s.
      unsafe {
         let tag = state.byte_add(field.tag).cast::<u32>();
         let value = state.byte_add(field.value).cast::<u32>();
         let had = ptr::read(tag) != 0;
         let current = if had { ptr::read(value) } else { 0 };
         let wanted = if phoenix_runtime::staff_enabled() {
            current | STAFF
         } else {
            current & !STAFF
         };
         if wanted == current {
            return;
         }
         ptr::write(value, wanted);
         ptr::write(tag, u32::from(wanted != 0 || had));
      }
   }
}

#[cfg(target_arch = "arm")]
mod imp {
   use std::{
      ffi::{
         c_int,
         c_void,
      },
      ptr::NonNull,
      sync::OnceLock,
   };

   use phoenix_hook::{
      Hook,
      Inline,
      image::Image,
   };

   static HOOK: OnceLock<Option<Inline>> = OnceLock::new();

   /// Hooks the check from the loader's constructor, before any thread can be
   /// inside it, because the hook rewrites its first 8 or 10 bytes.
   pub fn start(image: &Image<'_>) {
      HOOK.get_or_init(|| hook(image));
   }

   /// The check is hooked, so the store is left alone.
   pub const unsafe fn apply(_state: *mut c_void) {}

   fn hook(image: &Image<'_>) -> Option<Inline> {
      let target = phoenix_sigs::mobile::staff_arm(image)
         .inspect_err(|err| log::warn!("staff: {err}"))
         .ok()?;
      // ShadowHook picks ARM or Thumb by bit 0, and the check is Thumb.
      let target = NonNull::new((target | 1) as *mut c_void)?;
      let detour = NonNull::new(is_staff as *mut c_void)?;
      // SAFETY: the pattern matches the start of the check, and the detour has
      // its ABI.
      match unsafe { Inline::attach(target, detour) } {
         Ok(hook) => Some(hook),
         Err(err) => {
            log::warn!("staff: {err}");
            None
         },
      }
   }

   extern "C" fn is_staff(store: *const c_void, force: c_int) -> bool {
      if phoenix_runtime::staff_enabled() {
         return true;
      }
      let Some(hook) = HOOK.wait() else {
         return false;
      };
      // SAFETY: the trampoline runs the original check, which has this ABI.
      let original: extern "C" fn(*const c_void, c_int) -> bool =
         unsafe { std::mem::transmute(hook.original().as_ptr()) };
      original(store, force)
   }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
mod imp {
   use std::ffi::c_void;

   use phoenix_hook::image::Image;

   pub fn start(_image: &'static Image<'static>) {}

   pub const unsafe fn apply(_state: *mut c_void) {}
}
