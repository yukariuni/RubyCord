//! Plugin pages and dialogs (`phoenix.ui.openPage`, `choose`, `showSheet`) as
//! `AppKit` windows. Discord's own GPUI modal has no arm64 locators, and a
//! native window needs none.
//!
//! Each window is a vertical `NSStackView` of section titles, button rows,
//! wrapping labels and scrollable read-only text views. One shared target
//! object answers the buttons, by tag, and is every window's delegate, so a
//! closed window reports itself.

use std::{
   collections::HashMap,
   ffi::c_void,
   ptr,
   sync::{
      Arc, LazyLock, Mutex, PoisonError,
      atomic::{AtomicIsize, AtomicU64, Ordering},
   },
};

use super::{
   Id, Sel, class, ns_string, objc_msgSend, on_main, sel, send, send_bool, send_f64, send_id,
   send_isize, send_tag, spawn, target_object,
};

/// A click handler. Handlers may block or call back into plugins, so they run
/// on their own thread, never `AppKit`'s.
pub type Action = Arc<dyn Fn() + Send + Sync>;

pub struct PanelButton {
   pub label: String,
   pub danger: bool,
   pub on_press: Action,
}

pub enum PanelSection {
   Buttons {
      title: Option<String>,
      buttons: Vec<PanelButton>,
   },
   Text {
      title: Option<String>,
      text: String,
   },
   Code {
      title: Option<String>,
      text: String,
      monospace: bool,
   },
}

/// An open window: the `NSWindow`, its buttons' tags and what runs when it
/// closes.
struct Open {
   window: usize,
   tags: Vec<isize>,
   on_close: Option<Action>,
}

static NEXT_PANEL: AtomicU64 = AtomicU64::new(1);
static NEXT_TAG: AtomicIsize = AtomicIsize::new(1);
static BUTTONS: LazyLock<Mutex<HashMap<isize, Action>>> = LazyLock::new(Mutex::default);
static OPEN: LazyLock<Mutex<HashMap<u64, Open>>> = LazyLock::new(Mutex::default);

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
   x: f64,
   y: f64,
   width: f64,
   height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Insets {
   top: f64,
   left: f64,
   bottom: f64,
   right: f64,
}

msg_send!(send_window(rect: Rect, style: usize, backing: usize, defer: bool) -> Id);
msg_send!(send_insets(insets: Insets) -> ());
msg_send!(send_f64_unit(value: f64) -> ());
msg_send!(send_font(size: f64, weight: f64) -> Id);
msg_send!(send_button(title: Id, target: Id, action: Sel) -> Id);
msg_send!(send_priority(priority: f32, orientation: isize) -> ());
msg_send!(send_size(width: f64, height: f64) -> ());
msg_send!(send_anchor(anchor: Id, constant: f64) -> Id);
msg_send!(send_fitting() -> Size);

#[repr(C)]
#[derive(Clone, Copy)]
struct Size {
   width: f64,
   height: f64,
}

/// Opens a window titled `title`, and returns its id for [`close_panel`].
/// `on_close` runs once when it closes, by the user or by [`close_panel`].
pub fn show_panel(title: String, sections: Vec<PanelSection>, on_close: Option<Action>) -> u64 {
   let id = NEXT_PANEL.fetch_add(1, Ordering::Relaxed);
   on_main(move || {
      // SAFETY: on the main thread, inside the dispatch block's autorelease
      // pool.
      unsafe { build(id, &title, sections, on_close) };
   });
   id
}

/// Closes window `id` if it is still open.
pub fn close_panel(id: u64) {
   on_main(move || {
      let window = OPEN
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .get(&id)
         .map(|open| open.window);
      if let Some(window) = window {
         // SAFETY: a window `build` made and still holds; closing it calls
         // `window_will_close`, which forgets it.
         unsafe { send(window as Id, sel(c"close")) };
      }
   });
}

/// # Safety
///
/// On the main thread, inside an autorelease pool.
#[expect(
   clippy::too_many_lines,
   reason = "one arm per section kind, each a few AppKit calls"
)]
unsafe fn build(id: u64, title: &str, sections: Vec<PanelSection>, on_close: Option<Action>) {
   /// Titled, closable, miniaturizable and resizable.
   const STYLE: usize = 1 | 2 | 4 | 8;
   /// `NSBackingStoreBuffered`.
   const BUFFERED: usize = 2;
   /// `NSUserInterfaceLayoutOrientationVertical`, `Horizontal`.
   const VERTICAL: isize = 1;
   const HORIZONTAL: isize = 0;
   /// `NSLayoutAttributeLeading`, and `CenterY` for a row of buttons.
   const LEADING: isize = 5;
   const CENTER_Y: isize = 10;
   /// The stack's inset on each side.
   const INSET: f64 = 16.0;
   /// `NSBezelBorder`.
   const BEZEL: isize = 2;
   /// `NSFontWeightRegular`.
   const REGULAR: f64 = 0.0;
   /// `NSLayoutPriorityDefaultLow`, so a text view grows before a row does.
   const LOW: f32 = 250.0;

   let target = target();
   if target.is_null() {
      return;
   }
   let mut tags = Vec::new();
   // SAFETY: on the main thread; every object made here is checked for nil
   // before it is used, and the window holds the views it is given.
   unsafe {
      let window = send_window(
         send(class(c"NSWindow"), sel(c"alloc")),
         sel(c"initWithContentRect:styleMask:backing:defer:"),
         Rect {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 560.0,
         },
         STYLE,
         BUFFERED,
         false,
      );
      if window.is_null() {
         return;
      }
      send_bool(window, sel(c"setReleasedWhenClosed:"), false);
      send_id(window, sel(c"setTitle:"), ns_string(title));
      send_size(window, sel(c"setContentMinSize:"), 360.0, 240.0);

      let stack = send(send(class(c"NSStackView"), sel(c"alloc")), sel(c"init"));
      send_isize(stack, sel(c"setOrientation:"), VERTICAL);
      send_isize(stack, sel(c"setAlignment:"), LEADING);
      send_f64_unit(stack, sel(c"setSpacing:"), 10.0);
      send_insets(
         stack,
         sel(c"setEdgeInsets:"),
         Insets {
            top: INSET,
            left: INSET,
            bottom: INSET,
            right: INSET,
         },
      );

      // Every row spans the window, less the insets.
      let add = |view: Id| {
         if view.is_null() {
            return;
         }
         send_id(stack, sel(c"addArrangedSubview:"), view);
         let wide = send_anchor(
            send(view, sel(c"widthAnchor")),
            sel(c"constraintEqualToAnchor:constant:"),
            send(stack, sel(c"widthAnchor")),
            -2.0 * INSET,
         );
         send_bool(wide, sel(c"setActive:"), true);
      };
      let mut fixed_height = true;
      let heading = |text: &str| {
         let label = send_id(
            class(c"NSTextField"),
            sel(c"labelWithString:"),
            ns_string(text),
         );
         send_id(
            label,
            sel(c"setFont:"),
            send_f64(class(c"NSFont"), sel(c"boldSystemFontOfSize:"), 13.0),
         );
         label
      };

      for section in sections {
         let (PanelSection::Buttons { title, .. }
         | PanelSection::Text { title, .. }
         | PanelSection::Code { title, .. }) = &section;
         if let Some(title) = title {
            add(heading(title));
         }
         match section {
            PanelSection::Buttons { buttons, .. } => {
               let row = send(send(class(c"NSStackView"), sel(c"alloc")), sel(c"init"));
               send_isize(row, sel(c"setOrientation:"), HORIZONTAL);
               send_isize(row, sel(c"setAlignment:"), CENTER_Y);
               send_f64_unit(row, sel(c"setSpacing:"), 8.0);
               for button in buttons {
                  let tag = NEXT_TAG.fetch_add(1, Ordering::Relaxed);
                  let view = send_button(
                     class(c"NSButton"),
                     sel(c"buttonWithTitle:target:action:"),
                     ns_string(&button.label),
                     target,
                     sel(c"rubyPanel:"),
                  );
                  if view.is_null() {
                     continue;
                  }
                  send_isize(view, sel(c"setTag:"), tag);
                  if button.danger {
                     send_bool(view, sel(c"setHasDestructiveAction:"), true);
                     send_id(
                        view,
                        sel(c"setContentTintColor:"),
                        send(class(c"NSColor"), sel(c"systemRedColor")),
                     );
                  }
                  BUTTONS
                     .lock()
                     .unwrap_or_else(PoisonError::into_inner)
                     .insert(tag, button.on_press);
                  tags.push(tag);
                  send_id(row, sel(c"addArrangedSubview:"), view);
               }
               add(row);
            },
            PanelSection::Text { text, .. } => {
               let label = send_id(
                  class(c"NSTextField"),
                  sel(c"wrappingLabelWithString:"),
                  ns_string(&text),
               );
               send_bool(label, sel(c"setSelectable:"), true);
               add(label);
            },
            PanelSection::Code {
               text, monospace, ..
            } => {
               let scroll = send(class(c"NSTextView"), sel(c"scrollableTextView"));
               if scroll.is_null() {
                  continue;
               }
               fixed_height = false;
               send_isize(scroll, sel(c"setBorderType:"), BEZEL);
               let view = send(scroll, sel(c"documentView"));
               send_bool(view, sel(c"setEditable:"), false);
               send_bool(view, sel(c"setSelectable:"), true);
               send_bool(view, sel(c"setRichText:"), false);
               send_id(view, sel(c"setString:"), ns_string(&text));
               let font = if monospace {
                  send_font(
                     class(c"NSFont"),
                     sel(c"monospacedSystemFontOfSize:weight:"),
                     12.0,
                     REGULAR,
                  )
               } else {
                  send_f64(class(c"NSFont"), sel(c"systemFontOfSize:"), 13.0)
               };
               send_id(view, sel(c"setFont:"), font);
               send_priority(
                  scroll,
                  sel(c"setContentHuggingPriority:forOrientation:"),
                  LOW - 1.0,
                  VERTICAL,
               );
               let tall = send_f64(
                  send(scroll, sel(c"heightAnchor")),
                  sel(c"constraintGreaterThanOrEqualToConstant:"),
                  90.0,
               );
               send_bool(tall, sel(c"setActive:"), true);
               add(scroll);
            },
         }
      }

      send_id(window, sel(c"setContentView:"), stack);
      // Without a text view to grow, the window fits what it holds.
      if fixed_height {
         let fits = send_fitting(stack, sel(c"fittingSize"));
         send_size(
            window,
            sel(c"setContentSize:"),
            420.0,
            fits.height.max(120.0),
         );
      }
      send_id(window, sel(c"setDelegate:"), target);
      OPEN.lock().unwrap_or_else(PoisonError::into_inner).insert(
         id,
         Open {
            window: window as usize,
            tags,
            on_close,
         },
      );
      send(window, sel(c"center"));
      send_id(window, sel(c"makeKeyAndOrderFront:"), ptr::null_mut());
   }
}

/// A button's click: runs the handler its tag maps to.
extern "C" fn panel_clicked(_this: Id, _selector: Sel, sender: Id) {
   // SAFETY: AppKit passes the `NSButton` that was clicked.
   let tag = unsafe { send_tag(sender, sel(c"tag")) };
   let action = BUTTONS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get(&tag)
      .cloned();
   if let Some(action) = action {
      spawn("ruby-panel", move || action());
   }
}

/// `windowWillClose:`: forgets the window and its buttons, and runs its
/// `on_close`. Only windows [`build`] made have this delegate.
extern "C" fn window_will_close(_this: Id, _selector: Sel, notification: Id) {
   // SAFETY: AppKit passes the `NSNotification`, whose object is the window.
   let window = unsafe { send(notification, sel(c"object")) } as usize;
   let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
   let Some(id) = open
      .iter()
      .find_map(|(&id, entry)| (entry.window == window).then_some(id))
   else {
      return;
   };
   let Some(closed) = open.remove(&id) else {
      return;
   };
   drop(open);
   let mut buttons = BUTTONS.lock().unwrap_or_else(PoisonError::into_inner);
   for tag in &closed.tags {
      buttons.remove(tag);
   }
   drop(buttons);
   // Released once AppKit is done closing it.
   // SAFETY: the window `build` allocated, which nothing else releases.
   unsafe { send(window as Id, sel(c"autorelease")) };
   if let Some(on_close) = closed.on_close {
      spawn("ruby-panel", move || on_close());
   }
}

/// The shared button target and window delegate.
fn target() -> Id {
   static TARGET: LazyLock<usize> = LazyLock::new(|| {
      // SAFETY: on the main thread, with `panel_clicked` and
      // `window_will_close` of type `v@:@`.
      unsafe {
         target_object(
            c"RubyPanelTarget",
            &[
               (c"rubyPanel:", panel_clicked as *const c_void),
               (c"windowWillClose:", window_will_close as *const c_void),
            ],
         )
      }
      .addr()
   });
   *TARGET as Id
}
