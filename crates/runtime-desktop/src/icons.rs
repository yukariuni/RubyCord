//! Plugin SVG icons for Desktop Phoenix.
//!
//! GPUI's `svg().path(path)` loads its bytes through Discord's asset loader
//! and fills the shape with the element's text color. Plugins hand the runtime
//! SVG source, which [`register`] files under a `rubycord/icons/` path, and the
//! hooked loader answers those paths with the source, in a `Vec` from Discord's
//! own allocator, which Discord frees when it is done.

use std::{
   collections::HashMap,
   sync::{LazyLock, OnceLock, PoisonError, RwLock},
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

use crate::gpui;

/// What the loader returns: `Vec<u8>` as `{cap, ptr, len}`, or `NOT_FOUND` in
/// the first word.
#[repr(C)]
struct Loaded {
   cap: usize,
   ptr: *mut u8,
   len: usize,
}

type AssetLoadFn = unsafe extern "C" fn(*mut Loaded, *const u8, usize) -> *mut Loaded;

const PREFIX: &str = "rubycord/icons/";
/// Plenty for an icon; anything larger is not drawn.
const MAX_SVG: usize = 64 * 1024;
const MAX_ICONS: usize = 1024;

static ORIG_LOAD: OnceLock<AssetLoadFn> = OnceLock::new();
/// SVG source by asset path.
type Icons = HashMap<&'static str, Box<[u8]>>;

static ICONS: LazyLock<RwLock<Icons>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Whether plugin icons can be loaded and drawn.
pub fn ready() -> bool {
   ORIG_LOAD.get().is_some() && gpui::svg_ready()
}

/// Files `svg` under a path named for its contents and returns the path, or
/// nothing when icons cannot be drawn or the source is unusable.
pub fn register(svg: &str) -> Option<&'static str> {
   if !ready() || svg.is_empty() || svg.len() > MAX_SVG || !svg.contains("<svg") {
      return None;
   }
   let path = format!("{PREFIX}{:016x}.svg", fnv1a(svg.as_bytes()));
   let mut icons = ICONS.write().unwrap_or_else(PoisonError::into_inner);
   if let Some((&known, _)) = icons.get_key_value(path.as_str()) {
      return Some(known);
   }
   if icons.len() >= MAX_ICONS {
      return None;
   }
   let path = gpui::intern_str(&path);
   icons.insert(path, svg.as_bytes().into());
   drop(icons);
   Some(path)
}

fn fnv1a(bytes: &[u8]) -> u64 {
   bytes.iter().fold(0xCBF2_9CE4_8422_2325, |hash, &byte| {
      (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3)
   })
}

/// Hooks Discord's asset loader during [`crate::prepare`], once
/// [`gpui::locate_svg`] found the `Svg` builders.
pub fn install(image: &Image<'static>) {
   if !gpui::svg_ready() {
      return;
   }
   let load = match desktop::asset_load(image) {
      Ok(load) => load,
      Err(err) => {
         log::warn!("desktop icons: {err}, icons fall back to text");
         return;
      },
   };
   let pro = desktop::Prologues::of(phoenix_hook::x86::Abi::of(image)).asset_load;
   // SAFETY: runs from `prepare` before Discord threads execute; `load_detour`
   // has the loader's `(sret, path ptr, path len)` signature.
   match unsafe {
      image.hook_entry(
         load,
         pro.pattern,
         pro.len,
         load_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => {
         // SAFETY: `trampoline` replays the prologue and jumps past it.
         let orig = unsafe { std::mem::transmute::<usize, AssetLoadFn>(trampoline) };
         let _ = ORIG_LOAD.set(orig);
         log::info!("desktop icons: hooked the asset loader @ {load:#x}");
      },
      Err(err) => log::warn!("desktop icons: asset loader hook failed: {err}"),
   }
}

unsafe extern "C" fn load_detour(sret: *mut Loaded, path: *const u8, len: usize) -> *mut Loaded {
   if !path.is_null() && len > PREFIX.len() && len < 128 && !sret.is_null() {
      // SAFETY: the loader's path, `len` bytes of UTF-8.
      let bytes = unsafe { std::slice::from_raw_parts(path, len) };
      if bytes.starts_with(PREFIX.as_bytes())
         && let Ok(path) = std::str::from_utf8(bytes)
         && let Some(owned) = copy_icon(path)
      {
         // SAFETY: `sret` is the loader's result slot.
         unsafe { sret.write(owned) };
         return sret;
      }
   }
   let Some(orig) = ORIG_LOAD.get().copied() else {
      return sret;
   };
   // SAFETY: forwards the loader's arguments.
   unsafe { orig(sret, path, len) }
}

/// The icon at `path` in a fresh `Vec` from Discord's allocator.
fn copy_icon(path: &str) -> Option<Loaded> {
   let svg = ICONS
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .get(path)?
      .clone();
   let ptr = gpui::discord_alloc(svg.len())?;
   // SAFETY: `ptr` holds `svg.len()` fresh bytes.
   unsafe { std::ptr::copy_nonoverlapping(svg.as_ptr(), ptr, svg.len()) };
   Some(Loaded {
      cap: svg.len(),
      ptr,
      len: svg.len(),
   })
}
