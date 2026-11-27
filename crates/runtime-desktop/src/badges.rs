//! Native custom badge URL unwrapping for Desktop Phoenix.
//!
//! Plugins like `global-badges` inject custom badges into `/users/{id}/profile`
//! (as `icon: "phx_u_<hex>"`, which Desktop Phoenix turns into
//! `{cdn}/badge-icons/phx_u_<hex>.png`) and `/users/{id}/badges` (as
//! `simple_icon_url: "https://cdn.discordapp.com/badge-icons/phx_u_<hex>.png"`).
//!
//! On Desktop Phoenix, image fetches run through `libdiscord_media::fetch`
//! (building its own `reqwest::RequestBuilder`, separate from
//! `ReqwestTransport::send`). Hooking `libdiscord_media::fetch` and unwrapping
//! `/badge-icons/phx_u_<hex>` back to the real `https://` URL makes custom
//! badges render natively in both profile popouts and the Badge Directory.

use std::{
   ffi::c_void,
   panic::{self, AssertUnwindSafe},
   slice, str,
   sync::OnceLock,
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

use crate::gpui;

const MARKER: &str = "/badge-icons/phx_u_";

#[repr(C)]
#[derive(Clone, Copy)]
struct RawPinnedFuture {
   data: *mut c_void,
   vtable: *const c_void,
}

/// Rust-ABI signature of `libdiscord_media::fetch`:
/// `fn(this, url_ptr, url_len, max_bytes) -> Pin<Box<dyn Future>>` in
/// `RAX:RDX`. The Linux build takes two more words in `R8`/`R9` (8 and 0 at
/// its one call site) and returns the box in `RAX` alone, so the detour
/// forwards six words. On Win64 the last two are caller stack slots the
/// original ignores.
type MediaFetchFn =
   unsafe fn(*const c_void, *const u8, usize, usize, usize, usize) -> RawPinnedFuture;

static ORIG_MEDIA_FETCH: OnceLock<MediaFetchFn> = OnceLock::new();

/// Locates and inline-hooks `libdiscord_media::fetch` during
/// [`crate::prepare`].
pub fn install(image: &Image<'static>) {
   let found = if crate::X86_CODE {
      desktop::media_fetch(image)
   } else {
      phoenix_sigs::macos::media_fetch(image)
   };
   let func = match found {
      Ok(func) => func,
      Err(err) => {
         log::warn!("desktop badges: {err}, custom badge icons disabled");
         return;
      },
   };
   let pro = desktop::Prologues::of(phoenix_hook::x86::Abi::of(image)).media_fetch;
   // SAFETY: runs from `prepare` before Discord threads execute;
   // `media_fetch_detour` matches `libdiscord_media::fetch`'s exact Rust ABI
   // (`this, url.ptr, url.len, max_bytes -> Pin<Box<dyn Future>>`).
   let trampoline = match unsafe {
      image.hook_entry(
         func,
         pro.pattern,
         pro.len,
         media_fetch_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("desktop badges: media_fetch hook failed: {err}");
         return;
      },
   };
   // SAFETY: `trampoline` replays the 15-byte prologue and jumps to `func + 15`.
   let orig = unsafe { std::mem::transmute::<usize, MediaFetchFn>(trampoline) };
   let _ = ORIG_MEDIA_FETCH.set(orig);
   log::info!("desktop badges: hooked libdiscord_media::fetch @ {func:#x}");
}

fn decode_hex_url(hex: &str) -> Option<String> {
   if hex.is_empty() || !hex.len().is_multiple_of(2) {
      return None;
   }
   let mut bytes = Vec::with_capacity(hex.len() / 2);
   for pair in hex.as_bytes().as_chunks::<2>().0 {
      let hi = (pair[0] as char).to_digit(16)?;
      let lo = (pair[1] as char).to_digit(16)?;
      bytes.push(u8::try_from((hi << 4) | lo).ok()?);
   }
   let url = String::from_utf8(bytes).ok()?;
   url.starts_with("https://").then_some(url)
}

fn unwrap_badge_url(url: &str) -> Option<String> {
   let pos = url.find(MARKER)?;
   let rest = &url[pos + MARKER.len()..];
   let hex = rest.split(['.', '?', '#']).next().unwrap_or(rest);
   decode_hex_url(hex)
}

unsafe fn media_fetch_detour(
   this: *const c_void,
   url_ptr: *const u8,
   url_len: usize,
   max_bytes: usize,
   extra0: usize,
   extra1: usize,
) -> RawPinnedFuture {
   let orig = ORIG_MEDIA_FETCH
      .get()
      .copied()
      .expect("media_fetch_detour only runs after install");

   let rewritten = if !url_ptr.is_null() && url_len > MARKER.len() && url_len <= 8192 {
      panic::catch_unwind(AssertUnwindSafe(|| {
         // SAFETY: `url_ptr..url_ptr + url_len` is a valid `&str` passed by
         // `libdiscord_media`.
         let bytes = unsafe { slice::from_raw_parts(url_ptr, url_len) };
         let url = str::from_utf8(bytes).ok()?;
         let decoded = unwrap_badge_url(url)?;
         // The returned future borrows `&str` across `.await` points, so intern
         // it to `'static` lifetime (bounded by distinct badge URLs seen).
         Some(gpui::intern_str(&decoded))
      }))
      .ok()
      .flatten()
   } else {
      None
   };

   if let Some(real_url) = rewritten {
      // SAFETY: `real_url` is a `'static` UTF-8 string slice.
      return unsafe {
         orig(
            this,
            real_url.as_ptr(),
            real_url.len(),
            max_bytes,
            extra0,
            extra1,
         )
      };
   }
   // SAFETY: forward untouched arguments to original `libdiscord_media::fetch`.
   unsafe { orig(this, url_ptr, url_len, max_bytes, extra0, extra1) }
}

#[cfg(test)]
mod tests {
   use std::fmt::Write;

   use super::*;

   #[test]
   fn unwraps_valid_https_badge_urls() {
      let real = "https://badges.equicord.org/icon.png";
      let hex = real.bytes().fold(String::new(), |mut hex, b| {
         let _ = write!(hex, "{b:02x}");
         hex
      });
      let wrapped = format!("https://cdn.discordapp.com/badge-icons/phx_u_{hex}.png?size=64");
      assert_eq!(unwrap_badge_url(&wrapped).as_deref(), Some(real));
   }

   #[test]
   fn rejects_non_https_or_malformed_badge_urls() {
      let http = "http://example.com/a.png";
      let hex = http.bytes().fold(String::new(), |mut hex, b| {
         let _ = write!(hex, "{b:02x}");
         hex
      });
      let wrapped = format!("https://cdn.discordapp.com/badge-icons/phx_u_{hex}.png");
      assert_eq!(unwrap_badge_url(&wrapped), None);
      assert_eq!(
         unwrap_badge_url("https://cdn.discordapp.com/badge-icons/phx_u_zz.png"),
         None
      );
      assert_eq!(
         unwrap_badge_url("https://cdn.discordapp.com/badge-icons/normal.png"),
         None
      );
   }
}
