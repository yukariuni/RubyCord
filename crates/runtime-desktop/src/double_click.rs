//! Double clicks on message rows, for `phoenix.ui.onEvent("rowDoubleTap")`.
//! A handler that calls `ev.press(action)` turns the click into the right
//! click that opens the row's menu, and the menu presses that item for it.

use std::{
   ffi::c_void,
   ptr,
   sync::{
      Mutex, PoisonError,
      atomic::{AtomicBool, Ordering},
   },
   time::{Duration, Instant},
};

use crate::{
   gpui::{self, RawAnyElement, RcFnVtbl},
   modal,
};

/// `gpui::MouseDownEvent`, measured on build 2233: `click_count` and the
/// `MouseButton` byte, where `Left` is 2 and `Right` 3 past the two
/// `Navigate` niches.
const CLICK_COUNT: usize = 0x8;
const BUTTON: usize = 0x10;
const LEFT: u8 = 2;
const RIGHT: u8 = 3;
const PRESS_WINDOW: Duration = Duration::from_secs(2);

static ACTIVE: AtomicBool = AtomicBool::new(false);
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

struct Pending {
   message: u64,
   item: String,
   at: Instant,
}

static ROW_PRESS_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: noop_drop as *const c_void,
   size: 0,
   align: 1,
   call_once: row_press as *const c_void,
   call_mut: row_press as *const c_void,
   call: row_press as *const c_void,
   extra: ptr::null(),
};

const unsafe extern "C" fn noop_drop(_data: *mut u8) {}

/// Whether a plugin listens for double clicks.
pub fn set_active(active: bool) {
   ACTIVE.store(active, Ordering::Release);
}

/// Wraps the row `MessageRow::render` returned in `sret` in a div that
/// listens for presses on message `message`. The listener is zero-sized, so
/// the id rides in its data pointer, which GPUI never reads or frees.
pub fn wrap_row(sret: *mut RawAnyElement, message: u64) {
   if !ACTIVE.load(Ordering::Acquire) || message == 0 {
      return;
   }
   let Some(mut div) = gpui::v_stack(0.0) else {
      return;
   };
   // SAFETY: `sret` holds the rendered row, and gets it back or the wrapper.
   let mut row = unsafe { ptr::read(sret) };
   if !gpui::push_any(&mut div, &mut row) {
      // SAFETY: the row was not moved into the div.
      unsafe { ptr::write(sret, row) };
      return;
   }
   modal::listen_mouse_down(&mut div, message as *mut u8, &ROW_PRESS_VTBL);
   gpui::write_any(div, sret);
}

/// The menu item to press on `message`'s menu, when a double click asked for
/// one just now.
pub fn take_press(message: u64) -> Option<String> {
   let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
   if pending
      .as_ref()
      .is_none_or(|held| held.message != message || held.at.elapsed() > PRESS_WINDOW)
   {
      return None;
   }
   pending.take().map(|held| held.item)
}

/// Whether a press waits for a menu.
pub fn pending() -> bool {
   PENDING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .is_some()
}

/// Rust ABI like the `Fn::call` slot it fills, see `modal::scrim_press`. The
/// capture phase reaches this wrapper before the row's own bubble listener,
/// which opens the menu on a right press.
unsafe fn row_press(
   data: *const u8,
   event: *mut u8,
   phase: u8,
   hitbox: *const c_void,
   window: *const c_void,
   _app: *mut u8,
) {
   let _ = std::panic::catch_unwind(|| {
      if phase != modal::PHASE_CAPTURE || !modal::hovered(hitbox, window) {
         return;
      }
      // SAFETY: GPUI passes the live `MouseDownEvent`.
      let (count, button) = unsafe {
         (
            event.add(CLICK_COUNT).cast::<u64>().read_unaligned(),
            event.add(BUTTON).read(),
         )
      };
      if count != 2 || button != LEFT {
         return;
      }
      let message = data as u64;
      let Some(item) =
         phoenix_runtime::engine().and_then(|engine| crate::js::ui::double_tap(&engine, message))
      else {
         return;
      };
      *PENDING.lock().unwrap_or_else(PoisonError::into_inner) = Some(Pending {
         message,
         item,
         at: Instant::now(),
      });
      // SAFETY: the event GPUI hands every listener in turn, on this stack.
      unsafe { event.add(BUTTON).write(RIGHT) };
   });
}
