//! Where Phoenix's gateway handlers live and how they are called.
//!
//! The event set is never compiled in.
//! [`GatewayRegistry`](registry::GatewayRegistry) walks the target's registry
//! at load time by event name, so a new Discord build brings its own events and
//! needs no table entry. `cargo run -p sigscan -- liblibdiscord_wasm_host.so`
//! dumps what it finds.

use std::ffi::c_void;

#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
pub mod desktop;
#[cfg(feature = "file")] pub mod file;
#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
pub mod gateway_send;
#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
pub mod http;
pub mod ios;
#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
pub mod macos;
pub mod mobile;
pub mod registry;

/// What a locator found, or why this build does not match it.
pub type Found<T> = Result<T, String>;

/// The only match, or how many there were.
fn one<T: Copy>(what: &str, found: &[T]) -> Found<T> {
   match found {
      [only] => Ok(*only),
      _ => Err(format!("{what} matched {} times", found.len())),
   }
}

/// `flag_override: Option<_>` in the store state, as the byte offsets of its
/// tag and value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StaffField {
   pub tag:   usize,
   pub value: usize,
}

/// Libraries that carry the gateway registry, Phoenix's and the stand-in's.
#[cfg(not(any(
   target_os = "windows",
   target_os = "ios",
   target_os = "macos",
   all(target_os = "linux", target_arch = "x86_64")
)))]
pub const LIBRARIES: &[&str] = &["liblibdiscord_wasm_host.so", "libstand_in.so"];
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const LIBRARIES: &[&str] = &["discord-phoenix", "libstand_in.so"];
#[cfg(target_os = "ios")]
pub const LIBRARIES: &[&str] = &["PhoenixFatClient"];
#[cfg(target_os = "macos")]
pub const LIBRARIES: &[&str] = &["Discord", "libstand_in.dylib"];
#[cfg(target_os = "windows")]
pub const LIBRARIES: &[&str] = &["Discord.exe", "stand_in.dll"];

/// What macOS Phoenix sets when it re-execs itself to apply its malloc
/// settings.
///
/// The re-exec keeps the pid, and the loader starts only in the image that has
/// this, since the first one is replaced. `sigscan check` reports a build
/// without it, where the loader would never start.
pub const MACOS_REEXEC: &str = "DISCORD_DESKTOP_MALLOC_REEXEC";

/// One of the malloc settings macOS Phoenix re-execs itself to apply.
///
/// Launch Services sets them all from its Info.plist's `LSEnvironment`, so an
/// app started through `open` never re-execs or sets [`MACOS_REEXEC`].
pub const MACOS_MALLOC: &str = "MallocNanoZone";

/// Name of the plugin bundle the patcher stores next to the loader in the APK's
/// native library directory, a JSON object of plugin name to source.
pub const PLUGIN_BUNDLE: &str = "libplm.plugins.so";

/// Offset of the handler pointer in a gateway registry entry `{name ptr, name
/// len, handler}`.
pub const GATEWAY_HANDLER_FIELD: usize = 2 * size_of::<usize>();

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawStr {
   pub ptr: *const u8,
   pub len: usize,
}

/// Host fields after `name` and `data` that a handler reads and a synthetic
/// event must carry. The arm32 handler reads only `name` and `data`.
#[cfg(not(target_arch = "arm"))]
pub const REST_WORDS: usize = 4;
#[cfg(target_arch = "arm")]
pub const REST_WORDS: usize = 0;

/// Phoenix's gateway event, `name` and `data` are borrowed UTF-8 with `data`
/// holding the raw dispatch JSON. `rest` is opaque and copied through
/// unchanged.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GatewayEvent {
   pub name: RawStr,
   pub data: RawStr,
   pub rest: [u64; REST_WORDS],
}

/// The Rust `Result` a handler returns through a hidden pointer:
/// - 96 bytes (12 words) on Desktop Phoenix (`Discord.exe`, Linux
///   `discord-phoenix`, macOS `Discord`), with `Ok` niche `u64::MAX - 1`
///   (`0xffff_ffff_ffff_fffe`)
/// - 32 bytes (4 words) on mobile 64-bit, with `Ok` niche `u64::MAX`
/// - 16 bytes (2 words) on armeabi-v7a, with `Ok` niche `u64::MAX`
#[cfg(any(
   target_os = "windows",
   target_os = "macos",
   all(target_os = "linux", target_arch = "x86_64")
))]
pub const RESULT_WORDS: usize = 12;
#[cfg(not(any(
   target_os = "windows",
   target_os = "macos",
   all(target_os = "linux", target_arch = "x86_64"),
   target_arch = "arm"
)))]
pub const RESULT_WORDS: usize = 4;
#[cfg(target_arch = "arm")]
pub const RESULT_WORDS: usize = 2;

#[cfg(any(
   target_os = "windows",
   target_os = "macos",
   all(target_os = "linux", target_arch = "x86_64")
))]
const OK_TAG: u64 = u64::MAX - 1;
#[cfg(not(any(
   target_os = "windows",
   target_os = "macos",
   all(target_os = "linux", target_arch = "x86_64")
)))]
const OK_TAG: u64 = u64::MAX;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HandlerResult(pub [u64; RESULT_WORDS]);

impl HandlerResult {
   pub const OK: Self = {
      let mut words = [0; RESULT_WORDS];
      words[0] = OK_TAG;
      Self(words)
   };

   #[must_use]
   pub const fn is_ok(&self) -> bool {
      self.0[0] == OK_TAG
   }
}

/// A gateway registry handler, `handler(store, event, ctx)`.
pub type GatewayHandler =
   unsafe extern "C" fn(*mut c_void, *const GatewayEvent, *mut c_void) -> HandlerResult;

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn handler_result_abi_matches_target() {
      assert!(HandlerResult::OK.is_ok());
      let mut err = HandlerResult::OK;
      err.0[0] = 0;
      assert!(!err.is_ok());
      #[cfg(any(
         target_os = "windows",
         target_os = "macos",
         all(target_os = "linux", target_arch = "x86_64")
      ))]
      {
         assert_eq!(size_of::<HandlerResult>(), 96);
         assert_eq!(HandlerResult::OK.0[0], 0xFFFF_FFFF_FFFF_FFFE);
      }
      #[cfg(not(any(
         target_os = "windows",
         target_os = "macos",
         all(target_os = "linux", target_arch = "x86_64"),
         target_arch = "arm"
      )))]
      {
         assert_eq!(size_of::<HandlerResult>(), 32);
         assert_eq!(HandlerResult::OK.0[0], u64::MAX);
      }
   }
}
