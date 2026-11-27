use std::{
   ffi::{
      CStr,
      c_void,
   },
   ptr::{
      self,
      NonNull,
   },
   sync::OnceLock,
};

use sunhook_sys::{
   SHADOWHOOK_MODE_UNIQUE,
   shadowhook_get_errno,
   shadowhook_hook_func_addr,
   shadowhook_init,
   shadowhook_to_errmsg,
   shadowhook_unhook,
};

use crate::{
   Error,
   Hook,
};

pub struct ShadowHook {
   stub:     NonNull<c_void>,
   target:   usize,
   original: NonNull<c_void>,
}

// SAFETY: the stub is an opaque token shadowhook accepts from any thread, the
// rest is code.
unsafe impl Send for ShadowHook {}
// SAFETY: see the Send impl.
unsafe impl Sync for ShadowHook {}

fn errmsg(errno: i32) -> String {
   // SAFETY: `shadowhook_to_errmsg` returns a static NUL-terminated string for
   // every errno.
   unsafe { CStr::from_ptr(shadowhook_to_errmsg(errno)) }
      .to_string_lossy()
      .into_owned()
}

pub fn init() -> Result<(), Error> {
   static INIT: OnceLock<i32> = OnceLock::new();

   // SAFETY: shadowhook serializes init internally.
   let errno = *INIT.get_or_init(|| unsafe { shadowhook_init(SHADOWHOOK_MODE_UNIQUE, false) });

   if errno == 0 {
      Ok(())
   } else {
      Err(Error::Init {
         backend: "shadowhook",
         message: errmsg(errno),
      })
   }
}

impl Hook for ShadowHook {
   unsafe fn attach(target: NonNull<c_void>, detour: NonNull<c_void>) -> Result<Self, Error> {
      init()?;
      let mut original = ptr::null_mut();
      // SAFETY: forwarded from the caller.
      let stub =
         unsafe { shadowhook_hook_func_addr(target.as_ptr(), detour.as_ptr(), &raw mut original) };
      let attach_error = || {
         Error::Attach {
            backend: "shadowhook",
            target:  target.addr().get(),
            // SAFETY: plain FFI call reading a thread-local errno.
            message: errmsg(unsafe { shadowhook_get_errno() }),
         }
      };
      let stub = NonNull::new(stub).ok_or_else(attach_error)?;
      let original = NonNull::new(original).ok_or_else(attach_error)?;
      Ok(Self {
         stub,
         target: target.addr().get(),
         original,
      })
   }

   fn original(&self) -> NonNull<c_void> {
      self.original
   }

   unsafe fn detach(self) -> Result<(), Error> {
      // SAFETY: the stub came from shadowhook_hook_func_addr and is unhooked
      // once.
      if unsafe { shadowhook_unhook(self.stub.as_ptr()) } == 0 {
         return Ok(());
      }
      Err(Error::Detach {
         backend: "shadowhook",
         target:  self.target,
         // SAFETY: plain FFI call reading a thread-local errno.
         message: errmsg(unsafe { shadowhook_get_errno() }),
      })
   }
}
