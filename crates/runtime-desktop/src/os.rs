//! The few places Desktop Phoenix differs between Windows, Linux and macOS.
//! Every other file in this crate is the same on all three.

use phoenix_hook::image::Image;

/// What this crate needs from the OS it runs on, picked at build time as
/// [`Os`].
pub trait DesktopOs {
   /// Hooks what [`Self::force_redraw`] needs, from `prepare` before
   /// Discord's own code runs.
   fn install(image: &Image<'static>);

   /// Makes every visible GPUI window in this process render a frame soon,
   /// from any thread. `validation_offset` is where GPUI's Windows window
   /// state keeps the token a forced update has to carry, 0 when not found.
   fn force_redraw(validation_offset: usize);

   /// The calling thread's id as the OS and debuggers show it.
   #[cfg_attr(
      not(any(windows, target_os = "linux")),
      expect(
         dead_code,
         reason = "phoenix.native.hook, its one reader, runs on Windows and Linux"
      )
   )]
   fn current_thread_id() -> u32;

   /// Where `image` is mapped, so addresses print as the offsets `sigscan`
   /// shows for the file.
   fn image_base(image: &Image<'_>) -> usize;

   /// Copies `len` bytes of this process's memory at `addr`, or the readable
   /// start of them. Unmapped memory gives `None` instead of a fault.
   fn read_memory(addr: usize, len: usize) -> Option<Vec<u8>>;
}

#[cfg(target_os = "linux")]
pub use linux::Linux as Os;
#[cfg(target_os = "macos")]
pub use macos::Macos as Os;
#[cfg(windows)]
pub use windows::Windows as Os;

#[cfg(windows)]
mod windows {
   use std::ffi::c_void;

   use phoenix_hook::{image::Image, x86};

   use super::DesktopOs;

   /// GPUI's `WM_GPUI_FORCE_UPDATE_WINDOW`, `WM_USER + 5`.
   const WM_GPUI_FORCE_UPDATE_WINDOW: u32 = 0x0405;
   const GWLP_USERDATA: i32 = -21;

   #[link(name = "kernel32")]
   unsafe extern "system" {
      fn GetCurrentThreadId() -> u32;
   }

   #[link(name = "user32")]
   unsafe extern "system" {
      fn EnumWindows(cb: unsafe extern "system" fn(isize, isize) -> i32, lparam: isize) -> i32;
      fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
      fn IsWindowVisible(hwnd: isize) -> i32;
      fn GetClassNameW(hwnd: isize, buf: *mut u16, max_count: i32) -> i32;
      fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
      fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
      fn InvalidateRect(hwnd: isize, rect: *const c_void, erase: i32) -> i32;
   }

   pub struct Windows;

   impl DesktopOs for Windows {
      fn install(_image: &Image<'static>) {}

      fn force_redraw(validation_offset: usize) {
         let target = (std::process::id(), validation_offset);
         // SAFETY: `redraw_zed_window` reads `target` only during this call.
         unsafe { EnumWindows(redraw_zed_window, (&raw const target).addr().cast_signed()) };
      }

      fn current_thread_id() -> u32 {
         // SAFETY: no preconditions.
         unsafe { GetCurrentThreadId() }
      }

      fn image_base(image: &Image<'_>) -> usize {
         image.pdata.map_or(0, |(base, ..)| base)
      }

      fn read_memory(addr: usize, len: usize) -> Option<Vec<u8>> {
         x86::read_memory(addr, len)
      }
   }

   /// Posts GPUI's forced update with its validated window token to each of
   /// this process's visible `Zed::Window`s, which redraws from any thread
   /// where `InvalidateRect` alone waits for the next input.
   unsafe extern "system" fn redraw_zed_window(hwnd: isize, lparam: isize) -> i32 {
      // SAFETY: `lparam` is the `(pid, validation_offset)` `force_redraw`
      // passed.
      let (want_pid, val_off) = unsafe { *(lparam as *const (u32, usize)) };
      let mut pid = 0u32;
      // SAFETY: Win32 window queries for this process's own visible
      // `Zed::Window`s, whose user data is GPUI's `Rc<WindowsWindowInner>`.
      unsafe {
         GetWindowThreadProcessId(hwnd, &raw mut pid);
         if pid != want_pid || IsWindowVisible(hwnd) == 0 {
            return 1;
         }
         if val_off != 0 {
            let mut cls = [0u16; 32];
            let n = GetClassNameW(hwnd, cls.as_mut_ptr(), 32).cast_unsigned() as usize;
            if cls
               .get(..n)
               .is_some_and(|name| String::from_utf16_lossy(name) == "Zed::Window")
            {
               // This runs off GPUI's thread, which may be freeing the window,
               // so the words behind the user data are copied, not
               // dereferenced.
               let user_data = GetWindowLongPtrW(hwnd, GWLP_USERDATA).cast_unsigned();
               let word = |at: usize| {
                  x86::read_memory(at, 8)
                     .and_then(|bytes| bytes.try_into().ok())
                     .map(usize::from_le_bytes)
               };
               let pointer =
                  |at: usize| word(at).filter(|&value| value != 0 && value != usize::MAX);
               if let Some(rc) = pointer(user_data)
                  && pointer(rc).is_some()
                  && let Some(validation) = rc.checked_add(val_off).and_then(word)
               {
                  PostMessageW(hwnd, WM_GPUI_FORCE_UPDATE_WINDOW, validation, 0);
                  return 1;
               }
            }
         }
         InvalidateRect(hwnd, std::ptr::null(), 0);
      }
      1
   }
}

#[cfg(target_os = "linux")]
mod linux {
   use std::{
      sync::{
         Mutex, OnceLock, PoisonError,
         atomic::{AtomicU8, AtomicU64, Ordering},
      },
      time::Instant,
   };

   use phoenix_hook::{image::Image, x86};
   use phoenix_sigs::desktop;

   use super::DesktopOs;

   /// `WaylandWindowStatePtr::frame(&window, requested, force)`.
   type FrameFn = unsafe fn(*const usize, usize, usize, usize) -> usize;
   /// calloop's `Ping::ping(&ping)`.
   type PingFn = unsafe fn(*const usize);

   struct Frame {
      orig: FrameFn,
      force_render: usize,
      ping: PingFn,
   }

   /// A window's `status`, `ping` and `requested`, each holding one extra
   /// reference so they outlive the window.
   #[derive(Clone, Copy, PartialEq, Eq)]
   struct Window {
      status: usize,
      ping: usize,
      requested: usize,
   }

   /// `WaylandWindow`'s `status`: idle, requested, in flight, and in flight
   /// with another frame wanted.
   const IDLE: u8 = 7;
   const REQUESTED: u8 = 5;
   const IN_FLIGHT: u8 = 1;
   const AGAIN: u8 = 2;
   /// Where an `Rc`'s or `Arc`'s value starts, after its two counts.
   const RC_VALUE: usize = 0x10;
   /// A few frames at 60 Hz, so every window renders one forced frame.
   const FORCE_MS: u64 = 100;

   static FRAME: OnceLock<Frame> = OnceLock::new();
   static WINDOWS: Mutex<Vec<Window>> = Mutex::new(Vec::new());
   static EPOCH: OnceLock<Instant> = OnceLock::new();
   static FORCE_UNTIL: AtomicU64 = AtomicU64::new(0);

   fn now_ms() -> u64 {
      u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
   }

   /// Runs on GPUI's main thread for every frame of every window. While a
   /// forced redraw is due it sets the window's `force_render`, which
   /// re-renders every view, cached ones included.
   unsafe fn frame_detour(window: *const usize, requested: usize, b: usize, c: usize) -> usize {
      let Some(frame) = FRAME.get() else {
         return 0;
      };
      // SAFETY: `window` is the 5-word `WaylandWindow` `frame` was called on.
      let [state, _, status, ping, requested_cell] =
         unsafe { std::ptr::read(window.cast::<[usize; 5]>()) };
      if now_ms() < FORCE_UNTIL.load(Ordering::Acquire) {
         // SAFETY: `state` is the live `Rc<RefCell<WaylandWindowState>>`,
         // unborrowed until `frame` runs.
         unsafe { *((state + frame.force_render) as *mut u8) = 1 };
      }
      remember(Window {
         status,
         ping,
         requested: requested_cell,
      });
      // SAFETY: forwards `frame`'s own arguments.
      unsafe { (frame.orig)(window, requested, b, c) }
   }

   fn remember(window: Window) {
      let mut windows = WINDOWS.lock().unwrap_or_else(PoisonError::into_inner);
      if windows.contains(&window) {
         return;
      }
      // ponytail: a closed window's two small cells and ping stay allocated,
      // one set per window ever opened.
      for counted in [window.status, window.ping, window.requested] {
         // SAFETY: each is a live `Rc` or `Arc` whose strong count leads it;
         // GPUI's main thread, this one, is the only other user of the `Rc`s.
         unsafe { AtomicU64::from_ptr(counted as *mut u64).fetch_add(1, Ordering::Relaxed) };
      }
      windows.push(window);
   }

   /// What `WaylandWindow::request_frame` does, with the status moves made
   /// atomic since this runs off GPUI's main thread.
   ///
   /// ponytail: GPUI moves the same status with plain loads and stores at the
   /// end of a frame, so a request landing between them is lost and the modal
   /// waits for the next input; owning the frame state would close that.
   fn request_frame(frame: &Frame, window: Window) {
      // SAFETY: `remember` keeps `status` alive; GPUI stores its `Cell<u8>`
      // byte-wise.
      let status = unsafe { AtomicU8::from_ptr((window.status + RC_VALUE) as *mut u8) };
      if status
         .compare_exchange(IDLE, REQUESTED, Ordering::AcqRel, Ordering::Acquire)
         .is_err()
      {
         let _ = status.compare_exchange(IN_FLIGHT, AGAIN, Ordering::AcqRel, Ordering::Acquire);
         return;
      }
      let mut now = libc::timespec {
         tv_sec: 0,
         tv_nsec: 0,
      };
      // SAFETY: `now` is a valid out pointer; `Instant` reads this clock.
      unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) };
      let requested = window.requested + RC_VALUE;
      // SAFETY: `remember` keeps `requested`, a `Cell<Option<Instant>>`, alive.
      unsafe {
         *(requested as *mut i64) = now.tv_sec;
         *((requested + 8) as *mut u32) = u32::try_from(now.tv_nsec).unwrap_or(0);
      }
      let ping = window.ping;
      // SAFETY: `ping` is the `Ping`'s one word, its `Arc`, kept alive by
      // `remember`; pinging is calloop's cross-thread wake-up.
      unsafe { (frame.ping)(&raw const ping) };
   }

   pub struct Linux;

   impl DesktopOs for Linux {
      fn install(image: &Image<'static>) {
         let found = match desktop::wayland_frame(image) {
            Ok(found) => found,
            Err(err) => {
               log::warn!("desktop redraw: {err}, plugin UI waits for the next frame");
               return;
            },
         };
         let prologue = desktop::Prologues::of(x86::Abi::of(image)).frame;
         // SAFETY: from `prepare`, before Discord's threads run; the detour
         // has `frame`'s signature.
         let trampoline = match unsafe {
            image.hook_entry(
               found.frame,
               prologue.pattern,
               prologue.len,
               frame_detour as *const () as usize,
            )
         } {
            Ok(trampoline) => trampoline,
            Err(err) => {
               log::warn!("desktop redraw: {err}");
               return;
            },
         };
         // SAFETY: both are functions of these signatures, located by sigs.
         let _ = FRAME.set(unsafe {
            Frame {
               orig: std::mem::transmute::<usize, FrameFn>(trampoline),
               force_render: found.force_render,
               ping: std::mem::transmute::<usize, PingFn>(found.ping),
            }
         });
         log::info!("desktop redraw: hooked Wayland frame @ {:#x}", found.frame);
      }

      fn force_redraw(_validation_offset: usize) {
         let Some(frame) = FRAME.get() else {
            return;
         };
         FORCE_UNTIL.store(now_ms() + FORCE_MS, Ordering::Release);
         let windows = WINDOWS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
         for window in windows {
            request_frame(frame, window);
         }
      }

      fn current_thread_id() -> u32 {
         // SAFETY: no preconditions.
         unsafe { libc::gettid() }.cast_unsigned()
      }

      fn image_base(image: &Image<'_>) -> usize {
         // The first PT_LOAD of a PIE maps file offset 0 at the load bias.
         image
            .segments
            .iter()
            .map(|segment| segment.addr)
            .min()
            .map_or(0, |addr| addr & !0xFFF)
      }

      fn read_memory(addr: usize, len: usize) -> Option<Vec<u8>> {
         x86::read_memory(addr, len)
      }
   }
}

#[cfg(target_os = "macos")]
mod macos {
   use phoenix_hook::{image::Image, x86};

   use super::DesktopOs;

   pub struct Macos;

   impl DesktopOs for Macos {
      /// Points Discord's relaunch after an update at the launcher in
      /// `$RUBYCORD`, set by the copy's Info.plist, since the updated bundle is
      /// signed by Discord again and ignores the loader.
      fn install(image: &Image<'static>) {
         const SCRIPT: &[u8] =
            br#"while kill -0 $0 2>&-;do sleep .2;done;/usr/bin/open -n "${RUBYCORD:-$1}";rm -rf "$2""#;

         let at = match phoenix_sigs::macos::relaunch_script(image) {
            Ok(at) => at,
            Err(err) => {
               log::warn!("relaunch: {err}, an update restarts without RubyCord");
               return;
            },
         };
         let mut script = SCRIPT.to_vec();
         script.resize(phoenix_sigs::macos::RELAUNCH_SCRIPT.len(), b' ');
         // SAFETY: a string no thread reads while the loader's constructor
         // runs, replaced by one of the same length.
         match unsafe { image.write_code(at, &script) } {
            Ok(()) => log::info!("relaunch: Discord's updater restarts through RubyCord"),
            Err(err) => log::warn!("relaunch: {err}"),
         }
      }

      fn force_redraw(_validation_offset: usize) {
         phoenix_ui_desktop::redraw_windows();
      }

      fn current_thread_id() -> u32 {
         let mut id = 0u64;
         // SAFETY: a null thread is the calling one, and `id` is a valid out
         // pointer.
         unsafe { libc::pthread_threadid_np(0, &raw mut id) };
         #[expect(
            clippy::cast_possible_truncation,
            reason = "thread ids count up from boot and stay far below 2^32"
         )]
         let id = id as u32;
         id
      }

      fn image_base(image: &Image<'_>) -> usize {
         // The Mach-O header, so offsets match the file's `vmaddr -
         // 0x1_0000_0000`.
         let Some(inside) = image.segments.first().map(|segment| segment.addr) else {
            return 0;
         };
         let mut info = libc::Dl_info {
            dli_fname: std::ptr::null(),
            dli_fbase: std::ptr::null_mut(),
            dli_sname: std::ptr::null(),
            dli_saddr: std::ptr::null_mut(),
         };
         // SAFETY: dladdr fills `info` for an address inside a loaded image.
         if unsafe { libc::dladdr(inside as *const libc::c_void, &raw mut info) } == 0 {
            return 0;
         }
         info.dli_fbase.addr()
      }

      fn read_memory(addr: usize, len: usize) -> Option<Vec<u8>> {
         x86::read_memory(addr, len)
      }
   }
}
