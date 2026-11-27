//! Native GPUI / Mana widget bindings for Desktop Phoenix User Settings.
//!
//! Resolves `Discord.exe`'s own compiled GPUI (`Div`, `StyleRefinement`,
//! `AnyElement`, `SharedString`) and Mana (`Switch`, `Button`, `TextBlock`,
//! `ActionRow`) builders from unique string anchors during [`crate::prepare`] —
//! no hardcoded addresses and no webview overlay. All controls render natively
//! inside Discord's GPUI render tree with category tabs, rounded bordered
//! cards, collapsible plugin settings dropdowns, and native select dropdowns.

use std::{
   collections::HashSet,
   ffi::c_void,
   mem::MaybeUninit,
   ptr,
   sync::{
      LazyLock, Mutex, OnceLock, PoisonError,
      atomic::{AtomicBool, Ordering},
   },
};

use phoenix_hook::image::Image;
use phoenix_runtime::{
   Chosen, Control, theme,
   update::{self, Catalog, Listed},
};
use phoenix_sigs::desktop::{self, GpuiLayout, StyleOffsets, layout};

use crate::modal::{FieldKind, SettingField};

/// `gpui::SharedString` (24 bytes): tag `0x18` is `Static(&'static str)` and
/// `0x19` is `Arc<str>`, the `ArcInner` at `ptr` with the bytes at `+0x10`.
/// Not `Copy`: a heap one must reach Discord exactly once, which drops it.
#[repr(C)]
pub struct SharedString {
   pub tag: u8,
   _pad: [u8; 7],
   pub ptr: *const u8,
   pub len: usize,
}

impl SharedString {
   pub const fn from_static(s: &'static str) -> Self {
      Self {
         tag: 0x18,
         _pad: [0; 7],
         ptr: s.as_ptr(),
         len: s.len(),
      }
   }

   /// An `Arc<str>` on Discord's heap, which Discord frees with the element
   /// it moves into. Interned only before the builders resolve.
   pub fn from_str(s: &str) -> Self {
      let Some(sym) = SYMBOLS.get() else {
         return Self::from_static(intern(s));
      };
      // SAFETY: Discord's own allocator, with the layout `Arc<str>`'s drop
      // frees: two counts, then the bytes, padded to 8.
      let inner = unsafe { (sym.alloc_or_oom)(8, (16 + s.len()).next_multiple_of(8)) };
      if inner.is_null() {
         return Self::from_static(intern(s));
      }
      // SAFETY: `inner` holds `16 + s.len()` bytes, aligned to 8.
      unsafe {
         inner.cast::<[usize; 2]>().write_unaligned([1, 1]);
         ptr::copy_nonoverlapping(s.as_ptr(), inner.add(16), s.len());
      }
      Self {
         tag: 0x19,
         _pad: [0; 7],
         ptr: inner.cast_const(),
         len: s.len(),
      }
   }
}

/// Process-lifetime strings for element IDs and badge URLs, which Discord
/// borrows as `&'static str`. Only bounded sets of strings belong here.
static INTERNED: LazyLock<Mutex<HashSet<&'static str>>> =
   LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn intern_str(s: &str) -> &'static str {
   intern(s)
}

fn intern(s: &str) -> &'static str {
   let mut set = INTERNED.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some(&existing) = set.get(s) {
      return existing;
   }
   let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
   set.insert(leaked);
   leaked
}

/// A zeroed buffer at least as large as the Discord struct built in it, whose
/// size is decoded at startup and checked against `N` by `sigs`.
#[repr(C, align(16))]
pub struct Scratch<const N: usize>([u8; N]);

impl<const N: usize> Scratch<N> {
   pub const fn zeroed() -> Self {
      Self([0; N])
   }

   pub const fn as_mut_ptr(&mut self) -> *mut u8 {
      self.0.as_mut_ptr()
   }

   /// Writes `value` at `at`, or nothing if it would not fit.
   pub fn put<T>(&mut self, at: usize, value: T) {
      if at + size_of::<T>() <= N {
         // SAFETY: in bounds of `self.0`, checked above.
         unsafe {
            self
               .0
               .as_mut_ptr()
               .add(at)
               .cast::<T>()
               .write_unaligned(value);
         }
      }
   }
}

pub type RawDiv = Scratch<{ layout::DIV_CAP }>;
type RawSwitch = Scratch<{ layout::SCRATCH_CAP }>;
pub type RawButton = Scratch<{ layout::SCRATCH_CAP }>;

#[repr(C, align(8))]
pub struct RawAnyElement([u8; layout::ANY_ELEMENT]);

#[repr(C)]
pub struct RcInner {
   strong: usize,
   weak: usize,
   action_id: u64,
}

/// An `Rc<u64>` whose counts never reach zero, so Discord never frees it:
/// for when Discord's allocator isn't located (macOS, whose Rust heap may be
/// its own malloc zone). Leaks 24 bytes.
pub fn pinned_rc_u64(value: u64) -> *mut RcInner {
   Box::into_raw(Box::new(RcInner {
      strong: usize::MAX / 4,
      weak: usize::MAX / 4,
      action_id: value,
   }))
}

#[repr(C)]
pub struct RcFnVtbl {
   pub drop_in_place: *const c_void,
   pub size: usize,
   pub align: usize,
   pub call_once: *const c_void,
   pub call_mut: *const c_void,
   pub call: *const c_void,
   pub extra: *const c_void,
}

// SAFETY: vtables contain only function pointers and plain integers.
unsafe impl Sync for RcFnVtbl {}

static SWITCH_FN_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size: size_of::<u64>(),
   align: align_of::<u64>(),
   call_once: switch_callback as *const c_void,
   call_mut: switch_callback as *const c_void,
   call: switch_callback as *const c_void,
   extra: ptr::null(),
};

static BUTTON_FN_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size: size_of::<u64>(),
   align: align_of::<u64>(),
   call_once: button_callback as *const c_void,
   call_mut: button_callback as *const c_void,
   call: button_callback as *const c_void,
   extra: ptr::null(),
};

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum BtnVariant {
   Primary = 0,
   Secondary = 1,
   Critical = 3,
}

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum BtnSize {
   Small = 1,
   Medium = 2,
}

/// Encodes `Some(Length::Definite(DefiniteLength::Fraction(1.0)))` (`w_full` /
/// `h_full`) in GPUI's `StyleRefinement`.
pub const W_FULL: u64 = 0x3F80_0000_0000_0002;

/// Encodes `Some(AbsoluteLength::Pixels(px))` in GPUI's `StyleRefinement`.
pub fn gpui_px(px: f32) -> u64 {
   u64::from(px.to_bits()) << 32
}

type DivNewFn = unsafe extern "C" fn(*mut RawDiv, *const c_void) -> *mut RawDiv;
type DivMutFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv) -> *mut RawDiv;
type DivStyleMutFn = unsafe extern "C" fn(*mut RawDiv) -> *mut u8;
type DivChildDivFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, *mut RawDiv) -> *mut RawDiv;
type DivChildBtnFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, *mut RawButton) -> *mut RawDiv;
type DivIntoAnyFn = unsafe extern "C" fn(*mut RawAnyElement, *mut RawDiv) -> *mut RawAnyElement;
type DivPushAnyFn = unsafe extern "C" fn(*mut c_void, *mut RawAnyElement);
type StrIntoAnyFn =
   unsafe extern "C" fn(*mut RawAnyElement, *mut SharedString) -> *mut RawAnyElement;
type TextBlockFn =
   unsafe extern "C" fn(*mut RawDiv, *const SharedString, *const SharedString) -> *mut RawDiv;
type SwitchNewFn = unsafe extern "C" fn(*mut RawSwitch, *const u8, usize, u8) -> *mut RawSwitch;
type SwitchIntoDivFn = unsafe extern "C" fn(*mut RawDiv, *mut RawSwitch) -> *mut RawDiv;
type ButtonNewFn =
   unsafe extern "C" fn(*mut RawButton, *const u8, usize, *const SharedString) -> *mut RawButton;
type ActionRowFn = unsafe extern "C" fn(
   *mut RawDiv,
   *const SharedString,
   *const SharedString,
   *mut RawButton,
) -> *mut RawDiv;
/// Discord's `alloc_or_oom(align: usize, size: usize) -> *mut u8`.
type AllocOrOomFn = unsafe extern "C" fn(usize, usize) -> *mut u8;

struct GpuiSymbols {
   caller_loc: *const c_void,
   div_new: DivNewFn,
   div_flex: DivMutFn,
   div_flex_col: DivMutFn,
   div_style_mut: DivStyleMutFn,
   div_child_div: DivChildDivFn,
   div_child_button: DivChildBtnFn,
   div_into_any: DivIntoAnyFn,
   div_push_any: Option<DivPushAnyFn>,
   str_into_any: Option<StrIntoAnyFn>,
   text_block: TextBlockFn,
   switch_new: SwitchNewFn,
   switch_into_div: SwitchIntoDivFn,
   button_new: ButtonNewFn,
   action_row: ActionRowFn,
   alloc_or_oom: AllocOrOomFn,
   layout: GpuiLayout,
}

// SAFETY: function pointers and static `.rdata` `Location` pointer in
// `Discord.exe`.
unsafe impl Send for GpuiSymbols {}
unsafe impl Sync for GpuiSymbols {}

static SYMBOLS: OnceLock<GpuiSymbols> = OnceLock::new();

// ---------------------------------------------------------------------------
// Interactive UI state (tabs, filters, collapsible dropdowns, open selects)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
   Plugins,
   Themes,
   Catalog,
   General,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PluginFilter {
   All,
   Enabled,
   Disabled,
}

struct UiState {
   tab: Tab,
   filter: PluginFilter,
   expanded_plugins: HashSet<String>,
   open_select: Option<(String, String)>,
   cat_desktop_open: bool,
   cat_partial_open: bool,
   cat_mobile_open: bool,
}

impl Default for UiState {
   fn default() -> Self {
      Self {
         tab: Tab::Plugins,
         filter: PluginFilter::All,
         expanded_plugins: HashSet::new(),
         open_select: None,
         cat_desktop_open: true,
         cat_partial_open: false,
         cat_mobile_open: false,
      }
   }
}

/// Whether an enabled theme squares the corners, set at the top of each Ruby page render for
/// `card`.
static SQUARE: AtomicBool = AtomicBool::new(false);

static UI_STATE: LazyLock<Mutex<UiState>> = LazyLock::new(|| Mutex::new(UiState::default()));

#[derive(Clone)]
enum Action {
   SetTab(Tab),
   SetFilter(PluginFilter),
   ToggleExpandPlugin(String),
   ToggleSelectDropdown {
      plugin: String,
      key: String,
   },
   PickSelectOption {
      plugin: String,
      key: String,
      value: String,
   },
   ToggleCatalogGroup(u8),
   ToggleStaff,
   ReloadAll,
   ToggleTheme(String),
   ImportTheme,
   RefreshThemes,
   OpenDevTools,
   CheckUpdates,
   ApplyUpdates,
   RefreshCatalog,
   TogglePlugin(String),
   ReloadPlugin(String),
   EditPluginCode(String),
   EditSetting(SettingField),
   OpenInstallUrl,
   InstallFromFile,
   UninstallPlugin(String),
   ToggleSetting {
      plugin: String,
      key: String,
   },
   PressButton {
      plugin: String,
      key: String,
   },
   InstallCatalog(String),
}

static ACTIONS: LazyLock<Mutex<Vec<Action>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn register_action(action: Action) -> u64 {
   let mut list = ACTIONS.lock().unwrap_or_else(PoisonError::into_inner);
   let id = list.len() as u64;
   list.push(action);
   id
}

fn lookup_action(id: u64) -> Option<Action> {
   let list = ACTIONS.lock().unwrap_or_else(PoisonError::into_inner);
   usize::try_from(id)
      .ok()
      .and_then(|idx| list.get(idx).cloned())
}

/// Rust ABI like the `Fn::call` slot it fills: on Linux `extern "C"` would
/// assume the caller widened `checked`, which Rust callers do not.
unsafe fn switch_callback(
   self_ptr: *const u64,
   checked: u8,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   if self_ptr.is_null() {
      return;
   }
   let _ = std::panic::catch_unwind(|| {
      // SAFETY: `self_ptr` points to `RcInner.action_id` inside our `Rc<dyn
      // Fn>`.
      let id = unsafe { *self_ptr };
      let on = checked != 0;
      let Some(action) = lookup_action(id) else {
         return;
      };
      match action {
         Action::ToggleStaff => phoenix_runtime::set_staff_enabled(on),
         Action::TogglePlugin(name) => phoenix_runtime::set_enabled(&name, on),
         Action::ToggleTheme(name) => theme::set_enabled(&name, on),
         Action::ToggleSetting { plugin, key } => {
            phoenix_runtime::set_setting(&plugin, &key, Chosen::Switch(on));
         },
         _ => {},
      }
   });
}

#[expect(clippy::too_many_lines, reason = "one arm per callback kind")]
unsafe extern "C" fn button_callback(
   self_ptr: *const u64,
   _event: *const c_void,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   if self_ptr.is_null() {
      return;
   }
   let _ = std::panic::catch_unwind(|| {
      // SAFETY: `self_ptr` points to `RcInner.action_id` inside our `Rc<dyn
      // Fn>`.
      let id = unsafe { *self_ptr };
      let Some(action) = lookup_action(id) else {
         return;
      };
      match action {
         Action::SetTab(tab) => {
            let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            ui.tab = tab;
            ui.open_select = None;
         },
         Action::SetFilter(filter) => {
            UI_STATE
               .lock()
               .unwrap_or_else(PoisonError::into_inner)
               .filter = filter;
         },
         Action::ToggleExpandPlugin(name) => {
            let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            if !ui.expanded_plugins.remove(&name) {
               ui.expanded_plugins.insert(name);
            }
            ui.open_select = None;
         },
         Action::ToggleSelectDropdown { plugin, key } => {
            let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            let target = (plugin, key);
            if ui.open_select.as_ref() == Some(&target) {
               ui.open_select = None;
            } else {
               ui.open_select = Some(target);
            }
         },
         Action::PickSelectOption { plugin, key, value } => {
            phoenix_runtime::set_setting(&plugin, &key, Chosen::Text(value));
            UI_STATE
               .lock()
               .unwrap_or_else(PoisonError::into_inner)
               .open_select = None;
         },
         Action::ToggleCatalogGroup(group) => {
            let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            match group {
               0 => ui.cat_desktop_open = !ui.cat_desktop_open,
               1 => ui.cat_partial_open = !ui.cat_partial_open,
               _ => ui.cat_mobile_open = !ui.cat_mobile_open,
            }
         },
         Action::ReloadAll => phoenix_runtime::reload_all(),
         Action::RefreshThemes => theme::reload(),
         Action::ImportTheme => {
            std::thread::spawn(|| {
               let Some((file_name, bytes)) = phoenix_ui_desktop::pick_file("json") else {
                  return;
               };
               match theme::import(&file_name, &bytes) {
                  Ok(name) => phoenix_ui_desktop::toast(&format!("Imported theme {name}")),
                  Err(err) => phoenix_ui_desktop::toast(&format!("Theme import failed: {err}")),
               }
               crate::modal::force_redraw();
            });
         },
         Action::OpenDevTools => phoenix_ui_desktop::open_devtools(),
         Action::CheckUpdates => update::check_now(),
         Action::ApplyUpdates => update::apply(),
         Action::RefreshCatalog => update::fetch_catalog(),
         Action::ReloadPlugin(name) => phoenix_runtime::reload(&name),
         Action::EditPluginCode(name) => {
            let Some(source) = phoenix_runtime::source(&name) else {
               phoenix_ui_desktop::toast(&format!("Could not read source for {name}"));
               return;
            };
            crate::modal::open_code_editor(name, source);
         },
         Action::OpenInstallUrl => {
            crate::modal::open_install_url();
         },
         Action::EditSetting(field) => {
            crate::modal::open_setting_editor(field);
         },
         Action::InstallFromFile => {
            std::thread::spawn(|| {
               let Some((file_name, bytes)) = phoenix_ui_desktop::pick_file("js") else {
                  return;
               };
               match phoenix_runtime::install_file(&file_name, bytes) {
                  Ok(installed) => {
                     log::info!("ruby settings: {installed} from {file_name}");
                     phoenix_ui_desktop::toast(&installed.to_string());
                     crate::modal::force_redraw();
                  },
                  Err(err) => {
                     log::warn!("ruby settings: install file {file_name}: {err}");
                     phoenix_ui_desktop::toast(&format!("Install failed: {err}"));
                  },
               }
            });
         },
         Action::UninstallPlugin(name) => {
            if let Err(err) = phoenix_runtime::uninstall(&name) {
               log::warn!("ruby settings: uninstall {name}: {err}");
            }
         },
         Action::PressButton { plugin, key } => phoenix_runtime::press(&plugin, &key),
         Action::InstallCatalog(name) => {
            std::thread::spawn(move || match update::install_listed(&name) {
               Ok(installed) => {
                  log::info!("ruby settings: {installed}");
                  phoenix_ui_desktop::toast(&installed.to_string());
               },
               Err(err) => {
                  log::warn!("ruby settings: install {name}: {err}");
                  phoenix_ui_desktop::toast(&format!("Install failed: {err}"));
               },
            });
         },
         _ => {},
      }
   });
}

/// Resolves Discord's native GPUI / Mana widget builders from unique string
/// anchors. Safe to call from `DllMain` (`prepare()`).
pub fn locate(image: &Image<'static>) -> bool {
   if SYMBOLS.get().is_some() {
      return true;
   }

   let found = if crate::X86_CODE {
      desktop::gpui(image).and_then(|gpui| {
         let layout = desktop::gpui_layouts(image, &gpui)?;
         Ok((gpui, layout))
      })
   } else {
      arm64_builders(image)
   };
   let (
      desktop::Gpui {
         caller_loc,
         div_new,
         div_flex,
         div_flex_col,
         div_style_mut,
         div_child_div,
         div_child_button,
         div_into_any,
         div_push_any,
         str_into_any,
         text_block,
         switch_new,
         switch_into_div,
         button_new,
         action_row,
         alloc_or_oom,
         ..
      },
      found_layout,
   ) = match found {
      Ok(found) => found,
      Err(err) => {
         log::warn!("desktop gpui: {err}, leaving the settings page and accessories off");
         return false;
      },
   };

   // SAFETY: the builders were found by their call order and cross-checked,
   // their layouts match `layout`, and they have these Rust-ABI signatures.
   let symbols = unsafe {
      GpuiSymbols {
         caller_loc: caller_loc as *const c_void,
         div_new: std::mem::transmute::<usize, DivNewFn>(div_new),
         div_flex: std::mem::transmute::<usize, DivMutFn>(div_flex),
         div_flex_col: std::mem::transmute::<usize, DivMutFn>(div_flex_col),
         div_style_mut: std::mem::transmute::<usize, DivStyleMutFn>(div_style_mut),
         div_child_div: std::mem::transmute::<usize, DivChildDivFn>(div_child_div),
         div_child_button: std::mem::transmute::<usize, DivChildBtnFn>(div_child_button),
         div_into_any: std::mem::transmute::<usize, DivIntoAnyFn>(div_into_any),
         div_push_any: div_push_any.map(|addr| std::mem::transmute::<usize, DivPushAnyFn>(addr)),
         str_into_any: str_into_any.map(|addr| std::mem::transmute::<usize, StrIntoAnyFn>(addr)),
         text_block: std::mem::transmute::<usize, TextBlockFn>(text_block),
         switch_new: std::mem::transmute::<usize, SwitchNewFn>(switch_new),
         switch_into_div: std::mem::transmute::<usize, SwitchIntoDivFn>(switch_into_div),
         button_new: std::mem::transmute::<usize, ButtonNewFn>(button_new),
         action_row: std::mem::transmute::<usize, ActionRowFn>(action_row),
         alloc_or_oom: std::mem::transmute::<usize, AllocOrOomFn>(alloc_or_oom),
         layout: found_layout,
      }
   };
   let _ = SYMBOLS.set(symbols);
   log::info!(
      "desktop gpui: resolved native Mana builders (div={div_new:#x}, style={div_style_mut:#x}, \
       switch={switch_new:#x}, button={button_new:#x}, row={action_row:#x}, \
       push_any={div_push_any:#x?}, str_any={str_into_any:#x?})"
   );
   log::info!("desktop gpui: {found_layout:x?}");
   true
}

/// The arm64 builders ([`phoenix_sigs::macos::gpui`]) with every one that
/// returns through `x8` swapped for an `image::sret_caller` thunk, so they
/// are called the x86 way, result slot first, like the types here say.
#[cfg(target_os = "macos")]
fn arm64_builders(image: &Image<'_>) -> Result<(desktop::Gpui, GpuiLayout), String> {
   let gpui = phoenix_sigs::macos::gpui(image)?;
   let layout = phoenix_sigs::macos::gpui_layouts(image, &gpui)?;
   let thunk = |addr: usize| {
      phoenix_hook::image::sret_caller(addr).ok_or_else(|| "no memory for a thunk".to_owned())
   };
   let thunked = desktop::Gpui {
      div_new: thunk(gpui.div_new)?,
      div_flex: thunk(gpui.div_flex)?,
      div_flex_col: thunk(gpui.div_flex_col)?,
      div_child_div: thunk(gpui.div_child_div)?,
      div_child_button: thunk(gpui.div_child_button)?,
      div_into_any: thunk(gpui.div_into_any)?,
      str_into_any: gpui.str_into_any.map(thunk).transpose()?,
      text_block: thunk(gpui.text_block)?,
      switch_new: thunk(gpui.switch_new)?,
      switch_into_div: thunk(gpui.switch_into_div)?,
      button_new: thunk(gpui.button_new)?,
      action_row: thunk(gpui.action_row)?,
      ..gpui
   };
   Ok((thunked, layout))
}

#[cfg(not(target_os = "macos"))]
fn arm64_builders(_image: &Image<'_>) -> Result<(desktop::Gpui, GpuiLayout), String> {
   Err("no arm64 builders off macOS".to_owned())
}

/// `div.f(..)` for a `Div` builder taking and returning it by value.
fn chain(f: DivMutFn, mut div: RawDiv) -> RawDiv {
   let mut out = RawDiv::zeroed();
   // SAFETY: `f` moves `div` into `out`, both buffers fit a `Div`.
   unsafe { f(&raw mut out, &raw mut div) };
   out
}

/// `size` bytes aligned to `align` from Discord's allocator, once the builders
/// are located.
pub fn host_alloc(size: usize, align: usize) -> Option<*mut u8> {
   // SAFETY: Discord's own `alloc_or_oom`, which aborts rather than return null.
   SYMBOLS
      .get()
      .map(|sym| unsafe { (sym.alloc_or_oom)(align, size) })
}

/// The `GpuiLayout` decoded from this `Discord.exe`.
pub fn layout() -> Option<&'static GpuiLayout> {
   SYMBOLS.get().map(|sym| &sym.layout)
}

/// A `Div`'s `StyleRefinement`, written through the decoded field offsets.
pub struct Style {
   ptr: *mut u8,
   pub at: &'static StyleOffsets,
}

impl Style {
   fn put<T>(&self, field: usize, value: T) {
      if field + size_of::<T>() <= self.at.bytes {
         // SAFETY: `ptr` is the live `StyleRefinement` of `at.bytes` bytes that
         // `div_style_mut` returned, and the write is inside it.
         unsafe { self.ptr.add(field).cast::<T>().write_unaligned(value) };
      }
   }

   /// A one-byte enum field.
   pub fn set(&self, field: usize, value: u8) {
      self.put(field, value);
   }

   /// An `Option<Length>` or similar 8-byte field, [`gpui_px`] or [`W_FULL`].
   pub fn length(&self, field: usize, value: u64) {
      self.put(field, value);
   }

   pub fn lengths(&self, fields: &[usize], value: u64) {
      for &field in fields {
         self.length(field, value);
      }
   }

   /// `Some(Hsla)`: a `u32` tag, then four `f32`s.
   pub fn hsla(&self, field: usize, hsla: [f32; 4]) {
      if field + 20 <= self.at.bytes {
         self.put(field, 1u32);
         self.put(field + 4, hsla);
      }
   }

   /// `Some(f32)`: a `u32` tag, then the value.
   pub fn float(&self, field: usize, value: f32) {
      if field + 8 <= self.at.bytes {
         self.put(field, 1u32);
         self.put(field + 4, value);
      }
   }

   pub fn shared_string(&self, field: usize, value: SharedString) {
      self.put(field, value);
   }

   /// `Some(Background::Solid(hsla))`: tag `0`, the color at `+8`, and zeroed
   /// gradient stops up to `+0x48`.
   pub fn solid_background(&self, hsla: [f32; 4]) {
      let field = self.at.background;
      if field + 0x48 <= self.at.bytes {
         self.put(field, 0u64);
         self.put(field + 8, hsla);
         self.put(field + 0x18, [0u64; 6]);
      }
   }
}

// ---------------------------------------------------------------------------
// Native GPUI / Mana Layout & Styling Helpers
// ---------------------------------------------------------------------------

struct Builder {
   sym: &'static GpuiSymbols,
}

impl Builder {
   #[expect(
      clippy::cast_ptr_alignment,
      reason = "alloc_or_oom returns memory aligned to align_of::<RcInner>()"
   )]
   fn alloc_rc_action(&self, action: Action) -> *mut RcInner {
      let id = register_action(action);
      // SAFETY: calls Discord's own `alloc_or_oom(align=8, size=24)` so
      // Discord's `Rc::drop` frees it with the matching allocator.
      unsafe {
         let raw = (self.sym.alloc_or_oom)(align_of::<RcInner>(), size_of::<RcInner>());
         let inner = raw.cast::<RcInner>();
         inner.write(RcInner {
            strong: 1,
            weak: 1,
            action_id: id,
         });
         inner
      }
   }

   fn raw_div(&self) -> RawDiv {
      let mut d = RawDiv::zeroed();
      // SAFETY: constructs an empty GPUI `Div` into a buffer that fits it.
      unsafe { (self.sym.div_new)(&raw mut d, self.sym.caller_loc) };
      d
   }

   fn style_div(&self, div: &mut RawDiv, f: impl FnOnce(&Style)) {
      // SAFETY: `div_style_mut` returns `Div`'s boxed `StyleRefinement`.
      let ptr = unsafe { (self.sym.div_style_mut)(&raw mut *div) };
      if !ptr.is_null() {
         f(&Style {
            ptr,
            at: &self.sym.layout.style,
         });
      }
   }

   fn v_stack(&self, gap_px: f32) -> RawDiv {
      let d = chain(self.sym.div_flex, self.raw_div());
      let mut d = chain(self.sym.div_flex_col, d);
      self.style_div(&mut d, |s| s.lengths(&s.at.gap, gpui_px(gap_px)));
      d
   }

   fn h_row(&self, gap_px: f32, space_between: bool) -> RawDiv {
      const ALIGN_CENTER: u8 = 4;
      const JUSTIFY_SPACE_BETWEEN: u8 = 6;
      let mut d = chain(self.sym.div_flex, self.raw_div());
      self.style_div(&mut d, |s| {
         s.lengths(&s.at.gap, gpui_px(gap_px));
         s.set(s.at.align_items, ALIGN_CENTER);
         if space_between {
            s.set(s.at.justify_content, JUSTIFY_SPACE_BETWEEN);
         }
      });
      d
   }

   /// Rounded bordered card container matching Discord's native Settings cards.
   fn card(&self, gap_px: f32, pad_px: f32, radius_px: f32, border_alpha: f32) -> RawDiv {
      let radius_px = if SQUARE.load(Ordering::Relaxed) {
         0.0
      } else {
         radius_px
      };
      let mut d = self.v_stack(gap_px);
      self.style_div(&mut d, |s| {
         s.lengths(&s.at.border_widths, gpui_px(1.0));
         s.lengths(&s.at.corner_radii, gpui_px(radius_px));
         s.lengths(&s.at.padding, gpui_px(pad_px));
         s.hsla(s.at.border_color, [0.62, 0.05, 0.55, border_alpha]);
      });
      d
   }

   /// Compact tinted tag matching Mana's 6px button geometry for non-confirmed
   /// desktop plugins, when `str_into_any` and `div_push_any` are available.
   fn compat_badge(&self, compat: DesktopCompat) -> Option<RawDiv> {
      let label = compat.tag()?;
      let (bg, text) = compat.colors()?;
      let mut badge = self.text_line(label, text, 12.0)?;
      self.style_div(&mut badge, |s| {
         s.solid_background(bg);
         s.lengths(&s.at.corner_radii, gpui_px(6.0));
         s.length(s.at.padding[0], gpui_px(4.0));
         s.length(s.at.padding[1], gpui_px(10.0));
         s.length(s.at.padding[2], gpui_px(4.0));
         s.length(s.at.padding[3], gpui_px(10.0));
      });
      Some(badge)
   }

   /// Subtle 1px horizontal divider line between sections or rows.
   fn divider(&self) -> RawDiv {
      let mut d = self.raw_div();
      self.style_div(&mut d, |s| {
         s.length(s.at.border_widths[2], gpui_px(1.0));
         s.length(s.at.size[1], gpui_px(1.0));
         s.hsla(s.at.border_color, [0.62, 0.05, 0.55, 0.16]);
      });
      d
   }

   fn push_div(&self, mut parent: RawDiv, mut child: RawDiv) -> RawDiv {
      let mut out = RawDiv::zeroed();
      // SAFETY: moves both `RawDiv`s into `div_child_div`.
      unsafe { (self.sym.div_child_div)(&raw mut out, &raw mut parent, &raw mut child) };
      out
   }

   fn build_button(
      &self,
      id: &str,
      label: &str,
      variant: BtnVariant,
      size: BtnSize,
      action: Action,
   ) -> RawButton {
      let rc_ptr = self.alloc_rc_action(action);
      new_button(
         self.sym,
         id,
         label,
         variant,
         size,
         &raw const BUTTON_FN_VTBL,
         rc_ptr,
      )
   }

   fn push_button(
      &self,
      mut parent: RawDiv,
      id: &str,
      label: &str,
      variant: BtnVariant,
      size: BtnSize,
      action: Action,
   ) -> RawDiv {
      let mut btn = self.build_button(id, label, variant, size, action);
      let mut out = RawDiv::zeroed();
      // SAFETY: appends native Mana `Button` directly into `parent`.
      unsafe { (self.sym.div_child_button)(&raw mut out, &raw mut parent, &raw mut btn) };
      out
   }

   fn text_block(&self, title: &str, subtitle: &str) -> RawDiv {
      let title = SharedString::from_str(title);
      let subtitle = SharedString::from_str(subtitle);
      let mut out = RawDiv::zeroed();
      // SAFETY: Mana's `TextBlock::new` takes both strings.
      unsafe { (self.sym.text_block)(&raw mut out, &raw const title, &raw const subtitle) };
      out
   }

   fn switch_row(
      &self,
      id: &str,
      label: &str,
      description: Option<&str>,
      checked: bool,
      action: Action,
   ) -> RawDiv {
      let at = &self.sym.layout.switch;
      let id_static = intern(id);
      let mut sw = RawSwitch::zeroed();
      // SAFETY: creates a native Mana `Switch` in a buffer that fits it.
      unsafe {
         (self.sym.switch_new)(
            &raw mut sw,
            id_static.as_ptr(),
            id_static.len(),
            u8::from(checked),
         );
      }
      // The defaults these overwrite are static `SharedString`s and `None`,
      // which own nothing.
      sw.put(at.label, SharedString::from_str(label));
      if let Some(desc) = description
         && !desc.is_empty()
      {
         sw.put(at.description, SharedString::from_str(desc));
      }
      sw.put(at.on_change, self.alloc_rc_action(action));
      sw.put(at.on_change + 8, &raw const SWITCH_FN_VTBL);

      let mut out = RawDiv::zeroed();
      // SAFETY: moves the `Switch` into its `Div`.
      unsafe { (self.sym.switch_into_div)(&raw mut out, &raw mut sw) };
      out
   }

   #[expect(clippy::too_many_arguments, reason = "one per GPUI row part")]
   fn action_row(
      &self,
      id: &str,
      title: &str,
      subtitle: &str,
      button_label: &str,
      variant: BtnVariant,
      size: BtnSize,
      action: Action,
   ) -> RawDiv {
      let title_ss = SharedString::from_str(title);
      let sub_ss = SharedString::from_str(subtitle);
      let mut btn = self.build_button(id, button_label, variant, size, action);
      let mut out = RawDiv::zeroed();
      // SAFETY: builds Discord's native `ActionRow(title, subtitle, button)`.
      unsafe {
         (self.sym.action_row)(
            &raw mut out,
            &raw const title_ss,
            &raw const sub_ss,
            &raw mut btn,
         );
      }
      out
   }

   fn write_any(&self, mut div: RawDiv, sret: *mut RawAnyElement) {
      // SAFETY: writes the 32-byte `gpui::AnyElement` directly to `sret`.
      unsafe {
         (self.sym.div_into_any)(sret, &raw mut div);
      }
   }

   fn push_any(&self, parent: &mut RawDiv, child: &mut RawAnyElement) -> bool {
      let Some(push) = self.sym.div_push_any else {
         return false;
      };
      // SAFETY: `children` is the `Div`'s `SmallVec<[AnyElement; 2]>`, decoded
      // from `div_child_div` and inside the `Div`.
      unsafe {
         let children = parent
            .as_mut_ptr()
            .add(self.sym.layout.div.children)
            .cast::<c_void>();
         push(children, child);
      }
      true
   }

   fn text_line(&self, text: &str, color_hsla: [f32; 4], font_px: f32) -> Option<RawDiv> {
      let str_into_any = self.sym.str_into_any?;
      let mut line_div = self.raw_div();
      self.style_div(&mut line_div, |s| {
         s.length(s.at.font_size, gpui_px(font_px));
         s.length(s.at.line_height, gpui_px(font_px + 4.0));
         s.hsla(s.at.text_color, color_hsla);
      });
      let mut ss = SharedString::from_str(text);
      // SAFETY: moves the `SharedString` into a 32-byte `AnyElement` and
      // pushes it.
      let mut any = unsafe {
         let mut out = MaybeUninit::<RawAnyElement>::uninit();
         str_into_any(out.as_mut_ptr(), &raw mut ss);
         out.assume_init()
      };
      if !self.push_any(&mut line_div, &mut any) {
         return None;
      }
      Some(line_div)
   }
}

/// Returns the resolved address of `Div::into_any_element` for sibling desktop
/// locators (`crate::modal`).
pub fn div_into_any_addr() -> Option<usize> {
   SYMBOLS.get().map(|s| s.div_into_any as usize)
}

pub fn raw_div() -> Option<RawDiv> {
   let sym = SYMBOLS.get()?;
   Some(Builder { sym }.raw_div())
}

/// Takes a freshly built `Div` out of the layout (`Display::None`, the last of block, flex, grid
/// and none).
///
/// # Safety
///
/// `div` points to a live `gpui::Div`, or the `Stateful<Div>` that wraps it.
pub unsafe fn display_none(div: *mut c_void) {
   const DISPLAY_NONE: u8 = 3;
   let Some(sym) = SYMBOLS.get() else {
      return;
   };
   // SAFETY: `div_style_mut` takes the `Div` by pointer, as `style_div` does.
   let ptr = unsafe { (sym.div_style_mut)(div.cast()) };
   if !ptr.is_null() {
      let style = Style {
         ptr,
         at: &sym.layout.style,
      };
      style.set(style.at.display, DISPLAY_NONE);
   }
}

pub fn style_div(div: &mut RawDiv, f: impl FnOnce(&Style)) {
   if let Some(sym) = SYMBOLS.get() {
      Builder { sym }.style_div(div, f);
   }
}

pub fn v_stack(gap_px: f32) -> Option<RawDiv> {
   let sym = SYMBOLS.get()?;
   Some(Builder { sym }.v_stack(gap_px))
}

pub fn h_row(gap_px: f32, space_between: bool) -> Option<RawDiv> {
   let sym = SYMBOLS.get()?;
   Some(Builder { sym }.h_row(gap_px, space_between))
}

pub fn push_div(parent: RawDiv, child: RawDiv) -> RawDiv {
   let Some(sym) = SYMBOLS.get() else {
      return parent;
   };
   Builder { sym }.push_div(parent, child)
}

pub fn push_any(parent: &mut RawDiv, child: &mut RawAnyElement) -> bool {
   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   Builder { sym }.push_any(parent, child)
}

pub fn str_to_any(text: &str) -> Option<RawAnyElement> {
   let str_into_any = SYMBOLS.get()?.str_into_any?;
   let mut ss = SharedString::from_str(text);
   // SAFETY: moves the `SharedString` into a 32-byte `AnyElement`.
   unsafe {
      let mut out = MaybeUninit::<RawAnyElement>::uninit();
      str_into_any(out.as_mut_ptr(), &raw mut ss);
      Some(out.assume_init())
   }
}

pub fn build_raw_button(
   id: &str,
   label: &str,
   variant: BtnVariant,
   size: BtnSize,
   vtbl: *const RcFnVtbl,
   action_id: u64,
) -> Option<RawButton> {
   let sym = SYMBOLS.get()?;
   let rc_ptr = alloc_rc_u64(action_id)?;
   Some(new_button(sym, id, label, variant, size, vtbl, rc_ptr))
}

/// A native Mana `Button` with our `variant`, `size` and `Rc<dyn Fn>` click
/// handler.
fn new_button(
   sym: &GpuiSymbols,
   id: &str,
   label: &str,
   variant: BtnVariant,
   size: BtnSize,
   vtbl: *const RcFnVtbl,
   rc_ptr: *mut RcInner,
) -> RawButton {
   let at = &sym.layout.button;
   let id_static = intern(id);
   let label_ss = SharedString::from_str(label);
   let mut btn = RawButton::zeroed();
   // SAFETY: constructs a native Mana `Button` in a buffer that fits it.
   unsafe {
      (sym.button_new)(
         &raw mut btn,
         id_static.as_ptr(),
         id_static.len(),
         &raw const label_ss,
      );
   }
   btn.put(at.variant, variant as u8);
   btn.put(at.scale, size as u8);
   btn.put(at.on_click, rc_ptr);
   btn.put(at.on_click + 8, vtbl);
   btn
}

pub fn push_raw_button(mut parent: RawDiv, mut btn: RawButton) -> RawDiv {
   let Some(sym) = SYMBOLS.get() else {
      return parent;
   };
   let mut out = RawDiv::zeroed();
   // SAFETY: appends native Mana `Button` directly into `parent`.
   unsafe { (sym.div_child_button)(&raw mut out, &raw mut parent, &raw mut btn) };
   out
}

pub fn write_any(div: RawDiv, sret: *mut RawAnyElement) {
   if let Some(sym) = SYMBOLS.get() {
      Builder { sym }.write_any(div, sret);
   }
}

/// Converts 32-bit ARGB (`0xAARRGGBB`) into GPUI's `[h, s, l, a]`
/// (`0.0..=1.0`).
#[expect(clippy::many_single_char_names, reason = "the HSL formula's own names")]
pub fn argb_to_hsla(argb: u32) -> [f32; 4] {
   let [alpha, r, g, b] = argb.to_be_bytes().map(|byte| f32::from(byte) / 255.0);
   let a = if alpha == 0.0 && argb != 0 {
      1.0
   } else {
      alpha
   };

   let max = r.max(g).max(b);
   let min = r.min(g).min(b);
   let delta = max - min;
   let l = f32::midpoint(max, min);

   let (h, s) = if delta == 0.0 {
      (0.0, 0.0)
   } else {
      let s = delta / (1.0 - 2.0f32.mul_add(l, -1.0).abs());
      let mut h = if (max - r).abs() < f32::EPSILON {
         ((g - b) / delta) % 6.0
      } else if (max - g).abs() < f32::EPSILON {
         (b - r) / delta + 2.0
      } else {
         (r - g) / delta + 4.0
      };
      h /= 6.0;
      if h < 0.0 {
         h += 1.0;
      }
      (h, s)
   };
   [h, s, l, a]
}

/// Allocates an `RcInner { strong: 1, weak: 1, action_id: value }` on
/// `Discord.exe`'s own heap so Discord's `Rc::drop` frees it cleanly.
pub fn alloc_rc_u64(value: u64) -> Option<*mut RcInner> {
   let sym = SYMBOLS.get()?;
   // SAFETY: uses Discord's resolved `alloc_or_oom(align, size)` for a 24-byte
   // `RcInner` layout matching `std::rc::RcBox<u64>`.
   unsafe {
      #[expect(
         clippy::cast_ptr_alignment,
         reason = "allocated with `RcInner`'s alignment"
      )]
      let ptr = (sym.alloc_or_oom)(align_of::<RcInner>(), size_of::<RcInner>()).cast::<RcInner>();
      if ptr.is_null() {
         return None;
      }
      ptr.write(RcInner {
         strong: 1,
         weak: 1,
         action_id: value,
      });
      Some(ptr)
   }
}

/// Whether `div_push_any` resolved, which every native modal needs to take
/// Discord's own `Modal` element as a child.
#[must_use]
pub fn supports_push_any() -> bool {
   SYMBOLS.get().is_some_and(|s| s.div_push_any.is_some())
}

/// Whether `div_push_any` and `str_into_any` resolved for native message
/// accessories.
#[must_use]
pub fn supports_accessories() -> bool {
   SYMBOLS
      .get()
      .is_some_and(|s| s.div_push_any.is_some() && s.str_into_any.is_some())
}

/// Wraps a rendered message row `RawAnyElement` in place with native GPUI
/// accessory lines underneath and an optional tinted left accent bar.
pub fn wrap_with_accessories(
   sret: *mut RawAnyElement,
   lines: &[(String, Option<u32>)],
   tint: Option<u32>,
) -> bool {
   const MUTED: [f32; 4] = [0.60, 0.08, 0.62, 0.85];

   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   if sym.div_push_any.is_none() || sym.str_into_any.is_none() {
      return false;
   }
   let b = Builder { sym };

   // SAFETY: takes ownership of the original rendered `AnyElement` out of
   // `sret`; we write the replacement `AnyElement` back into `sret` before
   // returning.
   let mut orig = unsafe { ptr::read(sret) };

   let mut outer = b.v_stack(2.0);
   if let Some(argb) = tint {
      let hsla = argb_to_hsla(argb);
      // The text color is inherited by child text runs.
      b.style_div(&mut outer, |s| {
         s.hsla(s.at.text_color, hsla);
         s.length(s.at.border_widths[3], gpui_px(3.0));
         s.hsla(s.at.border_color, hsla);
         // A faint wash of the tint behind the whole row.
         let [h, sat, l, _] = hsla;
         s.solid_background([h, sat, l, 0.12]);
      });
   }
   if !b.push_any(&mut outer, &mut orig) {
      // SAFETY: restore original element if push failed.
      unsafe { ptr::write(sret, orig) };
      return false;
   }

   if !lines.is_empty() {
      let mut acc_box = b.v_stack(2.0);
      // Indent accessories under the message text column.
      b.style_div(&mut acc_box, |s| {
         let [_, right, bottom, left] = s.at.padding;
         s.length(right, gpui_px(16.0));
         s.length(bottom, gpui_px(4.0));
         s.length(left, gpui_px(72.0));
      });
      for (text, color) in lines {
         let hsla = color.map_or(MUTED, argb_to_hsla);
         for subline in text.lines() {
            if subline.is_empty() {
               continue;
            }
            if let Some(line_div) = b.text_line(subline, hsla, 12.0) {
               acc_box = b.push_div(acc_box, line_div);
            }
         }
      }
      outer = b.push_div(outer, acc_box);
   }

   b.write_any(outer, sret);
   true
}

/// Muted accessory text, for lines without a color of their own.
const ACCESSORY_MUTED: [f32; 4] = [0.60, 0.08, 0.62, 0.85];

/// One piece of an inline accessory: SVG icons when it has them and the build
/// can draw them, else its text.
#[derive(Clone, Debug)]
pub struct Piece {
   pub text: String,
   pub color: Option<u32>,
   /// Asset paths from [`crate::icons::register`], each with its own color.
   pub icons: Vec<(&'static str, Option<u32>)>,
}

/// GPUI's `Svg` builders and layout, resolved by [`locate_svg`].
struct SvgSymbols {
   new: SvgNewFn,
   path: SvgPathFn,
   into_any: SvgIntoAnyFn,
   style_box: usize,
}

type RawSvg = Scratch<{ layout::SVG_CAP }>;
type SvgNewFn = unsafe extern "C" fn(*mut RawSvg, *const c_void) -> *mut RawSvg;
type SvgPathFn = unsafe extern "C" fn(*mut RawSvg, *mut RawSvg, *const u8, usize) -> *mut RawSvg;
type SvgIntoAnyFn = unsafe extern "C" fn(*mut RawAnyElement, *mut RawSvg) -> *mut RawAnyElement;

static SVG: OnceLock<SvgSymbols> = OnceLock::new();

/// Resolves the `Svg` builders, after [`locate`].
pub fn locate_svg(image: &Image<'static>) {
   let found = match desktop::svg_builders(image) {
      Ok(found) => found,
      Err(err) => {
         log::warn!("desktop gpui: {err}, icons fall back to text");
         return;
      },
   };
   if found.size > layout::SVG_CAP {
      log::warn!(
         "desktop gpui: Svg is {:#x} bytes, over {:#x}",
         found.size,
         layout::SVG_CAP
      );
      return;
   }
   // SAFETY: decoded from the code drawing a bundled icon, which calls them
   // with these Rust-ABI signatures.
   let symbols = unsafe {
      SvgSymbols {
         new: std::mem::transmute::<usize, SvgNewFn>(found.new),
         path: std::mem::transmute::<usize, SvgPathFn>(found.path),
         into_any: std::mem::transmute::<usize, SvgIntoAnyFn>(found.into_any),
         style_box: found.style_box,
      }
   };
   let _ = SVG.set(symbols);
   log::info!("desktop gpui: resolved Svg builders {found:x?}");
}

/// Whether icons can be drawn.
pub fn svg_ready() -> bool {
   SVG.get().is_some() && SYMBOLS.get().is_some()
}

/// `len` bytes from Discord's own allocator, so Discord can free them.
pub fn discord_alloc(len: usize) -> Option<*mut u8> {
   let sym = SYMBOLS.get()?;
   // SAFETY: Discord's `alloc_or_oom(align, size)`, which aborts rather than
   // returning null.
   let ptr = unsafe { (sym.alloc_or_oom)(1, len.max(1)) };
   (!ptr.is_null()).then_some(ptr)
}

/// An `svg().path(path)` of `px` pixels filled with `color`, as an
/// `AnyElement`.
fn svg_icon(
   b: &Builder,
   svg: &SvgSymbols,
   path: &'static str,
   px: f32,
   color: [f32; 4],
) -> RawAnyElement {
   let mut fresh = RawSvg::zeroed();
   let mut icon = RawSvg::zeroed();
   // SAFETY: `svg()` builds into a buffer that fits an `Svg`, and `path` moves
   // it into the second with a `'static` path.
   unsafe {
      (svg.new)(&raw mut fresh, b.sym.caller_loc);
      (svg.path)(&raw mut icon, &raw mut fresh, path.as_ptr(), path.len());
   }
   // SAFETY: the `Svg`'s boxed `StyleRefinement`, at the decoded offset.
   let style = unsafe {
      icon
         .as_mut_ptr()
         .add(svg.style_box)
         .cast::<*mut u8>()
         .read_unaligned()
   };
   if !style.is_null() {
      let s = Style {
         ptr: style,
         at: &b.sym.layout.style,
      };
      s.lengths(&s.at.size, gpui_px(px));
      s.float(s.at.flex_shrink, 0.0);
      s.hsla(s.at.text_color, color);
   }
   let mut any = MaybeUninit::<RawAnyElement>::uninit();
   // SAFETY: moves the `Svg` into a 32-byte `AnyElement`.
   unsafe {
      (svg.into_any)(any.as_mut_ptr(), &raw mut icon);
      any.assume_init()
   }
}

/// `pieces` side by side in a row: each piece's icons when it has any and
/// they can be drawn, else its text.
fn inline_row(b: &Builder, pieces: &[Piece], font_px: f32) -> RawDiv {
   let mut row = b.h_row(4.0, false);
   for piece in pieces {
      let hsla = piece.color.map_or(ACCESSORY_MUTED, argb_to_hsla);
      if let Some(svg) = SVG.get().filter(|_| !piece.icons.is_empty()) {
         let mut icons = b.h_row(2.0, false);
         for &(path, color) in &piece.icons {
            let mut icon = svg_icon(
               b,
               svg,
               path,
               font_px + 3.0,
               color.map_or(hsla, argb_to_hsla),
            );
            b.push_any(&mut icons, &mut icon);
         }
         row = b.push_div(row, icons);
      } else if !piece.text.is_empty()
         && let Some(line) = b.text_line(&piece.text, hsla, font_px)
      {
         row = b.push_div(row, line);
      }
   }
   row
}

/// Adds `pieces` as one more child of a `Div` Discord built by value, like the
/// member name's row right after the name, `lead` pixels after the child
/// before them.
pub fn append_pieces(div: *mut c_void, pieces: &[Piece], lead: f32) -> bool {
   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   let Some(push) = sym.div_push_any else {
      return false;
   };
   if div.is_null() || sym.str_into_any.is_none() {
      return false;
   }
   let b = Builder { sym };
   let mut row = inline_row(&b, pieces, 13.0);
   if lead > 0.0 {
      b.style_div(&mut row, |s| s.length(s.at.padding[3], gpui_px(lead)));
   }
   let mut any = MaybeUninit::<RawAnyElement>::uninit();
   b.write_any(row, any.as_mut_ptr());
   // SAFETY: `div` is a live `Div` that `Div::child` just returned, whose
   // children sit at the decoded offset; `push` moves the element into them.
   unsafe {
      let children = div
         .cast::<u8>()
         .add(sym.layout.div.children)
         .cast::<c_void>();
      push(children, any.as_mut_ptr());
   }
   true
}

// ---------------------------------------------------------------------------
// Ruby Categorized Native GPUI Settings Page
// ---------------------------------------------------------------------------

const DESKTOP_FULL: &[&str] = &[
   "silent-typing",
   "message-latency",
   "message-logger",
   "no-profile-themes",
   "no-reply-mention",
   "spoiler-mode",
   "text-replace",
   "view-raw",
   "global-badges",
   "dearrow",
   "translate",
   "clear-urls",
   "fix-embeds",
   "platform-indicators",
];

const DESKTOP_PARTIAL: &[&str] = &["fakenitro"];

const MOBILE_ONLY: &[&str] = &["hide-composer-buttons", "reviewdb", "taptap", "example"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum DesktopCompat {
   Full,
   Partial,
   MobileOnly,
   Unverified,
}

impl DesktopCompat {
   fn of(name: &str) -> Self {
      if DESKTOP_FULL.contains(&name) {
         Self::Full
      } else if DESKTOP_PARTIAL.contains(&name) {
         Self::Partial
      } else if MOBILE_ONLY.contains(&name) {
         Self::MobileOnly
      } else {
         Self::Unverified
      }
   }

   const fn tag(self) -> Option<&'static str> {
      match self {
         Self::Full => None,
         Self::Partial => Some("Partial Desktop"),
         Self::MobileOnly => Some("Mobile Only"),
         Self::Unverified => Some("Unverified"),
      }
   }

   /// `(background, text)` HSLA colors for non-full compat badges.
   const fn colors(self) -> Option<([f32; 4], [f32; 4])> {
      match self {
         Self::Full => None,
         // Warm amber
         Self::Partial => Some(([0.10, 0.85, 0.52, 0.14], [0.11, 0.92, 0.72, 1.0])),
         // Soft violet/purple — distinct from neutral buttons and red Remove
         Self::MobileOnly => Some(([0.76, 0.72, 0.62, 0.18], [0.76, 0.86, 0.82, 1.0])),
         // Soft sky blue
         Self::Unverified => Some(([0.58, 0.78, 0.60, 0.14], [0.58, 0.88, 0.80, 1.0])),
      }
   }
}

/// Renders the categorized native Ruby Settings page into `sret`.
pub fn render_ruby_page(sret: *mut RawAnyElement) -> bool {
   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   let b = Builder { sym };

   ACTIONS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clear();

   let (tab, filter, expanded_plugins, open_select, cat_desktop, cat_partial, cat_mobile) = {
      let ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
      (
         ui.tab,
         ui.filter,
         ui.expanded_plugins.clone(),
         ui.open_select.clone(),
         ui.cat_desktop_open,
         ui.cat_partial_open,
         ui.cat_mobile_open,
      )
   };

   let plugins = phoenix_runtime::plugins();
   let enabled_count = plugins.iter().filter(|p| p.enabled).count();
   let disabled_count = plugins.len().saturating_sub(enabled_count);

   SQUARE.store(theme::effects().square, Ordering::Relaxed);
   let themes = theme::list();

   let mut root = b.v_stack(16.0);

   // 1. Header + Category Tab Bar
   let summary = format!(
      "v{} · {enabled_count}/{} plugins active · {} for DevTools",
      update::VERSION,
      plugins.len(),
      phoenix_ui_desktop::DEVTOOLS_SHORTCUT,
   );
   root = b.push_div(root, b.text_block("Ruby", &summary));
   if let Some(label) = b.text_line("RUBY SETTINGS", ACCESSORY_MUTED, 12.0) {
      root = b.push_div(root, label);
   }

   let variant_for = |t: Tab| {
      if tab == t {
         BtnVariant::Primary
      } else {
         BtnVariant::Secondary
      }
   };
   let mut tab_bar = b.h_row(8.0, false);
   tab_bar = b.push_button(
      tab_bar,
      "ruby-tab-plugins",
      &format!("Plugins ({})", plugins.len()),
      variant_for(Tab::Plugins),
      BtnSize::Small,
      Action::SetTab(Tab::Plugins),
   );
   tab_bar = b.push_button(
      tab_bar,
      "ruby-tab-themes",
      &format!("Themes ({})", themes.len()),
      variant_for(Tab::Themes),
      BtnSize::Small,
      Action::SetTab(Tab::Themes),
   );
   tab_bar = b.push_button(
      tab_bar,
      "ruby-tab-catalog",
      "Plugin Catalog",
      variant_for(Tab::Catalog),
      BtnSize::Small,
      Action::SetTab(Tab::Catalog),
   );
   tab_bar = b.push_button(
      tab_bar,
      "ruby-tab-general",
      "General & Updates",
      variant_for(Tab::General),
      BtnSize::Small,
      Action::SetTab(Tab::General),
   );
   root = b.push_div(root, tab_bar);
   root = b.push_div(root, b.divider());

   // 2. Active Category Content
   match tab {
      Tab::Plugins => {
         root = render_plugins_tab(
            &b,
            root,
            &plugins,
            enabled_count,
            disabled_count,
            filter,
            &expanded_plugins,
            open_select.as_ref(),
         );
      },
      Tab::Themes => {
         root = render_themes_tab(&b, root, &themes);
      },
      Tab::Catalog => {
         let installed: HashSet<String> = plugins.iter().map(|p| p.name.clone()).collect();
         root = render_catalog_tab(&b, root, &installed, cat_desktop, cat_partial, cat_mobile);
      },
      Tab::General => {
         root = render_general_tab(&b, root);
      },
   }

   b.write_any(root, sret);
   true
}

#[expect(
   clippy::too_many_arguments,
   clippy::too_many_lines,
   reason = "one GPUI element tree, built top to bottom from the page state"
)]
fn render_plugins_tab(
   b: &Builder,
   mut root: RawDiv,
   plugins: &[phoenix_runtime::PluginInfo],
   enabled_count: usize,
   disabled_count: usize,
   filter: PluginFilter,
   expanded: &HashSet<String>,
   open_select: Option<&(String, String)>,
) -> RawDiv {
   // Filter pills + Reload All toolbar
   let filt_var = |f: PluginFilter| {
      if filter == f {
         BtnVariant::Primary
      } else {
         BtnVariant::Secondary
      }
   };
   let mut left_pills = b.h_row(6.0, false);
   left_pills = b.push_button(
      left_pills,
      "ruby-filter-all",
      &format!("All ({})", plugins.len()),
      filt_var(PluginFilter::All),
      BtnSize::Small,
      Action::SetFilter(PluginFilter::All),
   );
   left_pills = b.push_button(
      left_pills,
      "ruby-filter-enabled",
      &format!("Enabled ({enabled_count})"),
      filt_var(PluginFilter::Enabled),
      BtnSize::Small,
      Action::SetFilter(PluginFilter::Enabled),
   );
   left_pills = b.push_button(
      left_pills,
      "ruby-filter-disabled",
      &format!("Disabled ({disabled_count})"),
      filt_var(PluginFilter::Disabled),
      BtnSize::Small,
      Action::SetFilter(PluginFilter::Disabled),
   );

   let mut right_actions = b.h_row(6.0, false);
   right_actions = b.push_button(
      right_actions,
      "ruby-toolbar-install-url",
      "+ URL",
      BtnVariant::Primary,
      BtnSize::Small,
      Action::OpenInstallUrl,
   );
   right_actions = b.push_button(
      right_actions,
      "ruby-toolbar-install-file",
      "+ File...",
      BtnVariant::Secondary,
      BtnSize::Small,
      Action::InstallFromFile,
   );
   right_actions = b.push_button(
      right_actions,
      "ruby-toolbar-reload-all",
      "Reload All",
      BtnVariant::Secondary,
      BtnSize::Small,
      Action::ReloadAll,
   );

   let mut toolbar = b.h_row(8.0, true);
   toolbar = b.push_div(toolbar, left_pills);
   toolbar = b.push_div(toolbar, right_actions);
   root = b.push_div(root, toolbar);

   let filtered: Vec<&phoenix_runtime::PluginInfo> = plugins
      .iter()
      .filter(|p| match filter {
         PluginFilter::All => true,
         PluginFilter::Enabled => p.enabled,
         PluginFilter::Disabled => !p.enabled,
      })
      .collect();

   if filtered.is_empty() {
      let msg = if plugins.is_empty() {
         "Open the Plugin Catalog tab to install plugins."
      } else {
         "No plugins match the current filter."
      };
      let mut empty = b.card(8.0, 16.0, 8.0, 0.18);
      empty = b.push_div(empty, b.text_block("Nothing here", msg));
      return b.push_div(root, empty);
   }

   let mut list = b.v_stack(10.0);
   for plugin in filtered {
      let is_open = expanded.contains(&plugin.name);
      let compat = DesktopCompat::of(&plugin.name);
      let badge = b.compat_badge(compat);
      let mut card = b.card(10.0, 14.0, 8.0, if plugin.enabled { 0.22 } else { 0.14 });

      let mut title = plugin.header.version.as_ref().map_or_else(
         || plugin.name.clone(),
         |ver| format!("{}  v{ver}", plugin.name),
      );
      if badge.is_none()
         && let Some(tag) = compat.tag()
      {
         title.push_str("  [");
         title.push_str(tag);
         title.push(']');
      }
      let mut desc_parts = Vec::new();
      if let Some(d) = &plugin.header.description {
         desc_parts.push(d.clone());
      }
      if let Some(a) = &plugin.header.author {
         desc_parts.push(format!("by {a}"));
      }
      if badge.is_none() {
         match compat {
            DesktopCompat::Full => {},
            DesktopCompat::Partial => desc_parts.push("Partial desktop support".to_owned()),
            DesktopCompat::MobileOnly => {
               desc_parts.push("Targets mobile UI; may not work on desktop".to_owned());
            },
            DesktopCompat::Unverified => desc_parts.push("Unverified on desktop".to_owned()),
         }
      }
      if let Some(err) = &plugin.error {
         desc_parts.push(format!("Error: {err}"));
      }
      let desc = desc_parts.join(" · ");

      card = b.push_div(
         card,
         b.switch_row(
            &format!("ruby-plugin-{}", plugin.name),
            &title,
            (!desc.is_empty()).then_some(desc.as_str()),
            plugin.enabled,
            Action::TogglePlugin(plugin.name.clone()),
         ),
      );

      let has_settings = plugin.enabled && !plugin.settings.is_empty();
      let mut buttons = b.h_row(8.0, false);
      if has_settings {
         let lbl = if is_open {
            format!("▾ Hide Settings ({})", plugin.settings.len())
         } else {
            format!("▸ Settings ({})", plugin.settings.len())
         };
         buttons = b.push_button(
            buttons,
            &format!("ruby-expand-{}", plugin.name),
            &lbl,
            if is_open {
               BtnVariant::Primary
            } else {
               BtnVariant::Secondary
            },
            BtnSize::Small,
            Action::ToggleExpandPlugin(plugin.name.clone()),
         );
      }
      buttons = b.push_button(
         buttons,
         &format!("ruby-edit-{}", plugin.name),
         "Edit Code",
         BtnVariant::Secondary,
         BtnSize::Small,
         Action::EditPluginCode(plugin.name.clone()),
      );
      if plugin.enabled && plugin.installed {
         buttons = b.push_button(
            buttons,
            &format!("ruby-reload-{}", plugin.name),
            "Reload",
            BtnVariant::Secondary,
            BtnSize::Small,
            Action::ReloadPlugin(plugin.name.clone()),
         );
      }
      if !plugin.enabled && plugin.installed {
         buttons = b.push_button(
            buttons,
            &format!("ruby-remove-{}", plugin.name),
            "Remove",
            BtnVariant::Critical,
            BtnSize::Small,
            Action::UninstallPlugin(plugin.name.clone()),
         );
      }
      let footer = if let Some(badge_div) = badge {
         let mut row = b.h_row(8.0, true);
         row = b.push_div(row, buttons);
         b.push_div(row, badge_div)
      } else {
         buttons
      };
      card = b.push_div(card, footer);

      // Collapsible per-plugin settings dropdown drawer
      if has_settings && is_open {
         card = b.push_div(card, b.divider());
         let mut drawer = b.card(10.0, 12.0, 6.0, 0.14);
         for setting in &plugin.settings {
            let sid = format!("ruby-set-{}-{}", plugin.name, setting.key);
            let sdesc = setting.description.as_deref().unwrap_or("");
            // A text, number or secret setting: its value, and Edit for an
            // input.
            let edit_row = |shown: &str, initial: String, kind: FieldKind| {
               let sub = if sdesc.is_empty() {
                  shown.to_owned()
               } else {
                  format!("{sdesc} · {shown}")
               };
               b.action_row(
                  &sid,
                  &setting.title,
                  &sub,
                  "Edit",
                  BtnVariant::Secondary,
                  BtnSize::Small,
                  Action::EditSetting(SettingField {
                     plugin: plugin.name.clone(),
                     key: setting.key.clone(),
                     title: setting.title.clone(),
                     initial,
                     kind,
                  }),
               )
            };
            match &setting.control {
               Control::Switch(val) => {
                  drawer = b.push_div(
                     drawer,
                     b.switch_row(
                        &sid,
                        &setting.title,
                        setting.description.as_deref(),
                        *val,
                        Action::ToggleSetting {
                           plugin: plugin.name.clone(),
                           key: setting.key.clone(),
                        },
                     ),
                  );
               },
               Control::Select { options, chosen } => {
                  let cur_label = options
                     .iter()
                     .find(|o| o.value == *chosen)
                     .map_or(chosen.as_str(), |o| o.label.as_str());
                  let dropdown_open =
                     open_select.is_some_and(|(p, k)| p == &plugin.name && k == &setting.key);
                  let trigger_lbl = if dropdown_open {
                     format!("{cur_label} ▴")
                  } else {
                     format!("{cur_label} ▾")
                  };
                  drawer = b.push_div(
                     drawer,
                     b.action_row(
                        &sid,
                        &setting.title,
                        sdesc,
                        &trigger_lbl,
                        if dropdown_open {
                           BtnVariant::Primary
                        } else {
                           BtnVariant::Secondary
                        },
                        BtnSize::Small,
                        Action::ToggleSelectDropdown {
                           plugin: plugin.name.clone(),
                           key: setting.key.clone(),
                        },
                     ),
                  );
                  if dropdown_open && !options.is_empty() {
                     let mut menu = b.card(6.0, 8.0, 6.0, 0.24);
                     for opt in options {
                        let selected = opt.value == *chosen;
                        let opt_lbl = if selected {
                           format!("✓  {}", opt.label)
                        } else {
                           opt.label.clone()
                        };
                        menu = b.push_button(
                           menu,
                           &format!("{sid}-opt-{}", opt.value),
                           &opt_lbl,
                           if selected {
                              BtnVariant::Primary
                           } else {
                              BtnVariant::Secondary
                           },
                           BtnSize::Small,
                           Action::PickSelectOption {
                              plugin: plugin.name.clone(),
                              key: setting.key.clone(),
                              value: opt.value.clone(),
                           },
                        );
                     }
                     drawer = b.push_div(drawer, menu);
                  }
               },
               Control::Button => {
                  drawer = b.push_div(
                     drawer,
                     b.action_row(
                        &sid,
                        &setting.title,
                        sdesc,
                        &setting.title,
                        BtnVariant::Secondary,
                        BtnSize::Small,
                        Action::PressButton {
                           plugin: plugin.name.clone(),
                           key: setting.key.clone(),
                        },
                     ),
                  );
               },
               Control::Text(val) => {
                  let count = val.lines().filter(|line| !line.trim().is_empty()).count();
                  let shown = match (setting.multiline, count) {
                     (_, 0) => "empty".to_owned(),
                     (true, 1) => "1 line".to_owned(),
                     (true, n) => format!("{n} lines"),
                     (false, _) => val.clone(),
                  };
                  let kind = if setting.multiline {
                     FieldKind::Lines
                  } else {
                     FieldKind::Text
                  };
                  drawer = b.push_div(drawer, edit_row(&shown, val.clone(), kind));
               },
               Control::Secret(val) => {
                  let shown = if val.is_empty() { "not set" } else { "set" };
                  drawer = b.push_div(drawer, edit_row(shown, String::new(), FieldKind::Secret));
               },
               Control::Number(val) => {
                  let shown = val.to_string();
                  drawer = b.push_div(drawer, edit_row(&shown, shown.clone(), FieldKind::Number));
               },
            }
         }
         card = b.push_div(card, drawer);
      }

      list = b.push_div(list, card);
   }

   b.push_div(root, list)
}

#[expect(clippy::too_many_arguments, reason = "one per GPUI row part")]
fn render_catalog_group(
   b: &Builder,
   mut root: RawDiv,
   group_id: u8,
   title: &str,
   subtitle: &str,
   items: &[&Listed],
   installed: &HashSet<String>,
   open: bool,
) -> RawDiv {
   if items.is_empty() {
      return root;
   }
   let btn_lbl = if open {
      format!("▾ Hide ({})", items.len())
   } else {
      format!("▸ Show ({})", items.len())
   };
   let mut section = b.card(10.0, 14.0, 8.0, 0.18);
   section = b.push_div(
      section,
      b.action_row(
         &format!("ruby-cat-group-{group_id}"),
         title,
         subtitle,
         &btn_lbl,
         BtnVariant::Secondary,
         BtnSize::Small,
         Action::ToggleCatalogGroup(group_id),
      ),
   );
   if open {
      section = b.push_div(section, b.divider());
      let mut list = b.v_stack(8.0);
      for item in items {
         let is_installed = installed.contains(&item.name);
         let compat = DesktopCompat::of(&item.name);
         let sub = match &item.warning {
            Some(w) if !w.is_empty() => format!("{} · Note: {w}", item.description),
            _ => item.description.clone(),
         };
         let mut row_card = b.card(8.0, 12.0, 6.0, 0.14);
         row_card = b.push_div(
            row_card,
            b.action_row(
               &format!("ruby-cat-{}", item.name),
               &item.name,
               &sub,
               if is_installed { "Reinstall" } else { "Install" },
               if is_installed {
                  BtnVariant::Secondary
               } else {
                  BtnVariant::Primary
               },
               BtnSize::Small,
               Action::InstallCatalog(item.name.clone()),
            ),
         );
         if let Some(badge) = b.compat_badge(compat) {
            let tag_row = b.push_div(b.h_row(6.0, false), badge);
            row_card = b.push_div(row_card, tag_row);
         }
         list = b.push_div(list, row_card);
      }
      section = b.push_div(section, list);
   }
   root = b.push_div(root, section);
   root
}

#[expect(
   clippy::too_many_lines,
   reason = "one GPUI element tree, built top to bottom"
)]
fn render_catalog_tab(
   b: &Builder,
   mut root: RawDiv,
   installed: &HashSet<String>,
   desktop_open: bool,
   partial_open: bool,
   mobile_open: bool,
) -> RawDiv {
   root = b.push_div(
      root,
      b.action_row(
         "ruby-catalog-refresh",
         "Official Plugin Catalog",
         "Install plugins from a configured update server.",
         "Refresh",
         BtnVariant::Secondary,
         BtnSize::Small,
         Action::RefreshCatalog,
      ),
   );

   let catalog = update::catalog();
   match catalog {
      Catalog::Unfetched => {
         update::fetch_catalog();
         let mut box_div = b.card(8.0, 14.0, 8.0, 0.18);
         box_div = b.push_div(
            box_div,
            b.text_block("Fetching catalog...", "Loading the plugin list."),
         );
         root = b.push_div(root, box_div);
      },
      Catalog::Loading => {
         let mut box_div = b.card(8.0, 14.0, 8.0, 0.18);
         box_div = b.push_div(
            box_div,
            b.text_block("Fetching catalog...", "Loading the plugin list."),
         );
         root = b.push_div(root, box_div);
      },
      Catalog::Failed(err) => {
         let mut box_div = b.card(8.0, 14.0, 8.0, 0.22);
         box_div = b.push_div(
            box_div,
            b.action_row(
               "ruby-catalog-retry",
               "Could not reach catalog",
               &err,
               "Retry",
               BtnVariant::Primary,
               BtnSize::Small,
               Action::RefreshCatalog,
            ),
         );
         root = b.push_div(root, box_div);
      },
      Catalog::Loaded(items) => {
         let desktop: Vec<&Listed> = items
            .iter()
            .filter(|i| DesktopCompat::of(&i.name) == DesktopCompat::Full)
            .collect();
         let partial: Vec<&Listed> = items
            .iter()
            .filter(|i| DesktopCompat::of(&i.name) == DesktopCompat::Partial)
            .collect();
         let other: Vec<&Listed> = items
            .iter()
            .filter(|i| {
               matches!(
                  DesktopCompat::of(&i.name),
                  DesktopCompat::MobileOnly | DesktopCompat::Unverified
               )
            })
            .collect();

         root = render_catalog_group(
            b,
            root,
            0,
            "Desktop Ready",
            "Confirmed working on Desktop Phoenix.",
            &desktop,
            installed,
            desktop_open,
         );
         root = render_catalog_group(
            b,
            root,
            1,
            "Partial Desktop Support",
            "Core hooks work on desktop; some features are still pending or unconfirmed.",
            &partial,
            installed,
            partial_open,
         );
         root = render_catalog_group(
            b,
            root,
            2,
            "Mobile Only & Unverified",
            "Targets Android Phoenix's UI tree or has not been confirmed on desktop yet.",
            &other,
            installed,
            mobile_open,
         );
      },
   }
   root
}

/// What a theme does, and which of it this Discord build can't do.
fn theme_summary(theme: &theme::Listed) -> String {
   if let Some(error) = &theme.error {
      return format!("Not a valid theme: {error}");
   }
   let mut parts = Vec::new();
   if theme.square {
      parts.push("square corners (Ruby's pages only for now)".to_owned());
   }
   for id in &theme.hide {
      let known = theme::HIDES.iter().any(|(known, _)| known == id);
      parts.push(match (known, crate::hide::supports(id)) {
         (false, _) => format!("unknown id {id}"),
         (true, false) => format!("{id} (not supported on this Discord build yet)"),
         (true, true) => format!("hides {id}"),
      });
   }
   if parts.is_empty() {
      "Does nothing".to_owned()
   } else {
      parts.join(", ")
   }
}

fn render_themes_tab(b: &Builder, mut root: RawDiv, themes: &[theme::Listed]) -> RawDiv {
   let ids = theme::HIDES
      .iter()
      .map(|(id, _)| *id)
      .collect::<Vec<_>>()
      .join(", ");
   root = b.push_div(
      root,
      b.action_row(
         "ruby-themes-import",
         "Import a theme",
         &format!(
            "Pick a .json theme, like {{\"square\": true, \"hide\": [\"composer.gift\"]}}. \"hide\" takes: {ids}."
         ),
         "Import theme",
         BtnVariant::Secondary,
         BtnSize::Medium,
         Action::ImportTheme,
      ),
   );
   root = b.push_div(
      root,
      b.action_row(
         "ruby-themes-refresh",
         "Refresh themes",
         "The list follows the themes folder by itself: files you add, delete or edit show up here within a second.",
         "Refresh",
         BtnVariant::Secondary,
         BtnSize::Medium,
         Action::RefreshThemes,
      ),
   );
   let mut card = b.card(12.0, 16.0, 8.0, 0.20);
   if themes.is_empty() {
      if let Some(line) = b.text_line("No themes found.", ACCESSORY_MUTED, 13.0) {
         card = b.push_div(card, line);
      }
   }
   for (i, theme) in themes.iter().enumerate() {
      if i > 0 {
         card = b.push_div(card, b.divider());
      }
      card = b.push_div(
         card,
         b.switch_row(
            &format!("ruby-theme-{}", theme.name),
            &theme.name,
            Some(&theme_summary(theme)),
            theme.enabled,
            Action::ToggleTheme(theme.name.clone()),
         ),
      );
   }
   b.push_div(root, card)
}

fn render_general_tab(b: &Builder, mut root: RawDiv) -> RawDiv {
   // Card 1: Discord & Developer controls
   let mut dev_card = b.card(12.0, 16.0, 8.0, 0.20);
   dev_card = b.push_div(
      dev_card,
      b.switch_row(
         "ruby-staff-unlock",
         "Staff Unlock",
         Some("Unlock Discord staff-only settings, experiments, and internal developer menus."),
         phoenix_runtime::staff_enabled(),
         Action::ToggleStaff,
      ),
   );
   dev_card = b.push_div(dev_card, b.divider());
   dev_card = b.push_div(
      dev_card,
      b.action_row(
         "ruby-open-devtools",
         "Runtime Console & DevTools",
         &format!(
            "Evaluate JavaScript in QuickJS and view live runtime logs ({}).",
            phoenix_ui_desktop::DEVTOOLS_SHORTCUT
         ),
         "Open DevTools",
         BtnVariant::Secondary,
         BtnSize::Medium,
         Action::OpenDevTools,
      ),
   );
   dev_card = b.push_div(dev_card, b.divider());
   dev_card = b.push_div(
      dev_card,
      b.action_row(
         "ruby-reload-all",
         "Reload All Plugins",
         "Re-evaluate every currently enabled plugin without restarting Discord.",
         "Reload All",
         BtnVariant::Secondary,
         BtnSize::Medium,
         Action::ReloadAll,
      ),
   );
   root = b.push_div(root, dev_card);

   // Card 2: Updates
   let (upd_desc, upd_btn, upd_var, upd_act) = updates_card_state();

   let mut upd_card = b.card(10.0, 16.0, 8.0, 0.20);
   upd_card = b.push_div(
      upd_card,
      b.action_row(
         "ruby-updates",
         "Ruby Updates",
         &upd_desc,
         upd_btn,
         upd_var,
         BtnSize::Medium,
         upd_act,
      ),
   );
   b.push_div(root, upd_card)
}

fn updates_card_state() -> (String, &'static str, BtnVariant, Action) {
   let status = update::status();
   if update::busy() {
      return (
         status.unwrap_or_else(|| "Checking for updates...".to_owned()),
         "Working...",
         BtnVariant::Secondary,
         Action::CheckUpdates,
      );
   }
   if let Some(found) = update::found() {
      if !found.plugins.is_empty() {
         let desc = match &found.loader {
            Some(version) => {
               format!(
                  "Plugin updates ready: {} · Ruby {version} available (restart to update)",
                  found.plugins.join(", ")
               )
            },
            None => format!("Plugin updates ready: {}", found.plugins.join(", ")),
         };
         return (
            desc,
            "Apply Updates",
            BtnVariant::Primary,
            Action::ApplyUpdates,
         );
      }
      if let Some(version) = found.loader {
         return (
            format!("Ruby {version} is available — restart Ruby to update."),
            "Check for Updates",
            BtnVariant::Secondary,
            Action::CheckUpdates,
         );
      }
   }
   (
      status.unwrap_or_else(|| format!("Ruby {} is up to date.", update::VERSION)),
      "Check for Updates",
      BtnVariant::Secondary,
      Action::CheckUpdates,
   )
}
