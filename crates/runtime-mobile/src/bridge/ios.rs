//! The iOS side of the bridge, where the Swift host and the Rust guest share
//! `PhoenixFatClient` and talk through two vtables in its data.
//!
//! - The host hands the guest every view and host event through `dispatch(data,
//!   bytes, len, out)` in the guest's vtable.
//! - The guest makes every host call, UI batches included, through `call(data,
//!   bytes, len)` in the vtable of the host's callbacks.
//!
//! [`start`] swaps both slots before the guest exists, so phoenix sees the
//! first frame either way. The host has no queue onto the guest thread that
//! Rust can reach, so injected and deferred frames wait for the next dispatch,
//! which delivers them after its own frame. [`wake`] asks for one.

use std::{
   ffi::c_void,
   mem::transmute,
   panic::{
      AssertUnwindSafe,
      catch_unwind,
   },
   ptr,
   slice,
   sync::{
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicPtr,
         Ordering,
      },
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::ios;
use phoenix_wire::{
   Value,
   encode_batch,
};

use crate::{
   bridge::{
      self,
      Action,
   },
   host::{
      self,
      Event,
      Field,
      Method,
   },
};

type Dispatch = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut c_void) -> *mut c_void;
type HostCall = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> Answer;

/// What a host call returns through `x8`: a byte buffer the guest owns, or a
/// `cap` of [`NO_ANSWER`].
#[repr(C)]
struct Answer {
   cap: isize,
   ptr: *mut u8,
   len: usize,
}

const NO_ANSWER: isize = -1;

static DISPATCH: OnceLock<Dispatch> = OnceLock::new();
static HOST_CALL: OnceLock<HostCall> = OnceLock::new();
/// The host callbacks' `data`, the first argument of every host call.
static CALLBACKS: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
/// View frames for the guest, each wrapped in a kind-4 batch.
static INJECTED: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
/// UI batches for the host, with their 4 zero bytes. The guest makes every UI
/// call from its own thread, so ours wait for the next dispatch there.
static OUTGOING: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

#[derive(Debug, flub::Error)]
enum BridgeError {
   #[error("{0}")]
   Anchor(String),
   #[error("the slot at {0:#x} is unmapped")]
   Unmapped(usize),
   #[error("the slot at {0:#x} changed while it was swapped")]
   Raced(usize),
   #[error("swapping a vtable slot: {0}")]
   Swap(#[source] phoenix_hook::Error),
}

/// Hooks both vtable slots.
pub fn start(image: &Image<'static>) {
   match hook(image) {
      Ok(()) => log::info!("bridge: hooked the guest and host vtables"),
      Err(err) => log::warn!("bridge: {err}, the UI stays unhooked"),
   }
}

fn hook(image: &Image<'static>) -> Result<(), BridgeError> {
   let found = ios::bridge(image).map_err(BridgeError::Anchor)?;
   swap(
      image,
      found.vtable + found.dispatch,
      dispatch as Dispatch as usize,
      |original| {
         // SAFETY: `ios::bridge` checked that the slot holds code, which the
         // host calls with this signature.
         let _ = DISPATCH.set(unsafe { transmute::<usize, Dispatch>(original) });
      },
   )?;
   swap(
      image,
      found.host_vtable + ios::HOST_CALL,
      host_call as HostCall as usize,
      |original| {
         // SAFETY: as above, for the guest's calls.
         let _ = HOST_CALL.set(unsafe { transmute::<usize, HostCall>(original) });
      },
   )
}

/// Swaps `hook` into the vtable word at `slot`. `keep` gets what the slot held
/// first, since the hook forwards there from its first call.
fn swap(
   image: &Image<'static>,
   slot: usize,
   hook: usize,
   keep: impl FnOnce(usize),
) -> Result<(), BridgeError> {
   let original = image.read_usize(slot).ok_or(BridgeError::Unmapped(slot))?;
   keep(original);
   // SAFETY: the slot is an aligned vtable word in the image, and every caller
   // of it accepts the hook, which forwards to `original`.
   let previous = unsafe { image.swap_words(&[(slot, hook)]) }.map_err(BridgeError::Swap)?;
   match previous[..] {
      [held] if held == original => Ok(()),
      [held] => {
         // SAFETY: as above, putting back what the slot held.
         unsafe { image.swap_words(&[(slot, held)]) }.map_err(BridgeError::Swap)?;
         Err(BridgeError::Raced(slot))
      },
      _ => Err(BridgeError::Raced(slot)),
   }
}

pub fn ready() -> bool {
   HOST_CALL.get().is_some() && !CALLBACKS.load(Ordering::Acquire).is_null()
}

unsafe extern "C" {
   static _dispatch_main_q: c_void;
   fn dispatch_async_f(
      queue: *const c_void,
      context: *mut c_void,
      work: unsafe extern "C" fn(*mut c_void),
   );
}

/// Calls the host as the guest would. UI batches go in on the guest thread at
/// its next dispatch, and every other call goes in on the main queue, since
/// host modules like `Timing` schedule on the calling thread's run loop. An
/// answer, which no frame phoenix sends gets, would leak.
pub fn invoke(frame: &[u8]) -> bool {
   let (Some(&original), callbacks) = (HOST_CALL.get(), CALLBACKS.load(Ordering::Acquire)) else {
      return false;
   };
   if callbacks.is_null() || DISPATCH.get().is_none() {
      return false;
   }
   if frame.starts_with(&[0; 4]) {
      OUTGOING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .push(frame.to_vec());
      wake();
      return true;
   }
   let call = Box::into_raw(Box::new((original, callbacks as usize, frame.to_vec())));
   // SAFETY: the main queue lives as long as the process, and `on_main` takes
   // back the box exactly once.
   unsafe { dispatch_async_f(&raw const _dispatch_main_q, call.cast(), on_main) };
   true
}

unsafe extern "C" fn on_main(call: *mut c_void) {
   // SAFETY: `invoke` leaked this box for this one call.
   let (original, callbacks, frame) =
      *unsafe { Box::from_raw(call.cast::<(HostCall, usize, Vec<u8>)>()) };
   // SAFETY: as in `invoke`.
   unsafe { original(callbacks as *mut c_void, frame.as_ptr(), frame.len()) };
}

/// Queues view `frame` for the next dispatch, which [`wake`] brings on.
pub fn inject(frame: &[u8]) -> bool {
   if DISPATCH.get().is_none() {
      return false;
   }
   INJECTED
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push(encode_batch(&[frame.to_vec()]));
   wake();
   true
}

/// Has the host fire a timer at once, whose dispatch delivers what waits.
/// [`host::poke`] keeps the timer's event from the guest.
pub fn wake() {
   let poked = host::poke(
      Method::TimingSet,
      &[(Field::Delay, Value::I32(0))],
      Event::Timer,
   );
   if let Err(err) = poked {
      log::warn!("bridge: no timer to deliver queued frames: {err}");
   }
}

/// Passes one host-to-guest frame through the sinks, then to the guest.
///
/// # Safety
///
/// Runs on the guest thread, with the guest idle, and `data` and `out` are
/// the host's own arguments.
unsafe fn deliver(
   original: Dispatch,
   data: *mut c_void,
   frame: &[u8],
   out: *mut c_void,
) -> *mut c_void {
   let action =
      catch_unwind(AssertUnwindSafe(|| bridge::dispatched(frame))).unwrap_or(Action::Pass);
   // SAFETY: forwarded from the caller.
   unsafe {
      match action {
         Action::Drop => ptr::null_mut(),
         Action::Pass => original(data, frame.as_ptr(), frame.len(), out),
         Action::Replace(replaced) => original(data, replaced.as_ptr(), replaced.len(), out),
      }
   }
}

/// A null return tells the host the guest took the frame.
unsafe extern "C" fn dispatch(
   data: *mut c_void,
   bytes: *const u8,
   len: usize,
   out: *mut c_void,
) -> *mut c_void {
   let Some(&original) = DISPATCH.get() else {
      return ptr::null_mut();
   };
   if bytes.is_null() {
      // SAFETY: the host's own arguments, passed on unchanged.
      return unsafe { original(data, bytes, len, out) };
   }
   // SAFETY: the host's frame, borrowed for this call.
   let frame = unsafe { slice::from_raw_parts(bytes, len) };
   // SAFETY: the host calls this on the guest thread, so each delivery runs
   // with the guest idle.
   let result = unsafe { deliver(original, data, frame, out) };
   // Deferred frames already passed the filters that queued them.
   for pending in bridge::take_deferred() {
      // SAFETY: as above, the guest returned from the frame before.
      let _ = unsafe { original(data, pending.as_ptr(), pending.len(), out) };
   }
   let injected = std::mem::take(&mut *INJECTED.lock().unwrap_or_else(PoisonError::into_inner));
   for pending in injected {
      // SAFETY: as above.
      let _ = unsafe { deliver(original, data, &pending, out) };
   }
   send_outgoing();
   result
}

/// Hands the host the UI batches [`invoke`] queued. Runs on the guest thread.
fn send_outgoing() {
   let outgoing = std::mem::take(&mut *OUTGOING.lock().unwrap_or_else(PoisonError::into_inner));
   let (Some(&call), callbacks) = (HOST_CALL.get(), CALLBACKS.load(Ordering::Acquire)) else {
      return;
   };
   for batch in outgoing {
      // SAFETY: `call` is the host's call, `callbacks` its own first argument,
      // and the guest thread is where the guest makes the same call.
      unsafe { call(callbacks, batch.as_ptr(), batch.len()) };
   }
}

unsafe extern "C" fn host_call(callbacks: *mut c_void, bytes: *const u8, len: usize) -> Answer {
   let no_answer = Answer {
      cap: NO_ANSWER,
      ptr: ptr::null_mut(),
      len: 0,
   };
   let Some(&original) = HOST_CALL.get() else {
      return no_answer;
   };
   CALLBACKS.store(callbacks, Ordering::Release);
   if bytes.is_null() {
      // SAFETY: the guest's own arguments, passed on unchanged.
      return unsafe { original(callbacks, bytes, len) };
   }
   // SAFETY: the guest's frame, borrowed for this call.
   let frame = unsafe { slice::from_raw_parts(bytes, len) };
   // SAFETY: the guest's own first argument, with a frame that outlives the
   // call.
   let call = |bytes: &[u8]| unsafe { original(callbacks, bytes.as_ptr(), bytes.len()) };
   if let Some(batch) = frame.strip_prefix(&[0; 4]) {
      let (action, _guard) = bridge::ops(batch);
      return match action {
         Action::Drop => no_answer,
         Action::Pass => call(frame),
         Action::Replace(replaced) => {
            let mut prefixed = vec![0; 4];
            prefixed.extend_from_slice(&replaced);
            call(&prefixed)
         },
      };
   }
   let action = catch_unwind(AssertUnwindSafe(|| {
      bridge::host_call(frame, || frame.to_vec())
   }))
   .unwrap_or(Action::Pass);
   match action {
      Action::Drop => no_answer,
      Action::Pass => call(frame),
      Action::Replace(replaced) => call(&replaced),
   }
}
