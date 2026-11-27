//! The composer's Nitro gift button, which `phoenix.ui.hide("composer.gift")` and a theme's
//! `"hide": ["composer.gift"]` leave out.

use std::sync::{
   Mutex, OnceLock, PoisonError,
   atomic::{AtomicBool, Ordering},
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

/// `jne rel32`, and `nop; jmp rel32` to the same place.
const SHOWN: [u8; 2] = [0x0F, 0x85];
const HIDDEN: [u8; 2] = [0x90, 0xE9];

static GIFT: OnceLock<usize> = OnceLock::new();
/// Plugins holding the gift button hidden.
static HIDERS: Mutex<usize> = Mutex::new(0);

/// Finds the gift button's branch from [`crate::prepare`], while the locator
/// indexes are built.
pub fn install(image: &Image<'static>) {
   match desktop::composer_gift(image) {
      Ok(site) if image.read(site, 2) == Some(&SHOWN[..]) => {
         let _ = GIFT.set(site);
      },
      Ok(site) => log::warn!("composer gift: {site:#x} is no jne"),
      Err(err) => log::warn!("composer gift: {err}, the gift button stays"),
   }
}

/// Whether this build can hide the theme id `id`.
pub fn supports(id: &str) -> bool {
   id == "composer.gift" && GIFT.get().is_some()
}

/// Hides what the enabled themes ask for, and shows it again when they stop.
pub(crate) fn apply(effects: &phoenix_runtime::theme::Effects) {
   static HIDDEN: AtomicBool = AtomicBool::new(false);
   let want = effects.hide.contains("composer.gift");
   if want != HIDDEN.load(Ordering::Acquire) && hide_gift(want) {
      HIDDEN.store(want, Ordering::Release);
   }
}

/// Hides the gift button until the matching `hide_gift(false)`. False when
/// this build's button was not found.
pub fn hide_gift(hide: bool) -> bool {
   let (Some(&site), Some(state)) = (GIFT.get(), phoenix_runtime::state()) else {
      return false;
   };
   let mut hiders = HIDERS.lock().unwrap_or_else(PoisonError::into_inner);
   let next = if hide {
      *hiders + 1
   } else {
      hiders.saturating_sub(1)
   };
   if (*hiders > 0) != (next > 0) {
      let bytes = if hide { HIDDEN } else { SHOWN };
      // SAFETY: both bytes lie in one aligned word, which the locator checked,
      // so a thread at the branch reads the old or the new opcode whole.
      if let Err(err) = unsafe { state.image.write_code(site, &bytes) } {
         log::error!("composer gift: {err}");
         return false;
      }
   }
   *hiders = next;
   true
}
