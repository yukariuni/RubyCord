//! Function hooks and signature scanning for phoenix.
//!
//! - [`Hook`] is the inline-hook interface, and [`Inline`] is `ShadowHook` on
//!   Android ARM, the one target that hooks code.
//! - [`Image`](image::Image) views a loaded ELF module, scans it for
//!   [`Pattern`](pattern::Pattern)s, and swaps pointers in its read-only data.

use std::{
   ffi::c_void,
   ptr::NonNull,
};

// On wasm32 only the parts the host-side patcher reads build; live process
// scanning uses dl_iterate_phdr on Unix and PE module enumeration on Windows.
#[cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]
pub mod arm64;
pub mod image;
pub mod pattern;
// macOS builds it on arm64 too, so sigscan reads x86 Desktop builds there and
// the desktop runtime compiles; its locators are not run on arm64 code.
#[cfg(all(
   target_os = "android",
   any(target_arch = "aarch64", target_arch = "arm")
))]
pub mod shadowhook;
#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
pub mod x86;

#[cfg(all(
   target_os = "android",
   any(target_arch = "aarch64", target_arch = "arm")
))]
pub type Inline = shadowhook::ShadowHook;

#[derive(Debug, flub::Error)]
pub enum Error {
   #[error("{backend} failed to initialize: {message}")]
   Init {
      backend: &'static str,
      message: String,
   },
   #[error("{backend} could not hook {target:#x}: {message}")]
   Attach {
      backend: &'static str,
      target:  usize,
      message: String,
   },
   #[error("{backend} could not unhook {target:#x}: {message}")]
   Detach {
      backend: &'static str,
      target:  usize,
      message: String,
   },
   #[error("{addr:#x} is not in any mapping")]
   Unmapped { addr: usize },
   #[error("mprotect {addr:#x}")]
   Protect {
      addr:   usize,
      #[source]
      source: std::io::Error,
   },
}

/// An installed inline hook. Dropping it leaves the hook in place, use
/// [`Hook::detach`] to restore the original code.
pub trait Hook: Sized + Send + Sync {
   /// Redirects calls to `target` into `detour`.
   ///
   /// # Safety
   ///
   /// `target` is the first instruction of a function, and `detour` has the
   /// same ABI.
   unsafe fn attach(target: NonNull<c_void>, detour: NonNull<c_void>) -> Result<Self, Error>;

   /// Entry point that runs the original function body.
   fn original(&self) -> NonNull<c_void>;

   /// # Safety
   ///
   /// No thread is inside the detour or holds the [`Hook::original`]
   /// trampoline.
   unsafe fn detach(self) -> Result<(), Error>;
}
