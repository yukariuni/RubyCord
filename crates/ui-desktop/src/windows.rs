use std::{
   ffi::c_void,
   path::PathBuf,
   ptr, slice,
   sync::{
      Mutex, PoisonError,
      atomic::{AtomicUsize, Ordering},
   },
};

use crate::{DesktopHost, Error, logo};

const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;
const SW_SHOWNORMAL: i32 = 1;

const WM_GETICON: u32 = 0x007F;
const WM_SETICON: u32 = 0x0080;
const ICON_SMALL: usize = 0;
const ICON_BIG: usize = 1;
const SMTO_ABORTIFHUNG: u32 = 0x0002;

const NIM_ADD: u32 = 0x0000_0000;
const NIM_MODIFY: u32 = 0x0000_0001;
const NIF_ICON: u32 = 0x0000_0002;
const NIF_INFO: u32 = 0x0000_0010;
const NIIF_USER: u32 = 0x0000_0004;

const IDI_APPLICATION: usize = 32512;
const BI_RGB: u32 = 0;
const DIB_RGB_COLORS: u32 = 0;
const PAGE_READWRITE: u32 = 0x04;

/// Discord Phoenix's own tray icon, as its first `NIM_ADD` named it.
static DISCORD_TRAY: Mutex<Option<(usize, u32)>> = Mutex::new(None);
/// Notifications asked for before that icon exists.
static PENDING_BALLOONS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// The real `Shell_NotifyIconW`, which Discord's import slot held before the hook.
static ORIGINAL_NOTIFY: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
struct NotifyIconDataW {
   cb_size: u32,
   hwnd: *mut c_void,
   u_id: u32,
   u_flags: u32,
   u_callback_message: u32,
   h_icon: *mut c_void,
   sz_tip: [u16; 128],
   dw_state: u32,
   dw_state_mask: u32,
   sz_info: [u16; 256],
   u_timeout_or_version: u32,
   sz_info_title: [u16; 64],
   dw_info_flags: u32,
   guid_item: [u8; 16],
   h_balloon_icon: *mut c_void,
}

#[repr(C)]
#[expect(clippy::struct_field_names, reason = "Win32's BITMAPINFOHEADER names")]
struct BitmapInfoHeader {
   bi_size: u32,
   bi_width: i32,
   bi_height: i32,
   bi_planes: u16,
   bi_bit_count: u16,
   bi_compression: u32,
   bi_size_image: u32,
   bi_x_pels: i32,
   bi_y_pels: i32,
   bi_clr_used: u32,
   bi_clr_important: u32,
}

#[repr(C)]
struct IconInfo {
   f_icon: i32,
   x_hotspot: u32,
   y_hotspot: u32,
   hbm_mask: *mut c_void,
   hbm_color: *mut c_void,
}

#[link(name = "user32")]
unsafe extern "system" {
   fn OpenClipboard(hwnd: *mut c_void) -> i32;
   fn CloseClipboard() -> i32;
   fn EmptyClipboard() -> i32;
   fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
   fn LoadIconW(instance: *mut c_void, name: *const u16) -> *mut c_void;
   fn CreateIconIndirect(info: *mut IconInfo) -> *mut c_void;
   fn SendMessageTimeoutW(
      hwnd: *mut c_void,
      msg: u32,
      wparam: usize,
      lparam: isize,
      flags: u32,
      timeout: u32,
      result: *mut usize,
   ) -> isize;
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
   fn VirtualProtect(address: *mut c_void, size: usize, new: u32, old: *mut u32) -> i32;
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
   l_struct_size: u32,
   hwnd_owner: *mut c_void,
   h_instance: *mut c_void,
   lpstr_filter: *const u16,
   lpstr_custom_filter: *mut u16,
   n_max_cust_filter: u32,
   n_filter_index: u32,
   lpstr_file: *mut u16,
   n_max_file: u32,
   lpstr_file_title: *mut u16,
   n_max_file_title: u32,
   lpstr_initial_dir: *const u16,
   lpstr_title: *const u16,
   flags: u32,
   n_file_offset: u16,
   n_file_extension: u16,
   lpstr_def_ext: *const u16,
   l_cust_data: isize,
   lpfn_hook: *const c_void,
   lp_template_name: *const u16,
   pv_reserved: *mut c_void,
   dw_reserved: u32,
   flags_ex: u32,
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

/// The RubyCord logo as an icon `size` pixels square, built once and kept for the life of the
/// process.
fn logo_icon(size: usize) -> *mut c_void {
   static ICONS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
   let mut icons = ICONS.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some(&(_, icon)) = icons.iter().find(|(known, _)| *known == size) {
      return icon as *mut c_void;
   }
   // SAFETY: builds a GDI icon from the logo's pixels.
   let icon = unsafe { create_logo_icon(size) };
   icons.push((size, icon as usize));
   icon
}

unsafe fn create_logo_icon(size: usize) -> *mut c_void {
   let side = i32::try_from(size).unwrap_or(32);
   #[expect(
      clippy::cast_possible_truncation,
      reason = "BITMAPINFOHEADER is 40 bytes"
   )]
   let bmi = BitmapInfoHeader {
      bi_size: size_of::<BitmapInfoHeader>() as u32,
      bi_width: side,
      bi_height: -side, // top-down DIB
      bi_planes: 1,
      bi_bit_count: 32,
      bi_compression: BI_RGB,
      bi_size_image: 0,
      bi_x_pels: 0,
      bi_y_pels: 0,
      bi_clr_used: 0,
      bi_clr_important: 0,
   };
   let mut bits: *mut c_void = ptr::null_mut();
   // SAFETY: Win32 GDI DIB creation and icon assembly; temporary bitmaps are deleted.
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
      slice::from_raw_parts_mut(bits.cast::<u32>(), size * size)
         .copy_from_slice(&logo::pixels(size));
      let mask_bytes = vec![0u8; size * size / 8];
      let mask = CreateBitmap(side, side, 1, 1, mask_bytes.as_ptr().cast());
      let mut info = IconInfo {
         f_icon: 1,
         x_hotspot: 0,
         y_hotspot: 0,
         hbm_mask: mask,
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

/// Gives a Discord window the RubyCord logo for its title bar, taskbar button and Alt+Tab. Cheap
/// to repeat: it only sends a message when the window shows another icon.
pub(crate) fn set_window_icons(hwnd: *mut c_void) {
   for (kind, size) in [(ICON_SMALL, 16), (ICON_BIG, 48)] {
      let icon = logo_icon(size);
      let mut current = 0_usize;
      // SAFETY: `hwnd` is a window of this process, and the timeout keeps a hung one from
      // blocking this thread.
      unsafe {
         SendMessageTimeoutW(
            hwnd,
            WM_GETICON,
            kind,
            0,
            SMTO_ABORTIFHUNG,
            500,
            &raw mut current,
         );
         if current != icon as usize {
            SendMessageTimeoutW(
               hwnd,
               WM_SETICON,
               kind,
               icon as isize,
               SMTO_ABORTIFHUNG,
               500,
               ptr::null_mut(),
            );
         }
      }
   }
}

fn empty_nid(hwnd: *mut c_void, uid: u32) -> NotifyIconDataW {
   #[expect(
      clippy::cast_possible_truncation,
      reason = "NOTIFYICONDATAW is 976 bytes"
   )]
   let cb_size = size_of::<NotifyIconDataW>() as u32;
   NotifyIconDataW {
      cb_size,
      hwnd,
      u_id: uid,
      u_flags: 0,
      u_callback_message: 0,
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

type NotifyIcon = unsafe extern "system" fn(u32, *mut NotifyIconDataW) -> i32;

/// Discord's `Shell_NotifyIconW`: its tray icon gets the RubyCord logo, and its handle is kept for
/// RubyCord's notifications, so RubyCord has no tray icon of its own.
unsafe extern "system" fn notify_detour(message: u32, data: *mut NotifyIconDataW) -> i32 {
   let original = ORIGINAL_NOTIFY.load(Ordering::Acquire);
   if original == 0 {
      return 0;
   }
   // SAFETY: `original` is the function the import slot held, stored before the slot changed.
   let original: NotifyIcon = unsafe { std::mem::transmute::<usize, NotifyIcon>(original) };
   if !data.is_null() && matches!(message, NIM_ADD | NIM_MODIFY) {
      // SAFETY: Discord passes a live NOTIFYICONDATAW. Only its leading fields are touched,
      // which sit at the same offsets in every version of the struct.
      unsafe {
         if (*data).u_flags & NIF_ICON != 0 {
            (*data).h_icon = logo_icon(32);
         }
      }
   }
   // SAFETY: forwards the caller's arguments.
   let result = unsafe { original(message, data) };
   if result != 0 && message == NIM_ADD && !data.is_null() {
      // SAFETY: as above.
      let (hwnd, uid) = unsafe { ((*data).hwnd as usize, (*data).u_id) };
      *DISCORD_TRAY.lock().unwrap_or_else(PoisonError::into_inner) = Some((hwnd, uid));
      let pending = std::mem::take(
         &mut *PENDING_BALLOONS
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
      );
      for (title, body) in pending {
         send_balloon(hwnd, uid, &title, &body);
      }
   }
   result
}

/// The slot in the main module's import table for `name`, from any DLL.
unsafe fn find_import(name: &[u8]) -> Option<*mut usize> {
   // SAFETY: reads this process's own PE headers and import table, which are mapped for good.
   unsafe {
      let base = GetModuleHandleW(ptr::null()).cast::<u8>();
      if base.is_null() {
         return None;
      }
      let word = |at: usize| base.add(at).cast::<u32>().read_unaligned();
      let nt = word(0x3C) as usize;
      // "PE\0\0", then the file header (20 bytes) and a PE32+ optional header.
      let optional = nt + 24;
      if word(nt) != 0x0000_4550 || base.add(optional).cast::<u16>().read_unaligned() != 0x20B {
         return None;
      }
      // Data directory 1 is the import table, after the 112 bytes before the directories.
      let mut descriptor = word(optional + 112 + 8) as usize;
      if descriptor == 0 {
         return None;
      }
      for _ in 0..1024 {
         let (lookup, dll, slots) = (
            word(descriptor),
            word(descriptor + 12),
            word(descriptor + 16),
         );
         if dll == 0 {
            break;
         }
         let names = if lookup == 0 { slots } else { lookup } as usize;
         for i in 0..8192 {
            let entry = base.add(names + i * 8).cast::<u64>().read_unaligned();
            if entry == 0 {
               break;
            }
            // The top bit marks an import by ordinal, which has no name.
            if entry >> 63 != 0 {
               continue;
            }
            let found = slice::from_raw_parts(base.add(entry as usize + 2), name.len() + 1);
            if &found[..name.len()] == name && found[name.len()] == 0 {
               return Some(base.add(slots as usize + i * 8).cast());
            }
         }
         descriptor += 20;
      }
      None
   }
}

fn hook_tray() {
   // SAFETY: patches one pointer in this process's own import table before Discord's tray code
   // can run through it, then restores the page's protection.
   unsafe {
      let Some(slot) = find_import(b"Shell_NotifyIconW") else {
         log::warn!("tray: Discord does not import Shell_NotifyIconW, its tray icon stays");
         return;
      };
      let mut old = 0_u32;
      if VirtualProtect(
         slot.cast(),
         size_of::<usize>(),
         PAGE_READWRITE,
         &raw mut old,
      ) == 0
      {
         log::warn!("tray: the import slot is not writable");
         return;
      }
      ORIGINAL_NOTIFY.store(slot.read(), Ordering::Release);
      slot.write(notify_detour as *const () as usize);
      VirtualProtect(slot.cast(), size_of::<usize>(), old, &raw mut old);
   }
   log::info!("tray: Discord's tray icon shows the RubyCord logo");
}

/// RubyCord has no tray icon: Discord's is the only one. This starts the Ctrl+Shift+I hook and the
/// window icons.
fn start_tray() {
   crate::webview::start_window_hook();
}

fn send_balloon(hwnd: usize, uid: u32, title: &str, body: &str) {
   let mut nid = empty_nid(hwnd as *mut c_void, uid);
   nid.u_flags = NIF_INFO;
   nid.dw_info_flags = NIIF_USER;
   nid.h_balloon_icon = logo_icon(32);
   fill_wide(&mut nid.sz_info_title, title);
   fill_wide(&mut nid.sz_info, body);
   // SAFETY: `hwnd` is the window Discord registered its tray icon with, in this process.
   unsafe {
      Shell_NotifyIconW(NIM_MODIFY, &raw mut nid);
   }
}

/// A balloon on Discord's tray icon, or the next one that appears.
fn balloon(title: &str, body: &str) {
   let tray = *DISCORD_TRAY.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some((hwnd, uid)) = tray {
      send_balloon(hwnd, uid, title, body);
      return;
   }
   let mut queue = PENDING_BALLOONS
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
   if queue.len() < 8 {
      queue.push((title.to_owned(), body.to_owned()));
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

/// Opens the native Windows file picker filtered to `.<ext>` files.
fn pick_path(ext: &str) -> Option<PathBuf> {
   let mut file_buf = vec![0u16; 32_768];
   let filter: Vec<u16> = format!("*.{ext}\0*.{ext}\0All Files (*.*)\0*.*\0\0")
      .encode_utf16()
      .collect();
   let title = wide(&format!("Import RubyCord file (.{ext})"));
   let def_ext = wide(ext);
   #[expect(
      clippy::cast_possible_truncation,
      reason = "OPENFILENAMEW is 152 bytes"
   )]
   let mut ofn = OpenFileNameW {
      l_struct_size: size_of::<OpenFileNameW>() as u32,
      hwnd_owner: ptr::null_mut(),
      h_instance: ptr::null_mut(),
      lpstr_filter: filter.as_ptr(),
      lpstr_custom_filter: ptr::null_mut(),
      n_max_cust_filter: 0,
      n_filter_index: 1,
      lpstr_file: file_buf.as_mut_ptr(),
      n_max_file: u32::try_from(file_buf.len()).unwrap_or(32_768),
      lpstr_file_title: ptr::null_mut(),
      n_max_file_title: 0,
      lpstr_initial_dir: ptr::null(),
      lpstr_title: title.as_ptr(),
      flags: OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER,
      n_file_offset: 0,
      n_file_extension: 0,
      lpstr_def_ext: def_ext.as_ptr(),
      l_cust_data: 0,
      lpfn_hook: ptr::null(),
      lp_template_name: ptr::null(),
      pv_reserved: ptr::null_mut(),
      dw_reserved: 0,
      flags_ex: 0,
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

   fn pick_path(ext: &str) -> Option<PathBuf> {
      pick_path(ext)
   }

   fn notify(title: &str, body: &str) {
      balloon(title, body);
   }

   fn start_tray() {
      start_tray();
   }

   fn hook_tray() {
      hook_tray();
   }
}
