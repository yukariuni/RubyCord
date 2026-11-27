//! Phoenix's emoji check, which while a plugin holds an
//! `phoenix.messages.onEmojiPick` handler stops locking emojis that need Nitro
//! in the message box. The picker and autocomplete then insert them like any
//! other, and `onSend` sees them in the message Phoenix posts.

use std::sync::{
   OnceLock,
   atomic::{AtomicBool, Ordering},
};

use phoenix_hook::{image::Image, x86::Abi};
use phoenix_sigs::desktop;

/// `getEmojiUnavailableReason(&Emoji, &EmojiContext) -> u8`.
type CheckFn = unsafe extern "C" fn(*const u8, *const u8) -> u8;

/// `EmojiContext.intention` for the message box.
const CHAT: u8 = 3;
const PREMIUM_LOCKED: u8 = 2;
const USABLE: u8 = 0xFF;

static UNLOCKED: AtomicBool = AtomicBool::new(false);
/// The original check and the context's intention offset.
static CHECK: OnceLock<(CheckFn, usize)> = OnceLock::new();

/// Hooks the check from [`crate::prepare`], before Discord's threads run.
pub fn install(image: &Image<'static>) {
   let located = if crate::X86_CODE {
      desktop::emoji_check(image)
   } else {
      phoenix_sigs::macos::emoji_check(image)
   };
   let found = match located {
      Ok(found) => found,
      Err(err) => {
         log::warn!("emoji check: {err}, Nitro-locked emojis stay locked");
         return;
      },
   };
   let prologue = desktop::Prologues::of(Abi::of(image)).emoji_check;
   // SAFETY: runs before Discord's threads, and the detour has the check's
   // ABI.
   let trampoline = match unsafe {
      image.hook_entry(
         found.func,
         prologue.pattern,
         prologue.len,
         unavailable_reason as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("emoji check: {err}");
         return;
      },
   };
   // SAFETY: the trampoline replays the displaced instructions and continues
   // in the check.
   let original = unsafe { std::mem::transmute::<usize, CheckFn>(trampoline) };
   let _ = CHECK.set((original, found.intention));
   log::info!("emoji check: hooked @ {:#x}", found.func);
}

/// While `on`, emojis Phoenix keeps for Nitro count as usable in the message
/// box. Reactions, statuses and the rest keep Phoenix's answer.
pub fn unlock(on: bool) {
   UNLOCKED.store(on, Ordering::Release);
}

extern "C" fn unavailable_reason(emoji: *const u8, context: *const u8) -> u8 {
   // The jump is written just before `CHECK` is set, all before Discord runs.
   let &(original, intention) = CHECK.wait();
   // SAFETY: the trampoline runs the original check with Phoenix's arguments.
   let reason = unsafe { original(emoji, context) };
   if reason != PREMIUM_LOCKED || !UNLOCKED.load(Ordering::Acquire) {
      return reason;
   }
   // SAFETY: Phoenix passes a live context, and the original just read this
   // byte of it.
   if unsafe { *context.add(intention) } == CHAT {
      USABLE
   } else {
      reason
   }
}
