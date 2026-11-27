//! Phoenix's guest/host bridge, observed without inline hooks.
//!
//! - The guest calls the host with frames, and a frame that starts with 4 zero
//!   bytes is a UI-op batch. Android carries them through `GuestHost.invoke`,
//!   iOS through the host table each guest thread holds.
//! - The host dispatches view and host events to the guest, through
//!   `WasmRuntime.nativeDispatch` on Android and the guest's vtable on iOS.
//!
//! Both directions run through the sinks here. [`jni`] and [`ios`] only move
//! the bytes.

#[cfg(target_os = "ios")] pub mod ios;
#[cfg(target_os = "android")] pub mod jni;
use std::{
   panic::{
      AssertUnwindSafe,
      catch_unwind,
   },
   sync::{
      Mutex,
      MutexGuard,
      PoisonError,
      RwLock,
      atomic::{
         AtomicBool,
         AtomicU64,
         Ordering,
      },
   },
};

#[cfg(target_os = "ios")] use ios as backend;
#[cfg(target_os = "android")] use jni as backend;

use crate::discord;

/// What to do with an observed frame.
pub enum Action {
   Pass,
   Drop,
   Replace(Vec<u8>),
}

/// Receives every UI-op batch Rust sends and every view event the host
/// dispatches. Sinks run in install order, each on what the one before it
/// left.
pub trait Sink: Send + Sync {
   /// `batch` is a version byte followed by ops.
   fn ops(&self, batch: &[u8]) -> Action;

   /// [`Action::Drop`] reports the frame as handled to the host.
   fn dispatch(&self, frame: &[u8]) -> Action;
}

static SINKS: RwLock<Vec<Box<dyn Sink>>> = RwLock::new(Vec::new());
static WATCHING: AtomicBool = AtomicBool::new(false);
static HAPTICS_DISABLED: AtomicBool = AtomicBool::new(false);
static HAPTICS_LOCKS: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());
/// The host's `UIManager` parses a batch on the caller's thread, so Rust's
/// batches and [`apply_ops`] share this lock. It is held while the sinks run,
/// so a follow-up batch a sink queues lands after the batch that made its
/// parents.
static UI_LOCK: Mutex<()> = Mutex::new(());
/// Dispatch frames queued from other threads, such as async
/// `phoenix.discord.onResponse` replies, delivered on the guest thread.
static DEFERRED: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

pub fn install(sink: Box<dyn Sink>) {
   SINKS
      .write()
      .unwrap_or_else(PoisonError::into_inner)
      .push(sink);
   WATCHING.store(true, Ordering::Release);
}

/// Whether any sink is installed, so the backends can skip reading frames.
fn watching() -> bool {
   WATCHING.load(Ordering::Acquire)
}

/// Runs `bytes` through every sink. A sink that panics leaves the bytes as the
/// sink before it left them.
fn consult(bytes: &[u8], ask: impl Fn(&dyn Sink, &[u8]) -> Action) -> Action {
   let sinks = SINKS.read().unwrap_or_else(PoisonError::into_inner);
   let mut replaced: Option<Vec<u8>> = None;
   for sink in sinks.iter() {
      let current = replaced.as_deref().unwrap_or(bytes);
      let action =
         catch_unwind(AssertUnwindSafe(|| ask(sink.as_ref(), current))).unwrap_or_else(|_| {
            log::warn!("bridge: a sink panicked, the frame went on unmodified");
            Action::Pass
         });
      match action {
         Action::Pass => {},
         Action::Drop => return Action::Drop,
         Action::Replace(bytes) => replaced = Some(bytes),
      }
   }
   drop(sinks);
   replaced.map_or(Action::Pass, Action::Replace)
}

/// Whether any plugin currently suppresses haptic feedback.
pub fn haptics_disabled() -> bool {
   HAPTICS_DISABLED.load(Ordering::Acquire)
}

/// Suppresses haptic feedback until [`enable_haptics`] or [`forget_haptics`].
pub fn disable_haptics(plugin: String) -> u64 {
   static NEXT: AtomicU64 = AtomicU64::new(1);
   let handle = NEXT.fetch_add(1, Ordering::Relaxed);
   haptics_locks(|locks| locks.push((handle, plugin)));
   handle
}

/// Releases one [`disable_haptics`] handle.
pub fn enable_haptics(handle: u64) {
   haptics_locks(|locks| locks.retain(|(id, _)| *id != handle));
}

/// Releases every [`disable_haptics`] handle owned by `plugin`.
pub fn forget_haptics(plugin: &str) {
   haptics_locks(|locks| locks.retain(|(_, owner)| owner != plugin));
}

fn haptics_locks(change: impl FnOnce(&mut Vec<(u64, String)>)) {
   let mut locks = HAPTICS_LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
   change(&mut locks);
   HAPTICS_DISABLED.store(!locks.is_empty(), Ordering::Release);
}

/// A guest-to-host call that is not a UI batch. `frame` reads the whole frame,
/// which only Discord's requests need.
fn host_call(head: &[u8], frame: impl FnOnce() -> Vec<u8>) -> Action {
   if phoenix_runtime::remote::enabled()
      && let Some((module, method)) = phoenix_wire::HostCall::target(head)
   {
      phoenix_runtime::remote::record(&crate::debug::Trace::HostCall { module, method });
   }
   if haptics_disabled() && crate::host::is_module(head, crate::host::Module::Haptics) {
      return Action::Drop;
   }
   if discord::wants(head) {
      discord::intercept_call(&frame())
   } else {
      Action::Pass
   }
}

/// A guest-to-host UI batch without its 4 zero bytes, and the lock to hold
/// until the host has taken it.
fn ops(batch: &[u8]) -> (Action, MutexGuard<'static, ()>) {
   let guard = UI_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
   if !watching() {
      return (Action::Pass, guard);
   }
   (consult(batch, |sink, batch| sink.ops(batch)), guard)
}

/// A host-to-guest dispatch frame, through Discord's replies, host events and
/// the sinks.
fn dispatched(payload: &[u8]) -> Action {
   let mut replaced = match discord::filter(payload) {
      Action::Drop => return Action::Drop,
      Action::Pass => None,
      Action::Replace(rest) => Some(rest),
   };
   match crate::host::filter(replaced.as_deref().unwrap_or(payload)) {
      Action::Drop => return Action::Drop,
      Action::Pass => {},
      Action::Replace(rest) => replaced = Some(rest),
   }
   if watching() {
      match consult(replaced.as_deref().unwrap_or(payload), |sink, frame| {
         sink.dispatch(frame)
      }) {
         Action::Drop => return Action::Drop,
         Action::Pass => {},
         Action::Replace(rest) => replaced = Some(rest),
      }
   }
   replaced.map_or(Action::Pass, Action::Replace)
}

fn take_deferred() -> Vec<Vec<u8>> {
   std::mem::take(&mut *DEFERRED.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Whether the guest has called the host yet, which [`invoke`] needs.
pub fn ready() -> bool {
   backend::ready()
}

/// Sends a UI-op batch (version byte and ops) to the host's `UIManager`.
///
/// Returns false until the guest has called the host once. Never call it on
/// the main thread, the guest may hold the UI lock while it waits on that
/// thread.
pub fn apply_ops(batch: &[u8]) -> bool {
   let mut frame = vec![0; 4];
   frame.extend_from_slice(batch);
   let _guard = UI_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
   invoke(&frame)
}

/// Passes `frame` to the host as if the guest had sent it, and drops whatever
/// it returns. Same conditions as [`apply_ops`].
pub fn invoke(frame: &[u8]) -> bool {
   backend::invoke(frame)
}

/// Queues view `frame` for the guest as if the host had dispatched it. The
/// frame still passes the sinks. Same conditions as [`apply_ops`].
pub fn inject(frame: &[u8]) -> bool {
   backend::inject(frame)
}

/// Queues a raw dispatch frame, such as a rewritten `@http` reply, for the
/// guest thread.
pub fn dispatch_deferred(frame: Vec<u8>) {
   DEFERRED
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push(frame);
   backend::wake();
}
