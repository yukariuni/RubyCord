//! Lightweight Discord main-window subclass for Desktop Phoenix.
//!
//! Subclasses Discord's main GPUI `HWND` so pressing `Ctrl+Shift+I` while
//! Discord Phoenix is focused opens the Ruby `DevTools` / Runtime Console
//! without registering a global hotkey that would steal `Ctrl+Shift+I` from
//! browsers or editors.

use std::{
   ffi::c_void,
   ptr,
   sync::atomic::{AtomicBool, AtomicUsize, Ordering},
   thread,
   time::Duration,
};

const WM_NCDESTROY: u32 = 0x0082;
const WM_KEYDOWN: u32 = 0x0100;
const WM_SYSKEYDOWN: u32 = 0x0104;

const VK_CONTROL: i32 = 0x11;
const VK_SHIFT: i32 = 0x10;
const VK_MENU: i32 = 0x12;

const SUBCLASS_ID: usize = 0x5342_4454; // "SBDT"

static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);
static DISCORD_HWND: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
   left: i32,
   top: i32,
   right: i32,
   bottom: i32,
}

#[link(name = "user32")]
unsafe extern "system" {
   fn EnumWindows(cb: unsafe extern "system" fn(*mut c_void, isize) -> i32, lparam: isize) -> i32;
   fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
   fn IsWindow(hwnd: *mut c_void) -> i32;
   fn IsWindowVisible(hwnd: *mut c_void) -> i32;
   fn GetClientRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
   fn GetKeyState(vk: i32) -> i16;
}

#[link(name = "comctl32")]
unsafe extern "system" {
   fn SetWindowSubclass(
      hwnd: *mut c_void,
      proc: unsafe extern "system" fn(*mut c_void, u32, usize, isize, usize, usize) -> isize,
      id: usize,
      ref_data: usize,
   ) -> i32;
   fn RemoveWindowSubclass(
      hwnd: *mut c_void,
      proc: unsafe extern "system" fn(*mut c_void, u32, usize, isize, usize, usize) -> isize,
      id: usize,
   ) -> i32;
   fn DefSubclassProc(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
}

#[link(name = "kernel32")]
unsafe extern "system" {
   fn GetCurrentProcessId() -> u32;
}

unsafe extern "system" fn discord_subclass_proc(
   hwnd: *mut c_void,
   msg: u32,
   wparam: usize,
   lparam: isize,
   _uid: usize,
   _ref_data: usize,
) -> isize {
   // SAFETY: Win32 subclass callback executing on Discord's UI thread.
   unsafe {
      match msg {
         WM_KEYDOWN | WM_SYSKEYDOWN => {
            let is_repeat = (lparam & 0x4000_0000) != 0;
            if wparam == usize::from(b'I') && !is_repeat {
               let ctrl = GetKeyState(VK_CONTROL) < 0;
               let shift = GetKeyState(VK_SHIFT) < 0;
               let alt = GetKeyState(VK_MENU) < 0;
               if ctrl && shift && !alt {
                  crate::open_devtools();
                  return 0;
               }
            }
         },
         WM_NCDESTROY => {
            DISCORD_HWND.store(0, Ordering::Release);
            RemoveWindowSubclass(hwnd, discord_subclass_proc, SUBCLASS_ID);
         },
         _ => {},
      }
      DefSubclassProc(hwnd, msg, wparam, lparam)
   }
}

struct FindCtx {
   pid: u32,
   hwnd: *mut c_void,
}

unsafe extern "system" fn enum_discord_windows(hwnd: *mut c_void, lparam: isize) -> i32 {
   // SAFETY: `lparam` points to `FindCtx` on `find_discord_hwnd`'s stack.
   unsafe {
      let ctx = &mut *(lparam as *mut FindCtx);
      let mut pid = 0u32;
      GetWindowThreadProcessId(hwnd, &raw mut pid);
      if pid != ctx.pid || IsWindowVisible(hwnd) == 0 {
         return 1;
      }
      let mut rect = Rect {
         left: 0,
         top: 0,
         right: 0,
         bottom: 0,
      };
      GetClientRect(hwnd, &raw mut rect);
      if rect.right - rect.left >= 300 && rect.bottom - rect.top >= 200 {
         ctx.hwnd = hwnd;
         return 0;
      }
      1
   }
}

fn find_discord_hwnd() -> *mut c_void {
   // SAFETY: Win32 process ID and EnumWindows with stack-owned `FindCtx`.
   unsafe {
      let mut ctx = FindCtx {
         pid: GetCurrentProcessId(),
         hwnd: ptr::null_mut(),
      };
      EnumWindows(enum_discord_windows, (&raw mut ctx) as isize);
      ctx.hwnd
   }
}

fn ensure_subclassed() -> *mut c_void {
   let current = DISCORD_HWND.load(Ordering::Acquire) as *mut c_void;
   // SAFETY: `IsWindow` safely validates any HWND handle.
   if !current.is_null() && unsafe { IsWindow(current) } != 0 {
      return current;
   }
   let found = find_discord_hwnd();
   if found.is_null() {
      return ptr::null_mut();
   }
   // SAFETY: attaches subclass to Discord's live top-level HWND.
   if unsafe { SetWindowSubclass(found, discord_subclass_proc, SUBCLASS_ID, 0) } != 0 {
      DISCORD_HWND.store(found as usize, Ordering::Release);
      log::info!("devtools: attached Ctrl+Shift+I shortcut to Discord window ({found:p})");
      found
   } else {
      ptr::null_mut()
   }
}

/// Starts a lightweight background watcher that attaches our subclass to
/// Discord's main window as soon as GPUI opens it, so `Ctrl+Shift+I` works
/// immediately.
pub fn start_window_hook() {
   if WATCHER_STARTED.swap(true, Ordering::AcqRel) {
      return;
   }
   let _ = thread::Builder::new()
      .name("ruby-hwnd".to_owned())
      .spawn(|| {
         // Keeps going: the window can be closed and opened again, and Discord can set its own
         // icon back.
         loop {
            let hwnd = ensure_subclassed();
            if !hwnd.is_null() {
               crate::windows::set_window_icons(hwnd);
            }
            thread::sleep(Duration::from_millis(if hwnd.is_null() {
               250
            } else {
               2000
            }));
         }
      });
}
