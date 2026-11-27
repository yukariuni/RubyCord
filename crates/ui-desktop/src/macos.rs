//! macOS desktop backend. Clipboard, links, notifications and the file picker
//! go through the system's own command-line tools, and the tray is an
//! `NSStatusItem` made through the Objective-C runtime on Discord's main
//! thread, so the crate links nothing beyond libSystem.

use std::{
   ffi::{CStr, CString, c_char, c_void},
   io::Write as _,
   path::PathBuf,
   process::{Command, Stdio},
   ptr,
   sync::Once,
   thread,
};

use crate::{DesktopHost, Error, TrayAction};

/// Spawns `command` and waits for it on a background thread, so it never
/// lingers as a zombie.
fn spawn_reaped(command: &mut Command) -> Result<(), Error> {
   let mut child = command.spawn().map_err(|err| Error::Platform {
      message: format!("{}: {err}", command.get_program().display()),
   })?;
   let _ = thread::Builder::new()
      .name("ruby-reap".to_owned())
      .spawn(move || child.wait());
   Ok(())
}

pub struct Host;

impl DesktopHost for Host {
   fn copy(text: &str) -> Result<(), Error> {
      let fail = |message: String| Error::Platform { message };
      let mut child = Command::new("/usr/bin/pbcopy")
         .stdin(Stdio::piped())
         .stdout(Stdio::null())
         .stderr(Stdio::null())
         .spawn()
         .map_err(|err| fail(format!("pbcopy: {err}")))?;
      let written = child
         .stdin
         .take()
         .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
      if child.wait().is_ok_and(|status| status.success()) && written {
         Ok(())
      } else {
         Err(fail("pbcopy failed".to_owned()))
      }
   }

   fn open(target: &str) -> Result<(), Error> {
      spawn_reaped(Command::new("/usr/bin/open").arg("--").arg(target))
   }

   fn pick_path(ext: &str) -> Option<PathBuf> {
      // `ext` is `js` or `css` from the callers, never user text.
      let output = Command::new("/usr/bin/osascript")
         .args([
            "-e".to_owned(),
            format!(
               "POSIX path of (choose file with prompt \"Import Ruby file (.{ext})\" of type \
                {{\"{ext}\"}})"
            ),
         ])
         .stderr(Stdio::null())
         .output()
         .ok()?;
      let path = String::from_utf8(output.stdout).ok()?;
      let path = path.trim_end_matches('\n');
      (output.status.success() && !path.is_empty()).then(|| PathBuf::from(path))
   }

   fn notify(title: &str, body: &str) {
      // The text travels as arguments, never inside the script, so a plugin's
      // quote can't change what runs.
      let mut script = Command::new("/usr/bin/osascript");
      script.args([
         "-e",
         "on run argv",
         "-e",
         "display notification (item 2 of argv) with title (item 1 of argv)",
         "-e",
         "end run",
         "--",
         title,
         body,
      ]);
      if let Err(err) = spawn_reaped(&mut script) {
         log::debug!("notify: {err}");
      }
   }

   fn start_tray() {
      static STARTED: Once = Once::new();
      STARTED.call_once(|| {
         on_main(start_status_item);
         on_main(start_shortcuts);
      });
   }
}

/// Re-renders every window, from any thread. A dirty layer alone only
/// presents GPUI's last frame, while a key status change makes GPUI refresh
/// the whole window.
pub fn redraw_windows() {
   on_main(|| {
      // SAFETY: on the main thread, where AppKit may be used; every object is
      // checked for nil before it is messaged.
      unsafe {
         let app = send(class(c"NSApplication"), sel(c"sharedApplication"));
         let windows = send(app, sel(c"windows"));
         if windows.is_null() {
            return;
         }
         let count = send_usize(windows, sel(c"count"));
         for at in 0..count {
            let window = send_index(windows, sel(c"objectAtIndex:"), at);
            let delegate = send(window, sel(c"delegate"));
            let key_status = sel(c"windowDidBecomeKey:");
            if !delegate.is_null()
               && send_responds(delegate, sel(c"respondsToSelector:"), key_status)
            {
               send_id(delegate, key_status, ptr::null_mut());
            }
         }
      }
   });
}

type Id = *mut c_void;
type Sel = *const c_void;

#[link(name = "objc")]
unsafe extern "C" {
   fn objc_getClass(name: *const c_char) -> Id;
   fn sel_registerName(name: *const c_char) -> Sel;
   fn objc_msgSend();
   fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
   fn objc_registerClassPair(class: Id);
   fn class_addMethod(class: Id, name: Sel, imp: *const c_void, types: *const c_char) -> bool;
}

unsafe extern "C" {
   static _dispatch_main_q: c_void;
   fn dispatch_async_f(
      queue: *const c_void,
      context: *mut c_void,
      work: extern "C" fn(*mut c_void),
   );
}

fn class(name: &CStr) -> Id {
   // SAFETY: a lookup by NUL-terminated name, nil when missing.
   unsafe { objc_getClass(name.as_ptr()) }
}

fn sel(name: &CStr) -> Sel {
   // SAFETY: registers or finds a selector by NUL-terminated name.
   unsafe { sel_registerName(name.as_ptr()) }
}

/// `objc_msgSend` cast to one method's exact signature, which arm64 needs
/// since it passes variadic arguments differently.
macro_rules! msg_send {
   ($name:ident ($($arg:ident: $ty:ty),*) -> $ret:ty) => {
      /// # Safety
      ///
      /// `object` is nil or responds to `selector` with this signature.
      unsafe fn $name(object: Id, selector: Sel $(, $arg: $ty)*) -> $ret {
         // SAFETY: the caller matches the method's signature.
         unsafe {
            let send: unsafe extern "C" fn(Id, Sel $(, $ty)*) -> $ret =
               std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(object, selector $(, $arg)*)
         }
      }
   };
}

msg_send!(send() -> Id);
msg_send!(send_id(arg: Id) -> Id);
msg_send!(send_bool(arg: bool) -> ());
msg_send!(send_responds(selector: Sel) -> bool);
msg_send!(send_usize() -> usize);
msg_send!(send_index(at: usize) -> Id);
msg_send!(send_isize(arg: isize) -> ());
msg_send!(send_f64(arg: f64) -> Id);
msg_send!(send_str(arg: *const c_char) -> Id);
msg_send!(send_key_code() -> u16);
msg_send!(send_monitor(mask: u64, handler: *const Block) -> Id);
msg_send!(send_symbol(name: Id, description: Id) -> Id);
msg_send!(send_item(title: Id, action: Sel, key: Id) -> Id);
msg_send!(send_tag() -> isize);

mod panel;

pub use panel::{Action as PanelAction, PanelButton, PanelSection, close_panel, show_panel};

/// An autoreleased `NSString`, nil for text with a NUL.
///
/// # Safety
///
/// On the main thread, inside its autorelease pool.
unsafe fn ns_string(text: &str) -> Id {
   let Ok(text) = CString::new(text) else {
      return ptr::null_mut();
   };
   // SAFETY: forwarded from the caller; `text` outlives the call, which copies
   // it.
   unsafe {
      send_str(
         class(c"NSString"),
         sel(c"stringWithUTF8String:"),
         text.as_ptr(),
      )
   }
}

/// Runs `work` on the main thread, where `AppKit` lives, through its dispatch
/// queue, which GPUI's `NSApplication` run loop drains.
fn on_main(work: impl FnOnce() + Send + 'static) {
   type Work = Box<dyn FnOnce() + Send>;
   extern "C" fn run(context: *mut c_void) {
      // SAFETY: `context` is the `Box<Work>` `on_main` leaked, read back
      // once.
      let work = unsafe { Box::from_raw(context.cast::<Work>()) };
      work();
   }
   let work: Box<Work> = Box::new(Box::new(work));
   // SAFETY: the main queue lives as long as the process, and `run` takes
   // back the box exactly once.
   unsafe {
      dispatch_async_f(&raw const _dispatch_main_q, Box::into_raw(work).cast(), run);
   }
}

/// The status item's menu calls this with the clicked item, whose tag is its
/// index in [`TrayAction::MENU`]. Actions may block, so they run elsewhere.
extern "C" fn tray_clicked(_this: Id, _selector: Sel, sender: Id) {
   // SAFETY: AppKit passes the `NSMenuItem` that was clicked.
   let tag = unsafe { send_tag(sender, sel(c"tag")) };
   let Some(action) = usize::try_from(tag)
      .ok()
      .and_then(|at| TrayAction::MENU.get(at).copied().flatten())
   else {
      return;
   };
   spawn("ruby-tray", move || action.run());
}

/// Runs `work` on a thread of its own, since `AppKit`'s may not block.
fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
   let _ = thread::Builder::new().name(name.to_owned()).spawn(work);
}

/// One instance of a new `NSObject` subclass `name`, kept for the life of the
/// process, whose methods are `(selector, imp)` pairs of type `v@:@`.
///
/// # Safety
///
/// On the main thread, and each `imp` is an `extern "C" fn(Id, Sel, Id)`.
unsafe fn target_object(name: &CStr, methods: &[(&CStr, *const c_void)]) -> Id {
   let mut target = class(name);
   if target.is_null() {
      // SAFETY: registers a new subclass with the caller's `v@:@` methods.
      unsafe {
         target = objc_allocateClassPair(class(c"NSObject"), name.as_ptr(), 0);
         if target.is_null() {
            return ptr::null_mut();
         }
         for &(selector, imp) in methods {
            class_addMethod(target, sel(selector), imp, c"v@:@".as_ptr());
         }
         objc_registerClassPair(target);
      }
   }
   // SAFETY: a fresh instance of an `NSObject` subclass, owned forever.
   unsafe { send(send(target, sel(c"alloc")), sel(c"init")) }
}

fn start_status_item() {
   /// `NSVariableStatusItemLength`.
   const VARIABLE_LENGTH: f64 = -1.0;

   // SAFETY: on the main thread; every object is checked for nil before
   // anything depends on it, and the item, menu and target are kept for the
   // life of the process.
   unsafe {
      let bar = send(class(c"NSStatusBar"), sel(c"systemStatusBar"));
      let target = target_object(
         c"RubyTrayTarget",
         &[(c"rubyTray:", tray_clicked as *const c_void)],
      );
      if bar.is_null() || target.is_null() {
         log::info!("tray: AppKit is not available");
         return;
      }
      let item = send(
         send_f64(bar, sel(c"statusItemWithLength:"), VARIABLE_LENGTH),
         sel(c"retain"),
      );
      let button = send(item, sel(c"button"));
      let image = send_symbol(
         class(c"NSImage"),
         sel(c"imageWithSystemSymbolName:accessibilityDescription:"),
         ns_string("bird"),
         ns_string("Ruby"),
      );
      if image.is_null() {
         send_id(button, sel(c"setTitle:"), ns_string("Ruby"));
      } else {
         send_id(button, sel(c"setImage:"), image);
      }
      send_id(button, sel(c"setToolTip:"), ns_string("Ruby"));

      let menu = send_id(
         send(class(c"NSMenu"), sel(c"alloc")),
         sel(c"initWithTitle:"),
         ns_string("Ruby"),
      );
      for (at, entry) in TrayAction::MENU.iter().enumerate() {
         let menu_item = entry.map_or_else(
            || send(class(c"NSMenuItem"), sel(c"separatorItem")),
            |action| {
               let menu_item = send_item(
                  send(class(c"NSMenuItem"), sel(c"alloc")),
                  sel(c"initWithTitle:action:keyEquivalent:"),
                  ns_string(action.label()),
                  sel(c"rubyTray:"),
                  // An uppercase key adds Shift to the default Command, so
                  // the menu shows the monitor's Shift-Command-I.
                  ns_string(if matches!(action, TrayAction::OpenDevtools) {
                     "I"
                  } else {
                     ""
                  }),
               );
               send_id(menu_item, sel(c"setTarget:"), target);
               send_isize(menu_item, sel(c"setTag:"), at.cast_signed());
               menu_item
            },
         );
         send_id(menu, sel(c"addItem:"), menu_item);
      }
      send_id(item, sel(c"setMenu:"), menu);
   }
}

/// Ruby's shortcuts while Discord is focused: Command-Shift-I opens its
/// `DevTools`, which Discord leaves free.
const SHORTCUTS: [(usize, u16, fn()); 1] = [
   // `kVK_ANSI_I`.
   (COMMAND_SHIFT, 0x22, crate::open_devtools),
];

/// `NSEventModifierFlagShift | NSEventModifierFlagCommand`, and the mask of
/// every device-independent modifier, so variants with another modifier miss.
const COMMAND_SHIFT: usize = (1 << 17) | (1 << 20);
const MODIFIERS: usize = 0xFFFF_0000;

/// An Objective-C block, laid out as the runtime reads one.
#[repr(C)]
struct Block {
   isa: *const c_void,
   flags: i32,
   reserved: i32,
   invoke: extern "C" fn(*const Self, Id) -> Id,
   descriptor: *const BlockDescriptor,
}

#[repr(C)]
struct BlockDescriptor {
   reserved: usize,
   size: usize,
}

unsafe extern "C" {
   static _NSConcreteGlobalBlock: c_void;
}

/// `NSEvent *(^)(NSEvent *)`: runs a shortcut and swallows its key, or hands
/// every other key on.
extern "C" fn key_down(_block: *const Block, event: Id) -> Id {
   // SAFETY: AppKit passes the `NSEvent` being dispatched.
   let (code, flags) = unsafe {
      (
         send_key_code(event, sel(c"keyCode")),
         send_usize(event, sel(c"modifierFlags")),
      )
   };
   let pressed = flags & MODIFIERS;
   let Some(&(_, _, run)) = SHORTCUTS
      .iter()
      .find(|&&(modifiers, key, _)| modifiers == pressed && key == code)
   else {
      return event;
   };
   log::info!("shortcuts: key {code:#x}");
   // Opening spawns a browser, which must not block the main thread.
   spawn("ruby-shortcut", run);
   ptr::null_mut()
}

fn start_shortcuts() {
   /// `NSEventMaskKeyDown`.
   const KEY_DOWN: u64 = 1 << 10;
   /// `BLOCK_IS_GLOBAL`: the runtime never copies or frees it.
   const IS_GLOBAL: i32 = 1 << 28;
   static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
      reserved: 0,
      size: size_of::<Block>(),
   };

   // Leaked, since the monitor keeps it for the life of the process.
   let block = Box::leak(Box::new(Block {
      isa: &raw const _NSConcreteGlobalBlock,
      flags: IS_GLOBAL,
      reserved: 0,
      invoke: key_down,
      descriptor: &raw const DESCRIPTOR,
   }));
   // SAFETY: on the main thread; the block has the handler's signature and
   // outlives the monitor, which AppKit retains.
   let monitor = unsafe {
      send_monitor(
         class(c"NSEvent"),
         sel(c"addLocalMonitorForEventsMatchingMask:handler:"),
         KEY_DOWN,
         block,
      )
   };
   if monitor.is_null() {
      log::info!("shortcuts: AppKit refused the key monitor");
   } else {
      log::info!("shortcuts: Command-Shift-I opens DevTools");
   }
}
