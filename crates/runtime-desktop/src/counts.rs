//! Phoenix's pending friend and message request counts, which
//! `phoenix.ui.hideCount` hides while the requests themselves stay listed.
//!
//! The friend requests count is answered with 0 where Phoenix computes it, for
//! the DM list's Friends row and the inbox's Friends tab, and taken out of the
//! Home button's count and the badge built like it. The message requests count
//! also decides whether the Message Requests row shows, so only the DM list's
//! badge for it draws 0.

use std::{
   str::FromStr,
   sync::{
      OnceLock,
      atomic::{AtomicBool, AtomicUsize, Ordering},
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::{
   Found,
   desktop::{self, FRIENDS_BADGE, MESSAGE_REQUESTS_BADGE},
};

/// The DM list's row badge, `fn(out, id: &str, count: u32, _, _) -> out`.
type BadgeFn = unsafe extern "C" fn(*mut u8, *const u8, usize, u32, usize, usize) -> *mut u8;
/// `u32 f(&RelationshipStore)`.
type CountFn = unsafe extern "C" fn(*const u8) -> u32;

/// The Home badge's state when it shows a count.
const STATE_COUNT: u32 = 2;
/// The Home badge's state when it shows nothing.
const STATE_NONE: u32 = 0;

static HIDE_FRIEND_REQUESTS: AtomicBool = AtomicBool::new(false);
static HIDE_MESSAGE_REQUESTS: AtomicBool = AtomicBool::new(false);
static BADGE: OnceLock<BadgeFn> = OnceLock::new();
static FRIEND_REQUESTS: OnceLock<CountFn> = OnceLock::new();
static HOME_COUNT: OnceLock<CountFn> = OnceLock::new();
/// The Home badge's trampoline, which [`home_badge_shim`] calls.
static HOME_BADGE: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Count {
   FriendRequests,
   MessageRequests,
}

impl FromStr for Count {
   type Err = ();

   fn from_str(name: &str) -> Result<Self, ()> {
      match name {
         "friendRequests" => Ok(Self::FriendRequests),
         "messageRequests" => Ok(Self::MessageRequests),
         _ => Err(()),
      }
   }
}

/// Hooks what draws and computes the counts from [`crate::prepare`], before
/// Discord's threads run.
pub fn install(image: &Image<'static>) {
   let found = match desktop::pending_counts(image) {
      Ok(found) => found,
      Err(err) => {
         log::warn!("pending counts: {err}, the counts stay");
         return;
      },
   };
   // They read the friend requests count's code, so they look before it is
   // hooked.
   let home = desktop::home_count(image, found.friend_requests);
   let home_state = desktop::home_badge(image, found.friend_requests);

   // SAFETY: runs before Discord's threads, and the detour has the badge's ABI.
   if let Some(original) = unsafe { hook(image, Ok(found.badge), badge as *const () as usize) } {
      // SAFETY: the trampoline replays the displaced instructions and
      // continues in the badge.
      let _ = BADGE.set(unsafe { std::mem::transmute::<usize, BadgeFn>(original) });
   }
   // SAFETY: as above, for the count's ABI.
   if let Some(original) = unsafe {
      hook(
         image,
         Ok(found.friend_requests),
         friend_requests as *const () as usize,
      )
   } {
      // SAFETY: as above, continuing in the count.
      let _ = FRIEND_REQUESTS.set(unsafe { std::mem::transmute::<usize, CountFn>(original) });
   }
   // SAFETY: as above, for the count's ABI.
   if let Some(original) = unsafe { hook(image, home, home_count as *const () as usize) } {
      // SAFETY: as above, continuing in the Home button's count.
      let _ = HOME_COUNT.set(unsafe { std::mem::transmute::<usize, CountFn>(original) });
   }
   // SAFETY: as above, the shim keeps the Home badge's registers.
   if let Some(original) = unsafe { hook(image, home_state, home_badge_shim as *const () as usize) }
   {
      HOME_BADGE.store(original, Ordering::Release);
   }
}

/// Sends `func` to `detour` and returns the trampoline to the original.
///
/// # Safety
///
/// As for [`Image::hook_entry`].
unsafe fn hook(image: &Image<'static>, func: Found<usize>, detour: usize) -> Option<usize> {
   let hooked = func.and_then(|func| {
      let prologue = desktop::entry_prologue(image, func)?;
      // SAFETY: forwarded from the caller.
      unsafe { image.hook_entry(func, prologue.pattern, prologue.len, detour) }
         .map(|trampoline| (func, trampoline))
         .map_err(|err| format!("{func:#x}: {err}"))
   });
   match hooked {
      Ok((func, trampoline)) => {
         log::info!("pending counts: hooked @ {func:#x}");
         Some(trampoline)
      },
      Err(err) => {
         log::warn!("pending counts: {err}");
         None
      },
   }
}

/// Hides each count in `hidden` and shows the others again.
pub fn hide(hidden: impl IntoIterator<Item = Count>) {
   let (mut friends, mut requests) = (false, false);
   for count in hidden {
      match count {
         Count::FriendRequests => friends = true,
         Count::MessageRequests => requests = true,
      }
   }
   let friends_changed = HIDE_FRIEND_REQUESTS.swap(friends, Ordering::AcqRel) != friends;
   let requests_changed = HIDE_MESSAGE_REQUESTS.swap(requests, Ordering::AcqRel) != requests;
   if friends_changed || requests_changed {
      crate::accessories::invalidate_discord_windows();
   }
}

extern "C" fn badge(
   out: *mut u8,
   id: *const u8,
   id_len: usize,
   count: u32,
   cx: usize,
   window: usize,
) -> *mut u8 {
   // The jump is written just before `BADGE` is set, all before Discord runs.
   let original = *BADGE.wait();
   // SAFETY: Phoenix passes the id as a live `&str`.
   let id_bytes = unsafe { std::slice::from_raw_parts(id, id_len) };
   let hidden = id_bytes == FRIENDS_BADGE && HIDE_FRIEND_REQUESTS.load(Ordering::Acquire)
      || id_bytes == MESSAGE_REQUESTS_BADGE && HIDE_MESSAGE_REQUESTS.load(Ordering::Acquire);
   let count = if hidden { 0 } else { count };
   // SAFETY: the trampoline runs the original with Phoenix's arguments.
   unsafe { original(out, id, id_len, count, cx, window) }
}

extern "C" fn friend_requests(store: *const u8) -> u32 {
   if HIDE_FRIEND_REQUESTS.load(Ordering::Acquire) {
      return 0;
   }
   let original = *FRIEND_REQUESTS.wait();
   // SAFETY: the trampoline runs the original with Phoenix's argument.
   unsafe { original(store) }
}

extern "C" fn home_count(store: *const u8) -> u32 {
   // SAFETY: the trampoline runs the original with Phoenix's argument.
   let count = unsafe { (*HOME_COUNT.wait())(store) };
   without_friend_requests(store, count)
}

/// `count` less the friend requests while they are hidden.
fn without_friend_requests(store: *const u8, count: u32) -> u32 {
   match FRIEND_REQUESTS.get() {
      Some(requests) if HIDE_FRIEND_REQUESTS.load(Ordering::Acquire) => {
         // SAFETY: Phoenix passed the store the count came from.
         count.saturating_sub(unsafe { requests(store) })
      },
      _ => count,
   }
}

/// Takes the friend requests out of the Home badge the original returned and
/// answers it as `state | count << 32`.
extern "C" fn home_badge(store: *const u8, state: u32, count: u32) -> u64 {
   let (state, count) = if state == STATE_COUNT {
      let count = without_friend_requests(store, count);
      // Phoenix looks for unread DMs only when it counts nothing, so a dot
      // they would show is lost when only friend requests were counted.
      (if count == 0 { STATE_NONE } else { STATE_COUNT }, count)
   } else {
      (state, count)
   };
   u64::from(state) | u64::from(count) << 32
}

/// Runs the Home badge and hands the `(state, count)` it returns in `eax` and
/// `edx`, which no stable Rust ABI returns, through [`home_badge`].
#[cfg(windows)]
#[unsafe(naked)]
extern "C" fn home_badge_shim() {
   core::arch::naked_asm!(
      "push rbx",
      "sub rsp, 32",
      "mov rbx, rcx",
      "call qword ptr [rip + {original}]",
      "mov rcx, rbx",
      "mov r8d, edx",
      "mov edx, eax",
      "call {adjust}",
      "mov rdx, rax",
      "shr rdx, 32",
      "add rsp, 32",
      "pop rbx",
      "ret",
      original = sym HOME_BADGE,
      adjust = sym home_badge,
   );
}

/// [`home_badge_shim`] for the System V ABI, which passes the count in `edx`
/// already.
#[cfg(not(windows))]
#[unsafe(naked)]
extern "C" fn home_badge_shim() {
   core::arch::naked_asm!(
      "push rbx",
      "mov rbx, rdi",
      "call qword ptr [rip + {original}]",
      "mov rdi, rbx",
      "mov esi, eax",
      "call {adjust}",
      "mov rdx, rax",
      "shr rdx, 32",
      "pop rbx",
      "ret",
      original = sym HOME_BADGE,
      adjust = sym home_badge,
   );
}
