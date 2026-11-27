//! Custom emoji presses in the emoji picker under the message box, for
//! `phoenix.messages.onEmojiPick`, and Phoenix's emoji check, which while a
//! handler exists stops locking emojis that need Nitro in the message box.

use phoenix_ui_mobile::Op;

use crate::{
   bridge,
   js::messages::pick_emoji,
   patches::{
      Patches,
      Place,
      aids,
      commands,
      patches,
      worker::press_context,
   },
};

/// Puts what an `onEmojiPick` handler returned at the end of the message box,
/// or gives Phoenix the press back when none returned anything.
pub(super) fn pick(input: i32, id: &str, text: String, frame: &[u8]) -> Vec<Op> {
   let context = press_context(&Place::Screen);
   let Some(insert) =
      phoenix_runtime::engine().and_then(|engine| pick_emoji(&engine, id, &context))
   else {
      patches().resent.push(frame.to_owned());
      if !bridge::inject(frame) {
         patches().resent.retain(|sent| *sent != frame);
         log::warn!("patches: the emoji picker did not take the press back");
      }
      return Vec::new();
   };
   let text = text + &insert;
   if !bridge::inject(&commands::change_text(input, &text)) {
      log::warn!("patches: the message box did not take the emoji");
   }
   patches().mirror.set_composer_text(input, text.clone());
   vec![commands::force_text(input, &text)]
}

impl Patches {
   /// Whether `id` is in the emoji picker under the message box (`xp-sheet`),
   /// not the one for reactions, which opens in a sheet of its own.
   pub(super) fn in_message_picker(&self, mut id: i32) -> bool {
      while let Some(parent) = self.mirror.parent(id) {
         if self.mirror.accessibility_id(parent) == Some(aids::EMOJI_SHEET) {
            return true;
         }
         id = parent;
      }
      false
   }
}

/// Hooks Phoenix's emoji check. Runs from the loader's constructor, before any
/// thread can be inside the check, because an arm32 hook rewrites 8 bytes of
/// it and a thread there would run the half-written jump.
#[cfg_attr(
   not(all(
      target_os = "android",
      any(target_arch = "aarch64", target_arch = "arm")
   )),
   allow(clippy::missing_const_for_fn, reason = "only Android ARM has the hook")
)]
pub(super) fn install() {
   #[cfg(all(
      target_os = "android",
      any(target_arch = "aarch64", target_arch = "arm")
   ))]
   check::install();
}

/// While `on`, emojis Phoenix keeps for Nitro count as usable in the message
/// box: the picker and autocomplete offer them without a lock. Reactions,
/// statuses and the rest keep Phoenix's answer, and nothing else about Nitro
/// changes.
#[cfg_attr(
   not(all(
      target_os = "android",
      any(target_arch = "aarch64", target_arch = "arm")
   )),
   allow(
      clippy::missing_const_for_fn,
      unused_variables,
      reason = "only Android ARM has the hook"
   )
)]
pub(super) fn unlock(on: bool) {
   #[cfg(all(
      target_os = "android",
      any(target_arch = "aarch64", target_arch = "arm")
   ))]
   check::unlock(on);
}

/// The hook on Phoenix's `getEmojiUnavailableReason`, `u8 f(&Emoji,
/// &EmojiContext)`, where 0xFF means usable and 2 premium locked.
#[cfg(all(
   target_os = "android",
   any(target_arch = "aarch64", target_arch = "arm")
))]
mod check {
   use std::sync::{
      OnceLock,
      atomic::{
         AtomicBool,
         Ordering,
      },
   };

   use phoenix_hook::{
      Hook,
      Inline,
   };

   /// `EmojiContext.intention`, and its value for the message box (`CHAT`).
   #[cfg(target_arch = "aarch64")]
   const INTENTION: usize = 0x24;
   #[cfg(target_arch = "arm")]
   const INTENTION: usize = 0x1C;
   const CHAT: u8 = 3;
   const PREMIUM_LOCKED: u8 = 2;
   const USABLE: u8 = 0xFF;

   static UNLOCKED: AtomicBool = AtomicBool::new(false);
   static CHECK: OnceLock<Option<Inline>> = OnceLock::new();

   pub fn install() {
      CHECK.get_or_init(hook_check);
   }

   pub fn unlock(on: bool) {
      UNLOCKED.store(on, Ordering::Release);
   }

   fn hook_check() -> Option<Inline> {
      let image = &phoenix_runtime::state()?.image;
      #[cfg(target_arch = "aarch64")]
      let found = phoenix_sigs::mobile::emoji_aarch64(image);
      #[cfg(target_arch = "arm")]
      let found = phoenix_sigs::mobile::emoji_arm(image);
      let target = found
         .inspect_err(|err| log::warn!("emoji check: {err}, Nitro-locked emojis stay locked"))
         .ok()?;
      // ShadowHook picks ARM or Thumb by bit 0, and the arm32 pattern is Thumb.
      let target = if cfg!(target_arch = "arm") {
         target | 1
      } else {
         target
      };
      let target = std::ptr::NonNull::new(target as *mut std::ffi::c_void)?;
      let detour = std::ptr::NonNull::new(unavailable_reason as *mut std::ffi::c_void)?;
      // SAFETY: the pattern matches the start of the check, and the detour has
      // its ABI.
      match unsafe { Inline::attach(target, detour) } {
         Ok(hook) => Some(hook),
         Err(err) => {
            log::warn!("emoji check: {err}");
            None
         },
      }
   }

   extern "C" fn unavailable_reason(emoji: *const u8, context: *const u8) -> u8 {
      // A call can land here before `CHECK` holds the hook that sent it.
      let Some(hook) = CHECK.wait() else {
         return USABLE;
      };
      // SAFETY: the trampoline runs the original check, which has this ABI.
      let original: extern "C" fn(*const u8, *const u8) -> u8 =
         unsafe { std::mem::transmute(hook.original().as_ptr()) };
      let reason = original(emoji, context);
      if reason != PREMIUM_LOCKED || !UNLOCKED.load(Ordering::Acquire) {
         return reason;
      }
      // SAFETY: Phoenix passes a live context, and the intention byte is in it.
      let intention = unsafe { *context.add(INTENTION) };
      if intention == CHAT { USABLE } else { reason }
   }
}
