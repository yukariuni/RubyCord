use std::{
   ffi::c_void,
   path::PathBuf,
   ptr,
   sync::{
      Mutex,
      PoisonError,
      atomic::{
         AtomicBool,
         AtomicU32,
         AtomicUsize,
         Ordering,
      },
   },
   thread,
};

use crate::{
   DesktopHost,
   Error,
   TrayAction,
};

const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;
const SW_SHOWNORMAL: i32 = 1;

const WM_NULL: u32 = 0x0000;
const WM_DESTROY: u32 = 0x0002;
const WM_COMMAND: u32 = 0x0111;
const WM_USER: u32 = 0x0400;
const WM_TRAY: u32 = WM_USER + 1;
const WM_LBUTTONUP: usize = 0x0202;
const WM_LBUTTONDBLCLK: usize = 0x0203;
const WM_RBUTTONUP: usize = 0x0205;
const NIN_BALLOONUSERCLICK: usize = 0x0405;

const NIM_ADD: u32 = 0x0000_0000;
const NIM_MODIFY: u32 = 0x0000_0001;
const NIM_DELETE: u32 = 0x0000_0002;
const NIF_MESSAGE: u32 = 0x0000_0001;
const NIF_ICON: u32 = 0x0000_0002;
const NIF_TIP: u32 = 0x0000_0004;
const NIF_INFO: u32 = 0x0000_0010;
const NIIF_USER: u32 = 0x0000_0004;

const MF_STRING: u32 = 0x0000_0000;
const MF_SEPARATOR: u32 = 0x0000_0800;
const TPM_RIGHTBUTTON: u32 = 0x0002;
const TPM_NONOTIFY: u32 = 0x0080;
const TPM_RETURNCMD: u32 = 0x0100;

const ID_OPEN_DEVTOOLS: usize = 1006;
const ID_RELOAD_ALL: usize = 1002;
const ID_CHECK_UPDATES: usize = 1003;
const ID_COPY_URL: usize = 1004;
const ID_OPEN_PLUGINS: usize = 1005;

const IDI_APPLICATION: usize = 32512;
const TRAY_UID: u32 = 0x5342_5244; // "SBRD"
const BI_RGB: u32 = 0;
const DIB_RGB_COLORS: u32 = 0;

static TRAY_STARTED: AtomicBool = AtomicBool::new(false);
static TRAY_HWND: AtomicUsize = AtomicUsize::new(0);
static TRAY_ICON: AtomicUsize = AtomicUsize::new(0);
static TASKBAR_CREATED_MSG: AtomicU32 = AtomicU32::new(0);
static PENDING_BALLOONS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

#[repr(C)]
struct WndClassExW {
   cb_size:    u32,
   style:      u32,
   wnd_proc:   unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize,
   cls_extra:  i32,
   wnd_extra:  i32,
   instance:   *mut c_void,
   icon:       *mut c_void,
   cursor:     *mut c_void,
   background: *mut c_void,
   menu_name:  *const u16,
   class_name: *const u16,
   icon_sm:    *mut c_void,
}

#[repr(C)]
struct Point {
   x: i32,
   y: i32,
}

#[repr(C)]
struct Msg {
   hwnd:    *mut c_void,
   message: u32,
   wparam:  usize,
   lparam:  isize,
   time:    u32,
   pt:      Point,
}

#[repr(C)]
struct NotifyIconDataW {
   cb_size:              u32,
   hwnd:                 *mut c_void,
   u_id:                 u32,
   u_flags:              u32,
   u_callback_message:   u32,
   h_icon:               *mut c_void,
   sz_tip:               [u16; 128],
   dw_state:             u32,
   dw_state_mask:        u32,
   sz_info:              [u16; 256],
   u_timeout_or_version: u32,
   sz_info_title:        [u16; 64],
   dw_info_flags:        u32,
   guid_item:            [u8; 16],
   h_balloon_icon:       *mut c_void,
}

#[repr(C)]
#[expect(clippy::struct_field_names, reason = "Win32's BITMAPINFOHEADER names")]
struct BitmapInfoHeader {
   bi_size:          u32,
   bi_width:         i32,
   bi_height:        i32,
   bi_planes:        u16,
   bi_bit_count:     u16,
   bi_compression:   u32,
   bi_size_image:    u32,
   bi_x_pels:        i32,
   bi_y_pels:        i32,
   bi_clr_used:      u32,
   bi_clr_important: u32,
}

#[repr(C)]
struct IconInfo {
   f_icon:    i32,
   x_hotspot: u32,
   y_hotspot: u32,
   hbm_mask:  *mut c_void,
   hbm_color: *mut c_void,
}

#[link(name = "user32")]
unsafe extern "system" {
   fn OpenClipboard(hwnd: *mut c_void) -> i32;
   fn CloseClipboard() -> i32;
   fn EmptyClipboard() -> i32;
   fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
   fn RegisterClassExW(cls: *const WndClassExW) -> u16;
   fn RegisterWindowMessageW(name: *const u16) -> u32;
   fn CreateWindowExW(
      ex_style: u32,
      class_name: *const u16,
      window_name: *const u16,
      style: u32,
      x: i32,
      y: i32,
      width: i32,
      height: i32,
      parent: *mut c_void,
      menu: *mut c_void,
      instance: *mut c_void,
      param: *mut c_void,
   ) -> *mut c_void;
   fn DestroyWindow(hwnd: *mut c_void) -> i32;
   fn DefWindowProcW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
   fn GetMessageW(msg: *mut Msg, hwnd: *mut c_void, min: u32, max: u32) -> i32;
   fn TranslateMessage(msg: *const Msg) -> i32;
   fn DispatchMessageW(msg: *const Msg) -> isize;
   fn PostMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
   fn LoadIconW(instance: *mut c_void, name: *const u16) -> *mut c_void;
   fn CreateIconIndirect(info: *mut IconInfo) -> *mut c_void;
   fn DestroyIcon(icon: *mut c_void) -> i32;
   fn CreatePopupMenu() -> *mut c_void;
   fn AppendMenuW(menu: *mut c_void, flags: u32, id: usize, item: *const u16) -> i32;
   fn SetMenuDefaultItem(menu: *mut c_void, item: u32, by_pos: u32) -> i32;
   fn TrackPopupMenu(
      menu: *mut c_void,
      flags: u32,
      x: i32,
      y: i32,
      reserved: i32,
      hwnd: *mut c_void,
      rect: *const c_void,
   ) -> i32;
   fn DestroyMenu(menu: *mut c_void) -> i32;
   fn GetCursorPos(pt: *mut Point) -> i32;
   fn SetForegroundWindow(hwnd: *mut c_void) -> i32;
   fn PostQuitMessage(code: i32);
}

#[link(name = "gdi32")]
unsafe extern "system" {
   fn CreateDIBSection(
      hdc: *mut c_void,
      bmi: *const BitmapInfoHeader,
      usage: u32,
      bits: *mut *mut c_void,
      section: *mut c_void,
      offset: u32,
   ) -> *mut c_void;
   fn CreateBitmap(
      width: i32,
      height: i32,
      planes: u32,
      bit_count: u32,
      bits: *const c_void,
   ) -> *mut c_void;
   fn DeleteObject(obj: *mut c_void) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
   fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
   fn GlobalLock(mem: *mut c_void) -> *mut c_void;
   fn GlobalUnlock(mem: *mut c_void) -> i32;
   fn GlobalFree(mem: *mut c_void) -> *mut c_void;
   fn GetModuleHandleW(name: *const u16) -> *mut c_void;
   fn GetCurrentProcessId() -> u32;
}

#[link(name = "shell32")]
unsafe extern "system" {
   fn ShellExecuteW(
      hwnd: *mut c_void,
      operation: *const u16,
      file: *const u16,
      parameters: *const u16,
      directory: *const u16,
      show_cmd: i32,
   ) -> isize;
   fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconDataW) -> i32;
}

#[repr(C)]
struct OpenFileNameW {
   l_struct_size:       u32,
   hwnd_owner:          *mut c_void,
   h_instance:          *mut c_void,
   lpstr_filter:        *const u16,
   lpstr_custom_filter: *mut u16,
   n_max_cust_filter:   u32,
   n_filter_index:      u32,
   lpstr_file:          *mut u16,
   n_max_file:          u32,
   lpstr_file_title:    *mut u16,
   n_max_file_title:    u32,
   lpstr_initial_dir:   *const u16,
   lpstr_title:         *const u16,
   flags:               u32,
   n_file_offset:       u16,
   n_file_extension:    u16,
   lpstr_def_ext:       *const u16,
   l_cust_data:         isize,
   lpfn_hook:           *const c_void,
   lp_template_name:    *const u16,
   pv_reserved:         *mut c_void,
   dw_reserved:         u32,
   flags_ex:            u32,
}

#[link(name = "comdlg32")]
unsafe extern "system" {
   fn GetOpenFileNameW(ofn: *mut OpenFileNameW) -> i32;
}

const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
const OFN_NOCHANGEDIR: u32 = 0x0000_0008;
const OFN_EXPLORER: u32 = 0x0008_0000;

fn wide(text: &str) -> Vec<u16> {
   text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn fill_wide<const N: usize>(dst: &mut [u16; N], text: &str) {
   dst.fill(0);
   for (slot, unit) in dst
      .iter_mut()
      .take(N.saturating_sub(1))
      .zip(text.encode_utf16())
   {
      *slot = unit;
   }
}

fn os_err(context: &str) -> Error {
   Error::Platform {
      message: format!("{context}: {}", std::io::Error::last_os_error()),
   }
}

/// Builds a crisp 32×32 ARGB Ruby amber emblem icon for the system tray.
unsafe fn create_ruby_icon() -> *mut c_void {
   const SIZE: i32 = 32;
   #[expect(
      clippy::cast_possible_truncation,
      reason = "BITMAPINFOHEADER is 40 bytes"
   )]
   let bmi = BitmapInfoHeader {
      bi_size:          size_of::<BitmapInfoHeader>() as u32,
      bi_width:         SIZE,
      bi_height:        -SIZE, // top-down DIB
      bi_planes:        1,
      bi_bit_count:     32,
      bi_compression:   BI_RGB,
      bi_size_image:    0,
      bi_x_pels:        0,
      bi_y_pels:        0,
      bi_clr_used:      0,
      bi_clr_important: 0,
   };
   let mut bits: *mut c_void = ptr::null_mut();
   // SAFETY: Win32 GDI DIB creation and icon assembly; temporary bitmaps are
   // deleted.
   unsafe {
      let color = CreateDIBSection(
         ptr::null_mut(),
         &raw const bmi,
         DIB_RGB_COLORS,
         &raw mut bits,
         ptr::null_mut(),
         0,
      );
      if color.is_null() || bits.is_null() {
         return LoadIconW(ptr::null_mut(), IDI_APPLICATION as *const u16);
      }
      let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (SIZE * SIZE) as usize);
      for y in 0..SIZE {
         for x in 0..SIZE {
            let dx = f32::from(i16::try_from(x).unwrap_or(0)) - 15.5;
            let dy = f32::from(i16::try_from(y).unwrap_or(0)) - 15.5;
            let dist = dx.hypot(dy);
            // Outer dark charcoal ring, rich amber sun body, bright gold core.
            let pixel = if dist <= 5.5 {
               0xFFFF_F3B0 // warm bright core (BGRA: 0xAARRGGBB in little-endian u32)
            } else if dist <= 12.5 {
               0xFFF5_9E0B // Ruby amber
            } else if dist <= 14.5 {
               0xFF1E_1F22 // dark Discord surface border
            } else {
               0x0000_0000 // transparent
            };
            #[expect(clippy::cast_sign_loss, reason = "x and y are in 0..SIZE")]
            let index = (y * SIZE + x) as usize;
            pixels[index] = pixel;
         }
      }
      let mask_bytes = [0u8; (SIZE * SIZE / 8) as usize];
      let mask = CreateBitmap(SIZE, SIZE, 1, 1, mask_bytes.as_ptr().cast());
      let mut info = IconInfo {
         f_icon:    1,
         x_hotspot: 0,
         y_hotspot: 0,
         hbm_mask:  mask,
         hbm_color: color,
      };
      let icon = CreateIconIndirect(&raw mut info);
      DeleteObject(color);
      if !mask.is_null() {
         DeleteObject(mask);
      }
      if icon.is_null() {
         LoadIconW(ptr::null_mut(), IDI_APPLICATION as *const u16)
      } else {
         icon
      }
   }
}

fn tray_uid() -> u32 {
   // SAFETY: GetCurrentProcessId has no preconditions.
   TRAY_UID ^ unsafe { GetCurrentProcessId() }
}

fn empty_nid(hwnd: *mut c_void) -> NotifyIconDataW {
   #[expect(
      clippy::cast_possible_truncation,
      reason = "NOTIFYICONDATAW is 976 bytes"
   )]
   let cb_size = size_of::<NotifyIconDataW>() as u32;
   NotifyIconDataW {
      cb_size,
      hwnd,
      u_id: tray_uid(),
      u_flags: 0,
      u_callback_message: WM_TRAY,
      h_icon: ptr::null_mut(),
      sz_tip: [0; 128],
      dw_state: 0,
      dw_state_mask: 0,
      sz_info: [0; 256],
      u_timeout_or_version: 5000,
      sz_info_title: [0; 64],
      dw_info_flags: 0,
      guid_item: [0; 16],
      h_balloon_icon: ptr::null_mut(),
   }
}

unsafe fn add_tray_icon(hwnd: *mut c_void, icon: *mut c_void) -> bool {
   let mut nid = empty_nid(hwnd);
   nid.u_flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
   nid.h_icon = icon;
   fill_wide(&mut nid.sz_tip, "Ruby — Click for DevTools");
   // SAFETY: `hwnd` and `icon` are live Win32 handles on the tray thread.
   unsafe { Shell_NotifyIconW(NIM_ADD, &raw mut nid) != 0 }
}

fn start_tray() {
   crate::webview::start_window_hook();
   if TRAY_STARTED.swap(true, Ordering::AcqRel) {
      return;
   }
   let _ = thread::Builder::new()
      .name("ruby-tray".to_owned())
      .spawn(run_tray_loop);
}

fn run_tray_loop() {
   let class_name = wide("RubyTrayWindow");
   let title = wide("Ruby");
   let taskbar_msg_name = wide("TaskbarCreated");
   // SAFETY: Win32 hidden message window, hotkey, and tray registration on
   // dedicated thread.
   unsafe {
      let taskbar_msg = RegisterWindowMessageW(taskbar_msg_name.as_ptr());
      TASKBAR_CREATED_MSG.store(taskbar_msg, Ordering::Release);

      let instance = GetModuleHandleW(ptr::null());
      #[expect(clippy::cast_possible_truncation, reason = "WNDCLASSEXW is 80 bytes")]
      let cb_size = size_of::<WndClassExW>() as u32;
      let cls = WndClassExW {
         cb_size,
         style: 0,
         wnd_proc: tray_wnd_proc,
         cls_extra: 0,
         wnd_extra: 0,
         instance,
         icon: ptr::null_mut(),
         cursor: ptr::null_mut(),
         background: ptr::null_mut(),
         menu_name: ptr::null(),
         class_name: class_name.as_ptr(),
         icon_sm: ptr::null_mut(),
      };
      if RegisterClassExW(&raw const cls) == 0 {
         log::debug!("tray: RegisterClassExW returned 0 (may already exist)");
      }

      let hwnd = CreateWindowExW(
         0,
         class_name.as_ptr(),
         title.as_ptr(),
         0,
         0,
         0,
         0,
         0,
         ptr::null_mut(),
         ptr::null_mut(),
         instance,
         ptr::null_mut(),
      );
      if hwnd.is_null() {
         log::warn!(
            "tray: CreateWindowExW failed: {}",
            std::io::Error::last_os_error()
         );
         return;
      }

      let icon = create_ruby_icon();
      TRAY_ICON.store(icon as usize, Ordering::Release);

      if !add_tray_icon(hwnd, icon) {
         log::warn!("tray: Shell_NotifyIconW(NIM_ADD) failed");
         DestroyWindow(hwnd);
         DestroyIcon(icon);
         TRAY_ICON.store(0, Ordering::Release);
         return;
      }
      TRAY_HWND.store(hwnd as usize, Ordering::Release);

      // Flush any startup notifications queued before the tray HWND was ready.
      let pending = std::mem::take(
         &mut *PENDING_BALLOONS
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
      );
      for (t, b) in pending {
         send_balloon(hwnd, icon, &t, &b);
      }

      let mut msg = Msg {
         hwnd:    ptr::null_mut(),
         message: 0,
         wparam:  0,
         lparam:  0,
         time:    0,
         pt:      Point { x: 0, y: 0 },
      };
      while GetMessageW(&raw mut msg, ptr::null_mut(), 0, 0) > 0 {
         TranslateMessage(&raw const msg);
         DispatchMessageW(&raw const msg);
      }

      TRAY_HWND.store(0, Ordering::Release);
      let mut del = empty_nid(hwnd);
      Shell_NotifyIconW(NIM_DELETE, &raw mut del);
      DestroyIcon(icon);
      TRAY_ICON.store(0, Ordering::Release);
   }
}

fn send_balloon(hwnd: *mut c_void, icon: *mut c_void, title: &str, body: &str) {
   let mut nid = empty_nid(hwnd);
   nid.u_flags = NIF_INFO;
   nid.dw_info_flags = NIIF_USER;
   nid.h_balloon_icon = icon;
   fill_wide(&mut nid.sz_info_title, title);
   fill_wide(&mut nid.sz_info, body);
   // SAFETY: `hwnd` is a live tray window registered via NIM_ADD.
   unsafe {
      Shell_NotifyIconW(NIM_MODIFY, &raw mut nid);
   }
}

fn balloon(title: &str, body: &str) {
   let raw = TRAY_HWND.load(Ordering::Acquire);
   if raw == 0 {
      let mut queue = PENDING_BALLOONS
         .lock()
         .unwrap_or_else(PoisonError::into_inner);
      if queue.len() < 8 {
         queue.push((title.to_owned(), body.to_owned()));
      }
      drop(queue);
      return;
   }
   let icon = TRAY_ICON.load(Ordering::Acquire) as *mut c_void;
   send_balloon(raw as *mut c_void, icon, title, body);
}

fn run_menu_action(cmd: usize) {
   let action = match cmd {
      ID_OPEN_DEVTOOLS => TrayAction::OpenDevtools,
      ID_RELOAD_ALL => TrayAction::ReloadAll,
      ID_CHECK_UPDATES => TrayAction::CheckUpdates,
      ID_COPY_URL => TrayAction::CopyUrl,
      ID_OPEN_PLUGINS => TrayAction::OpenPlugins,
      _ => return,
   };
   action.run();
}

unsafe extern "system" fn tray_wnd_proc(
   hwnd: *mut c_void,
   msg: u32,
   wparam: usize,
   lparam: isize,
) -> isize {
   let taskbar_msg = TASKBAR_CREATED_MSG.load(Ordering::Acquire);
   if taskbar_msg != 0 && msg == taskbar_msg {
      let icon = TRAY_ICON.load(Ordering::Acquire) as *mut c_void;
      if !icon.is_null() {
         // SAFETY: re-registering tray icon after explorer.exe restart.
         unsafe { add_tray_icon(hwnd, icon) };
      }
      return 0;
   }

   match msg {
      WM_TRAY => {
         #[expect(
            clippy::cast_sign_loss,
            reason = "Win32 tray event code sits in low bits of lparam"
         )]
         let event = (lparam as usize) & 0xFFFF;
         match event {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_BALLOONUSERCLICK => crate::open_devtools(),
            WM_RBUTTONUP => {
               // SAFETY: Win32 popup menu owned and destroyed in this scope.
               unsafe {
                  let menu = CreatePopupMenu();
                  if !menu.is_null() {
                     let devtools_lbl = wide("Open DevTools\tCtrl+Shift+I");
                     let reload_lbl = wide("Reload All Plugins");
                     let update_lbl = wide("Check for Updates");
                     let copy_lbl = wide("Copy DevTools URL");
                     let folder_lbl = wide("Open Plugins Folder");

                     AppendMenuW(menu, MF_STRING, ID_OPEN_DEVTOOLS, devtools_lbl.as_ptr());
                     AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                     AppendMenuW(menu, MF_STRING, ID_RELOAD_ALL, reload_lbl.as_ptr());
                     AppendMenuW(menu, MF_STRING, ID_CHECK_UPDATES, update_lbl.as_ptr());
                     AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                     AppendMenuW(menu, MF_STRING, ID_COPY_URL, copy_lbl.as_ptr());
                     AppendMenuW(menu, MF_STRING, ID_OPEN_PLUGINS, folder_lbl.as_ptr());
                     SetMenuDefaultItem(menu, 0, 1);

                     let mut pt = Point { x: 0, y: 0 };
                     GetCursorPos(&raw mut pt);
                     SetForegroundWindow(hwnd);
                     let cmd = TrackPopupMenu(
                        menu,
                        TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_RETURNCMD,
                        pt.x,
                        pt.y,
                        0,
                        hwnd,
                        ptr::null(),
                     );
                     // Required per MSDN KB135788 so clicking outside dismisses the menu cleanly.
                     PostMessageW(hwnd, WM_NULL, 0, 0);
                     DestroyMenu(menu);
                     #[expect(clippy::cast_sign_loss, reason = "menu command ids are positive")]
                     if cmd > 0 {
                        run_menu_action(cmd as usize);
                     }
                  }
               }
            },
            _ => {},
         }
         0
      },
      WM_COMMAND => {
         run_menu_action(wparam & 0xFFFF);
         0
      },
      WM_DESTROY => {
         // SAFETY: standard Win32 WM_DESTROY handler.
         unsafe { PostQuitMessage(0) };
         0
      },
      _ => {
         // SAFETY: delegate unhandled messages to DefWindowProcW.
         unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
      },
   }
}

fn copy(text: &str) -> Result<(), Error> {
   let units = wide(text);
   let bytes = units.len() * size_of::<u16>();

   // SAFETY: Win32 clipboard APIs with checked return values; memory is freed
   // on any error before SetClipboardData takes ownership.
   unsafe {
      if OpenClipboard(ptr::null_mut()) == 0 {
         return Err(os_err("OpenClipboard"));
      }
      if EmptyClipboard() == 0 {
         let err = os_err("EmptyClipboard");
         CloseClipboard();
         return Err(err);
      }
      let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
      if mem.is_null() {
         let err = os_err("GlobalAlloc");
         CloseClipboard();
         return Err(err);
      }
      let locked = GlobalLock(mem);
      if locked.is_null() {
         let err = os_err("GlobalLock");
         GlobalFree(mem);
         CloseClipboard();
         return Err(err);
      }
      ptr::copy_nonoverlapping(units.as_ptr(), locked.cast::<u16>(), units.len());
      GlobalUnlock(mem);
      if SetClipboardData(CF_UNICODETEXT, mem).is_null() {
         let err = os_err("SetClipboardData");
         GlobalFree(mem);
         CloseClipboard();
         return Err(err);
      }
      CloseClipboard();
   }
   Ok(())
}

fn open(target: &str) -> Result<(), Error> {
   let op = wide("open");
   let target = wide(target);
   // SAFETY: NUL-terminated UTF-16 strings passed to ShellExecuteW.
   let result = unsafe {
      ShellExecuteW(
         ptr::null_mut(),
         op.as_ptr(),
         target.as_ptr(),
         ptr::null(),
         ptr::null(),
         SW_SHOWNORMAL,
      )
   };
   if result <= 32 {
      return Err(Error::Platform {
         message: format!("ShellExecuteW failed with code {result}"),
      });
   }
   Ok(())
}

/// Opens the native Windows file picker filtered to `.js` plugin bundles.
fn pick_js_path() -> Option<PathBuf> {
   let mut file_buf = vec![0u16; 32_768];
   let filter: Vec<u16> = "JavaScript Plugins (*.js)\0*.js\0All Files (*.*)\0*.*\0\0"
      .encode_utf16()
      .collect();
   let title = wide("Install Ruby Plugin (.js)");
   let def_ext = wide("js");
   #[expect(
      clippy::cast_possible_truncation,
      reason = "OPENFILENAMEW is 152 bytes"
   )]
   let mut ofn = OpenFileNameW {
      l_struct_size:       size_of::<OpenFileNameW>() as u32,
      hwnd_owner:          ptr::null_mut(),
      h_instance:          ptr::null_mut(),
      lpstr_filter:        filter.as_ptr(),
      lpstr_custom_filter: ptr::null_mut(),
      n_max_cust_filter:   0,
      n_filter_index:      1,
      lpstr_file:          file_buf.as_mut_ptr(),
      n_max_file:          u32::try_from(file_buf.len()).unwrap_or(32_768),
      lpstr_file_title:    ptr::null_mut(),
      n_max_file_title:    0,
      lpstr_initial_dir:   ptr::null(),
      lpstr_title:         title.as_ptr(),
      flags:               OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER,
      n_file_offset:       0,
      n_file_extension:    0,
      lpstr_def_ext:       def_ext.as_ptr(),
      l_cust_data:         0,
      lpfn_hook:           ptr::null(),
      lp_template_name:    ptr::null(),
      pv_reserved:         ptr::null_mut(),
      dw_reserved:         0,
      flags_ex:            0,
   };
   // SAFETY: all pointers in `ofn` stay valid for the duration of the modal
   // `GetOpenFileNameW` call.
   if unsafe { GetOpenFileNameW(&raw mut ofn) } == 0 {
      return None;
   }
   let nul = file_buf
      .iter()
      .position(|&c| c == 0)
      .unwrap_or(file_buf.len());
   Some(PathBuf::from(String::from_utf16_lossy(&file_buf[..nul])))
}

pub struct Host;

impl DesktopHost for Host {
   fn copy(text: &str) -> Result<(), Error> {
      copy(text)
   }

   fn open(target: &str) -> Result<(), Error> {
      open(target)
   }

   fn pick_js_path() -> Option<PathBuf> {
      pick_js_path()
   }

   fn notify(title: &str, body: &str) {
      balloon(title, body);
   }

   fn start_tray() {
      start_tray();
   }
}
