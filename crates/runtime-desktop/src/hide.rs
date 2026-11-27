//! What a theme's `hide` list leaves out of Discord Phoenix. Every element is named by the id it
//! is built with, so `Div::id` is hooked: a div whose id is on the list is built with
//! `display: none`, which takes it out of the layout, gap included. The gift button is the
//! exception: [`composer`] already skips it where the composer builds it.

use std::{
   ffi::c_void,
   slice,
   sync::{
      OnceLock,
      atomic::{AtomicBool, Ordering},
   },
};

use phoenix_hook::{image::Image, pattern::Pattern, x86::Abi};
use phoenix_runtime::theme::Effects;
use phoenix_sigs::desktop;

use crate::{composer, gpui};

/// `push rbp; push rsi; sub rsp, imm8; lea rbp, [rsp + imm8]`: how `Div::id` opens in the build
/// that was checked. A build that opens differently leaves the hook off.
const PROLOGUE: Pattern = Pattern::new("55 56 48 83 EC ?? 48 8D 6C 24 ??");
const PROLOGUE_LEN: usize = 11;

type DivIdFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *const u8, usize) -> *mut c_void;

static ORIGINAL: OnceLock<DivIdFn> = OnceLock::new();
static ANY: AtomicBool = AtomicBool::new(false);

/// The theme id, the element id Discord builds it with, and whether a theme hides it now.
static TARGETS: [(&str, &[u8], AtomicBool); 3] = [
   ("dm.nitro", b"dm-nitro-row", AtomicBool::new(false)),
   ("dm.quests", b"dm-quests", AtomicBool::new(false)),
   (
      "composer.stickers",
      b"sticker-button",
      AtomicBool::new(false),
   ),
];

/// Hooks `Div::id` from [`crate::prepare`], before Discord's threads build anything.
pub fn install(image: &Image<'static>) {
   if !crate::X86_CODE || Abi::of(image) != Abi::Win64 {
      return;
   }
   let div_id = match desktop::div_id(image) {
      Ok(div_id) => div_id,
      Err(err) => {
         log::warn!("hide: {err}, themes cannot hide elements");
         return;
      },
   };
   // SAFETY: `detour` has `Div::id`'s ABI, and nothing runs before `prepare` finishes.
   match unsafe { image.hook_entry(div_id, PROLOGUE, PROLOGUE_LEN, detour as *const () as usize) } {
      Ok(trampoline) => {
         // SAFETY: the trampoline replays the prologue and continues in `Div::id`.
         let _ = ORIGINAL.set(unsafe { std::mem::transmute::<usize, DivIdFn>(trampoline) });
         log::info!("hide: hooked Div::id at {div_id:#x}");
      },
      Err(err) => log::warn!("hide: {err}, themes cannot hide elements"),
   }
}

unsafe extern "C" fn detour(
   out: *mut c_void,
   div: *mut c_void,
   id: *const u8,
   len: usize,
) -> *mut c_void {
   let Some(original) = ORIGINAL.get() else {
      return out;
   };
   // SAFETY: forwards the caller's arguments.
   let built = unsafe { original(out, div, id, len) };
   if ANY.load(Ordering::Relaxed) && !id.is_null() && len <= 32 {
      // SAFETY: `Div::id` takes its name as a pointer and a length.
      let name = unsafe { slice::from_raw_parts(id, len) };
      if TARGETS
         .iter()
         .any(|(_, element, hidden)| *element == name && hidden.load(Ordering::Relaxed))
      {
         // SAFETY: `built` is the `Div` the original just returned.
         unsafe { gpui::display_none(built) };
      }
   }
   built
}

/// Whether this build can hide the theme id `id`.
pub fn supports(id: &str) -> bool {
   composer::supports(id)
      || (ORIGINAL.get().is_some() && TARGETS.iter().any(|(theme, ..)| *theme == id))
}

/// Hides what the enabled themes ask for, and shows it again when they stop.
pub fn apply(effects: &Effects) {
   composer::apply(effects);
   let mut any = false;
   for (theme, _, hidden) in &TARGETS {
      let on = effects.hide.contains(*theme);
      hidden.store(on, Ordering::Relaxed);
      any |= on;
   }
   ANY.store(any, Ordering::Relaxed);
}
