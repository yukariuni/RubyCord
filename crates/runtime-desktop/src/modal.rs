//! Native GPUI / Mana modal overlay, syntax-colorized code viewer, and native
//! `TextInput` / `TextArea` editors for Desktop Phoenix.
//!
//! Hooks Discord's window root render (`modal-layer-scrim` ->
//! `WindowRoot::render`) and composes Discord's own
//! `mana_html::hosted::modal::Modal`, `TextInput`, and `TextArea` natively on
//! top of the active window — no Win32 popup windows and no `WebView2`
//! overlays.

use std::{
   ffi::c_void,
   mem::MaybeUninit,
   ptr, slice, str,
   sync::{
      Arc, Condvar, LazyLock, Mutex, OnceLock, PoisonError,
      atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
   },
   time::Duration,
};

use phoenix_hook::image::Image;
use phoenix_runtime::highlight::{self, JsonKind, Kind};
use phoenix_sigs::desktop::{self, FieldLayout, ModalLayout, layout};

use crate::{
   gpui::{
      self, BtnSize, BtnVariant, RawAnyElement, RawButton, RawDiv, RcFnVtbl, Scratch, SharedString,
      Style, W_FULL, argb_to_hsla, gpui_px, intern_str,
   },
   os::{DesktopOs, Os},
};

const MAX_HIGHLIGHT_LINES: usize = 600;
const MONO_FONT: &str = "gg sans mono";

// GitHub-Dark / Discord syntax palette (ARGB -> HSLA via `argb_to_hsla`).
const COL_PLAIN: u32 = 0xFFE6_EDF3;
const COL_KEY: u32 = 0xFF79_C0FF;
const COL_STRING: u32 = 0xFF7E_E787;
const COL_NUMBER: u32 = 0xFFFF_A657;
const COL_BOOL: u32 = 0xFFD2_A8FF;
const COL_NULL: u32 = 0xFFFF_7B72;
const COL_PUNCT: u32 = 0xFF8B_949E;
const COL_KEYWORD: u32 = 0xFFFF_7B72;
const COL_COMMENT: u32 = 0xFF8B_949E;
const COL_MUTED: u32 = 0xFF94_9BA4;
const COL_ERROR: u32 = 0xFFFF_7B72;
const COL_OK: u32 = 0xFF7E_E787;

// ---------------------------------------------------------------------------
// Public Modal Specs
// ---------------------------------------------------------------------------

/// A clickable action button inside a native GPUI page modal
/// (`phoenix.ui.openPage`).
#[derive(Clone)]
pub struct ModalButton {
   pub label: String,
   pub danger: bool,
   pub on_press: Arc<dyn Fn() + Send + Sync>,
}

/// A section inside a native GPUI page modal (`phoenix.ui.openPage`).
#[derive(Clone)]
pub enum ModalSection {
   Buttons {
      title: Option<String>,
      buttons: Vec<ModalButton>,
   },
   Text {
      title: Option<String>,
      text: String,
   },
   Code {
      title: Option<String>,
      text: String,
      monospace: bool,
      highlight: bool,
   },
}

/// Specification for a native GPUI plugin page (`phoenix.ui.openPage`).
#[derive(Clone)]
pub struct PageModalSpec {
   pub title: String,
   pub sections: Vec<ModalSection>,
}

/// A native `phoenix.ui.choose` / `showSheet` dialog whose answer a waiting
/// worker thread reads.
pub struct Choice {
   picked: Mutex<Option<i32>>,
   done: Condvar,
}

impl Choice {
   const fn new() -> Self {
      Self {
         picked: Mutex::new(None),
         done: Condvar::new(),
      }
   }

   /// Records `index` unless the dialog already answered.
   pub fn pick(&self, index: i32) {
      let mut picked = self.picked.lock().unwrap_or_else(PoisonError::into_inner);
      if picked.is_none() {
         *picked = Some(index);
      }
      drop(picked);
      self.done.notify_all();
   }

   /// Waits for the picked index, or `None` when `timeout` passes first.
   pub fn wait(&self, timeout: Duration) -> Option<i32> {
      let picked = self.picked.lock().unwrap_or_else(PoisonError::into_inner);
      let (picked, _) = self
         .done
         .wait_timeout_while(picked, timeout, |picked| picked.is_none())
         .unwrap_or_else(PoisonError::into_inner);
      *picked
   }
}

/// One button of a native choose dialog.
pub struct ChoiceOption {
   pub label: String,
   pub danger: bool,
}

#[derive(Clone)]
struct CodeEditorModal {
   plugin: String,
   pending_init: Option<String>,
   status: Option<(String, bool)>,
}

/// What a plugin's text, number or secret setting holds.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
   Text,
   /// Text over several lines, in a `TextArea`.
   Lines,
   Number,
   /// Starts empty rather than showing the stored value.
   Secret,
}

/// A plugin setting edited in the one-line field.
#[derive(Clone)]
pub struct SettingField {
   pub plugin: String,
   pub key: String,
   pub title: String,
   pub initial: String,
   pub kind: FieldKind,
}

/// A one-line field a plugin asks for, like `phoenix.ui.addProfileTab`'s
/// input block.
#[derive(Clone)]
pub struct Prompt {
   pub title: String,
   pub on_submit: Arc<dyn Fn(String) + Send + Sync>,
}

/// What the one-line field modal's text goes to.
#[derive(Clone)]
enum FieldTarget {
   Install,
   Setting(SettingField),
   Prompt(Prompt),
}

/// The one-line field modal: a plugin URL to install, a setting, or a prompt.
#[derive(Clone)]
struct InstallUrlModal {
   target: FieldTarget,
   pending_init: Option<String>,
   status: Option<(String, bool)>,
   busy: bool,
}

#[derive(Clone)]
enum ActiveModal {
   Page {
      id: u64,
      spec: PageModalSpec,
   },
   /// A choose dialog, over the page it returns to when answered.
   Choice {
      spec: PageModalSpec,
      choice: Arc<Choice>,
      under: Option<(u64, PageModalSpec)>,
   },
   CodeEditor(CodeEditorModal),
   InstallUrl(InstallUrlModal),
}

/// The active modal copied out of the state lock for one render pass. A choose
/// dialog draws as the page it is.
enum Snapshot {
   Page(PageModalSpec),
   CodeEditor(CodeEditorModal),
   InstallUrl(InstallUrlModal),
}

// ---------------------------------------------------------------------------
// ABI Mirrors & Resolved Discord Symbols
// ---------------------------------------------------------------------------

type RawModal = Scratch<{ layout::SCRATCH_CAP }>;
type RawTextInput = Scratch<{ layout::SCRATCH_CAP }>;
type RawTextArea = Scratch<{ layout::SCRATCH_CAP }>;

#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct RawEntity {
   id_lo: u32,
   id_hi: u32,
   rest: [u64; 3],
}
const _: () = assert!(size_of::<RawEntity>() == layout::ENTITY);

#[repr(C, align(8))]
struct RawContext {
   app: *mut c_void,
   weak: RawEntity,
}

#[repr(C)]
struct RawVecAny {
   cap: usize,
   ptr: *mut RawAnyElement,
   len: usize,
}

type OverlayRenderFn = unsafe extern "C" fn(
   sret: *mut RawVecAny,
   workspace: *mut c_void,
   arg3: *mut c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawVecAny;

type VecPushAnyFn = unsafe extern "C" fn(*mut RawVecAny, *mut RawAnyElement);

type ModalNewFn = unsafe extern "C" fn(*mut RawModal, *const SharedString) -> *mut RawModal;
type ModalIntoAnyFn = unsafe extern "C" fn(*mut RawAnyElement, *mut RawModal) -> *mut RawAnyElement;
type ModalChildDivFn =
   unsafe extern "C" fn(*mut RawModal, *mut RawModal, *mut RawDiv) -> *mut RawModal;
type ModalFooterBtnFn =
   unsafe extern "C" fn(*mut RawModal, *mut RawModal, *mut RawButton) -> *mut RawModal;
type DivIdFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, *const u8, usize) -> *mut RawDiv;

type CxNewFieldFn =
   unsafe extern "C" fn(*mut RawEntity, *mut c_void, *mut c_void) -> *mut RawEntity;
type EntityMapGetFn = unsafe extern "C" fn(*mut c_void, u32, u32) -> *mut c_void;
type EntityDowngradeFn = unsafe extern "C" fn(*mut RawEntity, *const RawEntity) -> *mut RawEntity;
type WeakDropFn = unsafe extern "C" fn(*mut c_void);
type TextGetFn =
   unsafe extern "C" fn(*mut SharedString, *const c_void, *const c_void) -> *mut SharedString;
type SetValueFn = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut c_void, *mut c_void);
type UpdateEntityDynFn = unsafe extern "C" fn(
   *mut c_void,
   *const RawEntity,
   *const u8,
   usize,
   *mut c_void,
   *const RcFnVtbl,
);
type ArcDropSlowFn = unsafe extern "C" fn(*mut *const u8);
type TextInputNewFn =
   unsafe extern "C" fn(*mut RawTextInput, *const RawEntity) -> *mut RawTextInput;
type TextInputIntoAnyFn =
   unsafe extern "C" fn(*mut RawAnyElement, *mut RawTextInput) -> *mut RawAnyElement;
type TextAreaNewFn = unsafe extern "C" fn(*mut RawTextArea, *const RawEntity) -> *mut RawTextArea;
type TextAreaIntoAnyFn =
   unsafe extern "C" fn(*mut RawAnyElement, *mut RawTextArea) -> *mut RawAnyElement;
type ListenerPushFn = unsafe extern "C" fn(*mut c_void, *mut u8, *const RcFnVtbl);
type IsHoveredFn = unsafe extern "C" fn(*const c_void, *const c_void) -> bool;

struct ModalSymbols {
   vec_push_any: VecPushAnyFn,
   modal_new: ModalNewFn,
   modal_into_any: ModalIntoAnyFn,
   modal_child_div: ModalChildDivFn,
   modal_footer_button: ModalFooterBtnFn,
   div_id: DivIdFn,
   layout: ModalLayout,
}

/// Click-outside-to-close, wired the way Discord's own overlay `backdrop` is.
struct DismissSymbols {
   push: ListenerPushFn,
   is_hovered: IsHoveredFn,
   /// `Div` offset of its `Box<Interactivity>`.
   interactivity: usize,
   mouse_down_listeners: usize,
   propagate_event: usize,
}

struct TextFieldSymbols {
   cx_new_single: CxNewFieldFn,
   cx_new_multiline: CxNewFieldFn,
   entity_map_get: EntityMapGetFn,
   entity_downgrade: EntityDowngradeFn,
   weak_drop: WeakDropFn,
   text_get: TextGetFn,
   set_value: SetValueFn,
   update_entity_dyn: UpdateEntityDynFn,
   arc_drop_slow: ArcDropSlowFn,
   text_input_new: TextInputNewFn,
   text_input_into_any: TextInputIntoAnyFn,
   text_area_new: TextAreaNewFn,
   text_area_into_any: TextAreaIntoAnyFn,
   layout: FieldLayout,
}

// SAFETY: resolved static function pointers in `Discord.exe`.
unsafe impl Send for ModalSymbols {}
unsafe impl Sync for ModalSymbols {}
unsafe impl Send for TextFieldSymbols {}
unsafe impl Sync for TextFieldSymbols {}
unsafe impl Send for DismissSymbols {}
unsafe impl Sync for DismissSymbols {}

static ORIG_OVERLAY_RENDER: OnceLock<OverlayRenderFn> = OnceLock::new();

/// The Settings layer's render, `fn(sret, layer, window, cx) -> sret`.
type SettingsLayerFn = unsafe extern "C" fn(
   *mut RawAnyElement,
   *mut c_void,
   *mut c_void,
   *mut c_void,
) -> *mut RawAnyElement;
static ORIG_SETTINGS_LAYER: OnceLock<SettingsLayerFn> = OnceLock::new();
/// Whether the open modal is drawn inside the Settings layer, from Settings'
/// first render while it is open until it closes. GPUI renders `Workspace` and
/// Settings independently (each re-renders only when notified, and replays its
/// last frame otherwise), so deciding this per frame made the modal flash
/// between the two.
static IN_SETTINGS: AtomicBool = AtomicBool::new(false);
/// When Settings last rendered, in ms since [`epoch_ms`]'s start.
static SETTINGS_AT: AtomicU64 = AtomicU64::new(0);
/// Settings this recently rendered may still be open: the press that opens a
/// modal from it re-renders it first.
const SETTINGS_RECENT_MS: u64 = 500;
/// Whether a redraw is due once [`SETTINGS_RECENT_MS`] has passed.
static RECHECK: AtomicBool = AtomicBool::new(false);

fn epoch_ms() -> u64 {
   static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
   u64::try_from(
      EPOCH
         .get_or_init(std::time::Instant::now)
         .elapsed()
         .as_millis(),
   )
   .unwrap_or(u64::MAX)
}
static MODAL_SYM: OnceLock<ModalSymbols> = OnceLock::new();
static FIELD_SYM: OnceLock<TextFieldSymbols> = OnceLock::new();
static DISMISS_SYM: OnceLock<DismissSymbols> = OnceLock::new();
/// Set while one mouse press is dispatched if it landed on the modal card.
static PRESS_IN_CARD: AtomicBool = AtomicBool::new(false);
static WINDOW_VAL_OFFSET: AtomicUsize = AtomicUsize::new(0);

static MODAL_OPEN: AtomicBool = AtomicBool::new(false);
static NEXT_PAGE_ID: AtomicU64 = AtomicU64::new(1);
static ACTIVE_MODAL: LazyLock<Mutex<Option<ActiveModal>>> = LazyLock::new(|| Mutex::new(None));

/// Main-thread-owned GPUI `Entity<FieldState>` handles, lazily created on first
/// render when `window` and `app` are live.
struct FieldEntities {
   url: Option<RawEntity>,
   code: Option<RawEntity>,
}

static FIELD_ENTITIES: LazyLock<Mutex<FieldEntities>> = LazyLock::new(|| {
   Mutex::new(FieldEntities {
      url: None,
      code: None,
   })
});

#[derive(Clone)]
enum ModalAction {
   Close,
   PageButton(Arc<dyn Fn() + Send + Sync>),
   SaveCode(String),
   CopyCode,
   SubmitInstallUrl,
}

static MODAL_ACTIONS: LazyLock<Mutex<Vec<ModalAction>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn register_modal_action(action: ModalAction) -> u64 {
   let mut list = MODAL_ACTIONS.lock().unwrap_or_else(PoisonError::into_inner);
   let id = list.len() as u64;
   list.push(action);
   id
}

fn lookup_modal_action(id: u64) -> Option<ModalAction> {
   let list = MODAL_ACTIONS.lock().unwrap_or_else(PoisonError::into_inner);
   usize::try_from(id)
      .ok()
      .and_then(|idx| list.get(idx).cloned())
}

// ---------------------------------------------------------------------------
// Vtables for Modal on_close (3 args), Modal Buttons (4 args), & update_entity
// ---------------------------------------------------------------------------

static MODAL_CLOSE_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size: size_of::<u64>(),
   align: align_of::<u64>(),
   call_once: modal_close_callback as *const c_void,
   call_mut: modal_close_callback as *const c_void,
   call: modal_close_callback as *const c_void,
   extra: ptr::null(),
};

static MODAL_BTN_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size: size_of::<u64>(),
   align: align_of::<u64>(),
   call_once: modal_button_callback as *const c_void,
   call_mut: modal_button_callback as *const c_void,
   call: modal_button_callback as *const c_void,
   extra: ptr::null(),
};

// `Box<dyn Fn(&MouseDownEvent, DispatchPhase, &Hitbox, &mut Window, &mut App)>`
// vtables for zero-sized listeners, so the box holds no allocation.
static SCRIM_PRESS_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: noop_drop as *const c_void,
   size: 0,
   align: 1,
   call_once: scrim_press as *const c_void,
   call_mut: scrim_press as *const c_void,
   call: scrim_press as *const c_void,
   extra: ptr::null(),
};

static CARD_PRESS_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: noop_drop as *const c_void,
   size: 0,
   align: 1,
   call_once: card_press as *const c_void,
   call_mut: card_press as *const c_void,
   call: card_press as *const c_void,
   extra: ptr::null(),
};

pub const PHASE_BUBBLE: u8 = 0;
pub const PHASE_CAPTURE: u8 = 1;

const unsafe extern "C" fn noop_drop(_data: *mut u8) {}

/// GPUI dispatches a press front to back in the capture phase, then back to
/// front in the bubble phase, so the card (painted over the scrim) bubbles
/// first. A press the card saw is kept; any other press on the scrim closes.
///
/// Both press handlers are Rust ABI like the `Fn::call` slot they fill: on
/// Linux `extern "C"` would assume the caller widened `phase`, which Rust
/// callers do not.
unsafe fn scrim_press(
   _data: *const u8,
   _event: *const c_void,
   phase: u8,
   hitbox: *const c_void,
   window: *const c_void,
   app: *mut u8,
) {
   let _ = std::panic::catch_unwind(|| {
      if phase == PHASE_CAPTURE {
         PRESS_IN_CARD.store(false, Ordering::Relaxed);
         return;
      }
      let Some(sym) = DISMISS_SYM.get() else {
         return;
      };
      // SAFETY: GPUI passes the live `&Hitbox`, `&Window` and `&mut App`.
      if phase != PHASE_BUBBLE || app.is_null() || !unsafe { (sym.is_hovered)(hitbox, window) } {
         return;
      }
      // Like Discord's backdrop, swallow the press so the chat underneath never
      // sees it (`cx.stop_propagation()`).
      // SAFETY: `propagate_event` is a `bool` in `App`, located by `sigs`.
      unsafe { app.add(sym.propagate_event).write(0) };
      if !PRESS_IN_CARD.swap(false, Ordering::Relaxed) {
         close_active();
      }
   });
}

unsafe fn card_press(
   _data: *const u8,
   _event: *const c_void,
   phase: u8,
   hitbox: *const c_void,
   window: *const c_void,
   _app: *mut u8,
) {
   let _ = std::panic::catch_unwind(|| {
      if phase == PHASE_BUBBLE
         && let Some(sym) = DISMISS_SYM.get()
         // SAFETY: GPUI passes the live `&Hitbox` and `&Window`.
         && unsafe { (sym.is_hovered)(hitbox, window) }
      {
         PRESS_IN_CARD.store(true, Ordering::Relaxed);
      }
   });
}

/// Pushes a zero-sized listener into `div`'s `Interactivity` mouse-down list,
/// which also makes GPUI give `div` a hitbox.
fn on_mouse_down(div: &mut RawDiv, vtbl: &'static RcFnVtbl) -> bool {
   listen_mouse_down(div, ptr::dangling_mut::<u8>(), vtbl)
}

/// Whether the pointer is over `hitbox`, for a mouse-down listener.
pub fn hovered(hitbox: *const c_void, window: *const c_void) -> bool {
   // SAFETY: GPUI passes listeners the live `&Hitbox` and `&Window`.
   DISMISS_SYM
      .get()
      .is_some_and(|sym| unsafe { (sym.is_hovered)(hitbox, window) })
}

/// [`on_mouse_down`] with `data` as the listener's `self`. A zero-sized
/// `vtbl` keeps GPUI from reading or freeing `data`, so it may be any
/// non-null value.
pub fn listen_mouse_down(div: &mut RawDiv, data: *mut u8, vtbl: &'static RcFnVtbl) -> bool {
   let Some(sym) = DISMISS_SYM.get() else {
      return false;
   };
   // SAFETY: `RawDiv` holds its `Box<Interactivity>` at `interactivity`
   // (decoded by `sigs`); `push` is Discord's own `Vec<Box<dyn ..>>::push`.
   unsafe {
      let interactivity = div
         .as_mut_ptr()
         .add(sym.interactivity)
         .cast::<*mut u8>()
         .read_unaligned();
      if interactivity.is_null() {
         return false;
      }
      (sym.push)(
         interactivity.add(sym.mouse_down_listeners).cast::<c_void>(),
         data,
         vtbl,
      );
   }
   true
}

#[repr(C)]
struct SetValueEnv {
   entity: *const RawEntity,
   text_ptr: *const u8,
   text_len: usize,
   tf: *const TextFieldSymbols,
}

static SET_VALUE_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size: size_of::<SetValueEnv>(),
   align: align_of::<SetValueEnv>(),
   call_once: set_value_trampoline as *const c_void,
   call_mut: set_value_trampoline as *const c_void,
   call: set_value_trampoline as *const c_void,
   extra: ptr::null(),
};

unsafe extern "C" fn set_value_trampoline(
   env: *mut SetValueEnv,
   field: *mut c_void,
   _any_vtbl: *const c_void,
   app: *mut c_void,
) {
   if env.is_null() || field.is_null() || app.is_null() {
      return;
   }
   let _ = std::panic::catch_unwind(|| {
      // SAFETY: called synchronously from `App::update_entity_dyn` on the GPUI
      // main thread with valid `env`, leased `field`, and `app`.
      unsafe {
         let env_ref = &*env;
         let tf = &*env_ref.tf;
         let mut ctx = MaybeUninit::<RawContext>::uninit();
         let ctx_ptr = ctx.as_mut_ptr();
         (*ctx_ptr).app = app;
         (tf.entity_downgrade)(&raw mut (*ctx_ptr).weak, env_ref.entity);
         let mut ctx = ctx.assume_init();
         // Discord passes a non-null dangling pointer (`1`) for empty slices.
         let ptr = if env_ref.text_len == 0 {
            ptr::dangling()
         } else {
            env_ref.text_ptr
         };
         (tf.set_value)(
            field,
            ptr,
            env_ref.text_len,
            ptr::null_mut(),
            (&raw mut ctx).cast::<c_void>(),
         );
         // `weak_drop` takes the `Weak` inside the `WeakEntity`, as Discord's
         // own `cx.new` drops its context.
         let weak_slot = (&raw mut ctx.weak)
            .cast::<u8>()
            .add(tf.layout.weak_ptr)
            .cast::<c_void>();
         (tf.weak_drop)(weak_slot);
      }
   });
}

/// `Modal::on_close` invokes `Fn::call(&self, window, cx)` — 3 arguments, no
/// click event argument.
unsafe extern "C" fn modal_close_callback(
   _self_ptr: *const u64,
   _window: *mut c_void,
   _cx: *mut c_void,
) {
   let _ = std::panic::catch_unwind(close_active);
}

/// Mana `Button::on_click` invokes `Fn::call(&self, event, window, app)` — 4
/// arguments, giving us a live `*mut App` for reading native `FieldState`s.
unsafe extern "C" fn modal_button_callback(
   self_ptr: *const u64,
   _event: *const c_void,
   _window: *mut c_void,
   app: *mut c_void,
) {
   if self_ptr.is_null() {
      return;
   }
   let _ = std::panic::catch_unwind(|| {
      // SAFETY: `self_ptr` points to `RcInner.action_id`.
      let id = unsafe { *self_ptr };
      let Some(action) = lookup_modal_action(id) else {
         return;
      };
      match action {
         ModalAction::Close => close_active(),
         ModalAction::PageButton(cb) => {
            std::thread::spawn(move || cb());
         },
         ModalAction::CopyCode => {
            if let Some(code) = read_code_field(app) {
               match phoenix_ui_desktop::copy(&code) {
                  Ok(()) => phoenix_ui_desktop::toast("Copied plugin source"),
                  Err(err) => phoenix_ui_desktop::toast(&format!("Copy failed: {err}")),
               }
            }
         },
         ModalAction::SaveCode(plugin) => {
            let Some(code) = read_code_field(app) else {
               set_editor_status("Could not read text field", true);
               return;
            };
            match phoenix_runtime::save_source(&plugin, code) {
               Ok(()) => {
                  close_active();
                  phoenix_ui_desktop::toast(&format!("Saved & reloaded {plugin}"));
               },
               Err(err) => {
                  set_editor_status(&format!("Save failed: {err}"), true);
               },
            }
         },
         ModalAction::SubmitInstallUrl => {
            let target = match ACTIVE_MODAL
               .lock()
               .unwrap_or_else(PoisonError::into_inner)
               .as_ref()
            {
               Some(ActiveModal::InstallUrl(inst)) => inst.target.clone(),
               _ => return,
            };
            match target {
               FieldTarget::Install => {},
               FieldTarget::Setting(field) => {
                  save_setting(app, &field);
                  return;
               },
               FieldTarget::Prompt(prompt) => {
                  let Some(raw) = read_url_field(app) else {
                     set_install_status("Could not read the text field", true, false);
                     return;
                  };
                  let text = raw.trim().to_owned();
                  if text.is_empty() {
                     return;
                  }
                  close_active();
                  std::thread::spawn(move || (prompt.on_submit)(text));
                  return;
               },
            }
            let Some(raw) = read_url_field(app) else {
               set_install_status("Could not read URL field", true, false);
               return;
            };
            let url = raw.trim().to_owned();
            if url.is_empty() {
               set_install_status("Enter an https:// link to a plugin .js file", true, false);
               return;
            }
            set_install_status("Downloading and installing plugin...", false, true);
            std::thread::spawn(move || match phoenix_runtime::install(&url) {
               Ok(installed) => {
                  close_active();
                  phoenix_ui_desktop::toast(&installed.to_string());
               },
               Err(err) => {
                  set_install_status(&format!("{err}"), true, false);
               },
            });
         },
      }
   });
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// macOS draws pages and dialogs as `AppKit` windows
/// ([`phoenix_ui_desktop::show_panel`]), since Discord's GPUI modal has no
/// arm64 locators.
#[cfg(target_os = "macos")]
mod panel {
   use std::sync::Arc;

   use phoenix_ui_desktop::{PanelButton, PanelSection, close_panel, show_panel};

   use super::{Choice, ChoiceOption, ModalSection, PageModalSpec};

   fn section(section: ModalSection) -> PanelSection {
      match section {
         ModalSection::Buttons { title, buttons } => PanelSection::Buttons {
            title,
            buttons: buttons
               .into_iter()
               .map(|button| PanelButton {
                  label: button.label,
                  danger: button.danger,
                  on_press: button.on_press,
               })
               .collect(),
         },
         ModalSection::Text { title, text } => PanelSection::Text { title, text },
         ModalSection::Code {
            title,
            text,
            monospace,
            ..
         } => PanelSection::Code {
            title,
            text,
            monospace,
         },
      }
   }

   pub fn open(spec: PageModalSpec, on_close: Option<phoenix_ui_desktop::PanelAction>) -> u64 {
      show_panel(
         spec.title,
         spec.sections.into_iter().map(section).collect(),
         on_close,
      )
   }

   /// A dialog whose buttons answer `choice` with their index and close it;
   /// closing it any other way answers -1.
   pub fn open_choice(
      title: String,
      body: Option<ModalSection>,
      options: Vec<ChoiceOption>,
      choice: &Arc<Choice>,
   ) {
      let id = Arc::new(std::sync::OnceLock::<u64>::new());
      let mut sections: Vec<PanelSection> = body.into_iter().map(section).collect();
      if !options.is_empty() {
         let buttons = options
            .into_iter()
            .enumerate()
            .map(|(index, option)| {
               let (choice, id) = (Arc::clone(choice), Arc::clone(&id));
               PanelButton {
                  label: option.label,
                  danger: option.danger,
                  on_press: Arc::new(move || {
                     choice.pick(i32::try_from(index).unwrap_or(-1));
                     if let Some(&id) = id.get() {
                        close_panel(id);
                     }
                  }),
               }
            })
            .collect();
         sections.push(PanelSection::Buttons {
            title: None,
            buttons,
         });
      }
      let dismissed = Arc::clone(choice);
      let _ = id.set(show_panel(
         title,
         sections,
         Some(Arc::new(move || dismissed.pick(-1))),
      ));
   }
}

/// Opens a native GPUI modal for `phoenix.ui.openPage` and triggers a GPUI
/// redraw immediately.
pub fn open_page(spec: PageModalSpec) -> u64 {
   if cfg!(target_os = "macos") {
      return open_panel(spec);
   }
   resolve_pending_choice();
   let id = NEXT_PAGE_ID.fetch_add(1, Ordering::Relaxed);
   set_active(Some(ActiveModal::Page { id, spec }));
   id
}

#[cfg(target_os = "macos")]
fn open_panel(spec: PageModalSpec) -> u64 {
   panel::open(spec, None)
}

#[cfg(not(target_os = "macos"))]
fn open_panel(_spec: PageModalSpec) -> u64 {
   0
}

/// Replaces the active modal. `MODAL_OPEN` changes under the same lock, so a
/// close racing an open cannot leave a held modal undrawn.
fn set_active(modal: Option<ActiveModal>) {
   let mut guard = ACTIVE_MODAL.lock().unwrap_or_else(PoisonError::into_inner);
   MODAL_OPEN.store(modal.is_some(), Ordering::Release);
   *guard = modal;
   drop(guard);
   force_redraw();
}

/// Opens a native choose or sheet dialog and returns what it will answer.
/// A `showSheet` body is a [`ModalSection::Code`]; a `choose` message is
/// [`ModalSection::Text`]. Without resolved GPUI modal symbols the dialog
/// cannot be drawn, so it answers -1 immediately.
pub fn open_choice(
   title: String,
   body: Option<ModalSection>,
   options: Vec<ChoiceOption>,
) -> Arc<Choice> {
   let choice = Arc::new(Choice::new());
   #[cfg(target_os = "macos")]
   if cfg!(target_os = "macos") {
      panel::open_choice(title, body, options, &choice);
      return choice;
   }
   if ORIG_OVERLAY_RENDER.get().is_none() {
      choice.pick(-1);
      return choice;
   }
   resolve_pending_choice();
   let under = match ACTIVE_MODAL
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_ref()
   {
      Some(ActiveModal::Page { id, spec }) => Some((*id, spec.clone())),
      Some(ActiveModal::Choice { under, .. }) => under.clone(),
      _ => None,
   };

   let mut sections = Vec::with_capacity(2);
   if let Some(body) = body {
      sections.push(body);
   }
   if !options.is_empty() {
      let buttons = options
         .into_iter()
         .enumerate()
         .map(|(index, option)| {
            let picked = Arc::clone(&choice);
            ModalButton {
               label: option.label,
               danger: option.danger,
               on_press: Arc::new(move || {
                  finish_choice(&picked, i32::try_from(index).unwrap_or(-1));
               }),
            }
         })
         .collect();
      sections.push(ModalSection::Buttons {
         title: None,
         buttons,
      });
   }
   set_active(Some(ActiveModal::Choice {
      spec: PageModalSpec { title, sections },
      choice: Arc::clone(&choice),
      under,
   }));
   choice
}

/// Opens a choose dialog and waits up to `timeout` for the picked index, -1
/// when it is dismissed or left open.
pub fn ask(
   title: String,
   body: Option<ModalSection>,
   options: Vec<ChoiceOption>,
   timeout: Duration,
) -> i32 {
   let choice = open_choice(title, body, options);
   choice.wait(timeout).unwrap_or_else(|| {
      dismiss_choice(&choice);
      -1
   })
}

/// Closes `choice`'s dialog if it is still the one on screen.
pub fn dismiss_choice(choice: &Arc<Choice>) {
   finish_choice(choice, -1);
}

/// Answers `choice` with `index` and closes its dialog, leaving any dialog
/// that already replaced it open.
fn finish_choice(choice: &Arc<Choice>, index: i32) {
   let mut guard = ACTIVE_MODAL.lock().unwrap_or_else(PoisonError::into_inner);
   let current = matches!(guard.as_ref(), Some(ActiveModal::Choice { choice: active, .. }) if Arc::ptr_eq(active, choice));
   choice.pick(index);
   if current {
      *guard = uncover(guard.take());
      MODAL_OPEN.store(guard.is_some(), Ordering::Release);
   }
   drop(guard);
   if current {
      force_redraw();
   }
}

/// Answers any open choose dialog with -1 before it is replaced.
fn resolve_pending_choice() {
   let guard = ACTIVE_MODAL.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some(ActiveModal::Choice { choice, .. }) = guard.as_ref() {
      choice.pick(-1);
   }
}

/// The page a closing modal leaves on screen.
fn uncover(closing: Option<ActiveModal>) -> Option<ActiveModal> {
   match closing {
      Some(ActiveModal::Choice {
         under: Some((id, spec)),
         ..
      }) => Some(ActiveModal::Page { id, spec }),
      _ => None,
   }
}

/// Swaps the sections of page `id`, on screen or under a choose dialog.
/// False when that page is gone.
pub fn replace_page(id: u64, spec: PageModalSpec) -> bool {
   let mut guard = ACTIVE_MODAL.lock().unwrap_or_else(PoisonError::into_inner);
   let slot = match guard.as_mut() {
      Some(
         ActiveModal::Page { id: cur, spec }
         | ActiveModal::Choice {
            under: Some((cur, spec)),
            ..
         },
      ) if *cur == id => spec,
      _ => return false,
   };
   *slot = spec;
   drop(guard);
   force_redraw();
   true
}

/// Closes an open page modal if its id still matches `id`.
pub fn close_page(id: u64) {
   #[cfg(target_os = "macos")]
   phoenix_ui_desktop::close_panel(id);
   let mut guard = ACTIVE_MODAL.lock().unwrap_or_else(PoisonError::into_inner);
   if matches!(&*guard, Some(ActiveModal::Page { id: cur, .. }) if *cur == id) {
      *guard = None;
      MODAL_OPEN.store(false, Ordering::Release);
      drop(guard);
      force_redraw();
   }
}

/// Opens a native GPUI multiline code editor modal for `plugin`.
pub fn open_code_editor(plugin: String, initial_source: String) {
   resolve_pending_choice();
   set_active(Some(ActiveModal::CodeEditor(CodeEditorModal {
      plugin,
      pending_init: Some(initial_source),
      status: None,
   })));
}

/// Opens a native GPUI single-line URL installer modal.
pub fn open_install_url() {
   resolve_pending_choice();
   set_active(Some(ActiveModal::InstallUrl(InstallUrlModal {
      target: FieldTarget::Install,
      pending_init: Some(String::new()),
      status: None,
      busy: false,
   })));
}

/// Opens the one-line field on a plugin's text, number or secret setting.
pub fn open_setting_editor(field: SettingField) {
   resolve_pending_choice();
   set_active(Some(ActiveModal::InstallUrl(InstallUrlModal {
      pending_init: Some(field.initial.clone()),
      target: FieldTarget::Setting(field),
      status: None,
      busy: false,
   })));
}

/// Opens the one-line field for `prompt`, which gets the text when it is
/// submitted.
pub fn open_prompt(prompt: Prompt) {
   resolve_pending_choice();
   set_active(Some(ActiveModal::InstallUrl(InstallUrlModal {
      target: FieldTarget::Prompt(prompt),
      pending_init: Some(String::new()),
      status: None,
      busy: false,
   })));
}

/// Stores the field's text as `field`'s value and closes the modal, or says
/// why it can't.
fn save_setting(app: *mut c_void, field: &SettingField) {
   let raw = if field.kind == FieldKind::Lines {
      read_code_field(app)
   } else {
      read_url_field(app)
   };
   let Some(raw) = raw else {
      set_install_status("Could not read the text field", true, false);
      return;
   };
   let text = raw.trim();
   let value = if field.kind == FieldKind::Number {
      match text.parse::<f64>() {
         Ok(number) if number.is_finite() => phoenix_runtime::Chosen::Number(number),
         _ => {
            set_install_status("Enter a number", true, false);
            return;
         },
      }
   } else {
      phoenix_runtime::Chosen::Text(text.to_owned())
   };
   phoenix_runtime::set_setting(&field.plugin, &field.key, value);
   close_active();
}

fn close_active() {
   resolve_pending_choice();
   let mut guard = ACTIVE_MODAL.lock().unwrap_or_else(PoisonError::into_inner);
   *guard = uncover(guard.take());
   MODAL_OPEN.store(guard.is_some(), Ordering::Release);
   drop(guard);
   force_redraw();
}

fn set_editor_status(msg: &str, is_error: bool) {
   let mut guard = ACTIVE_MODAL.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some(ActiveModal::CodeEditor(ed)) = guard.as_mut() {
      ed.status = Some((msg.to_owned(), is_error));
   }
   drop(guard);
   force_redraw();
}

fn set_install_status(msg: &str, is_error: bool, busy: bool) {
   let mut guard = ACTIVE_MODAL.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some(ActiveModal::InstallUrl(inst)) = guard.as_mut() {
      inst.status = Some((msg.to_owned(), is_error));
      inst.busy = busy;
   }
   drop(guard);
   force_redraw();
}

/// Forces every visible GPUI window in this process to re-render, from any
/// thread.
pub fn force_redraw() {
   Os::force_redraw(WINDOW_VAL_OFFSET.load(Ordering::Acquire));
}

// ---------------------------------------------------------------------------
// Installation (`DllMain` / `crate::prepare`)
// ---------------------------------------------------------------------------

/// Locates Discord's native `Modal`, `Div::id`, `TextInput`/`TextArea`, and
/// window root render, then hooks `WindowRoot::render`.
#[expect(clippy::too_many_lines, reason = "one symbol table per located group")]
pub fn install(image: &Image<'static>) {
   let (Some(div_into_any), Some(gpui_layout)) = (gpui::div_into_any_addr(), gpui::layout()) else {
      log::warn!("desktop modal: GPUI builders unavailable, skipping");
      return;
   };
   if !gpui::supports_push_any() {
      log::warn!("desktop modal: Div::child(AnyElement) unavailable, skipping");
      return;
   }
   let found = match desktop::modal(image, div_into_any, &gpui_layout.div) {
      Ok(m) => m,
      Err(err) => {
         log::warn!("desktop modal: {err}, native modals disabled");
         return;
      },
   };
   WINDOW_VAL_OFFSET.store(found.window_validation_offset, Ordering::Release);
   // SAFETY: resolved and shape-checked against `Discord.exe` by `sigs`.
   let _ = DISMISS_SYM.set(unsafe {
      DismissSymbols {
         push: std::mem::transmute::<usize, ListenerPushFn>(found.dismiss.listener_push),
         is_hovered: std::mem::transmute::<usize, IsHoveredFn>(found.dismiss.hitbox_is_hovered),
         interactivity: gpui_layout.div.interactivity,
         mouse_down_listeners: found.dismiss.mouse_down_listeners,
         propagate_event: found.dismiss.propagate_event,
      }
   });

   // SAFETY: resolved and layout-verified against `Discord.exe`.
   let modal_sym = unsafe {
      ModalSymbols {
         vec_push_any: std::mem::transmute::<usize, VecPushAnyFn>(found.vec_push_any),
         modal_new: std::mem::transmute::<usize, ModalNewFn>(found.modal_new),
         modal_into_any: std::mem::transmute::<usize, ModalIntoAnyFn>(found.modal_into_any),
         modal_child_div: std::mem::transmute::<usize, ModalChildDivFn>(found.modal_child_div),
         modal_footer_button: std::mem::transmute::<usize, ModalFooterBtnFn>(
            found.modal_footer_button,
         ),
         div_id: std::mem::transmute::<usize, DivIdFn>(found.div_id),
         layout: found.layout,
      }
   };
   let _ = MODAL_SYM.set(modal_sym);

   match desktop::text_fields(image) {
      Ok(tf) => {
         // SAFETY: resolved and cross-checked against `Discord.exe`.
         let sym = unsafe {
            TextFieldSymbols {
               cx_new_single: std::mem::transmute::<usize, CxNewFieldFn>(tf.cx_new_single),
               cx_new_multiline: std::mem::transmute::<usize, CxNewFieldFn>(tf.cx_new_multiline),
               entity_map_get: std::mem::transmute::<usize, EntityMapGetFn>(tf.entity_map_get),
               entity_downgrade: std::mem::transmute::<usize, EntityDowngradeFn>(
                  tf.entity_downgrade,
               ),
               weak_drop: std::mem::transmute::<usize, WeakDropFn>(tf.weak_drop),
               text_get: std::mem::transmute::<usize, TextGetFn>(tf.text_get),
               set_value: std::mem::transmute::<usize, SetValueFn>(tf.set_value),
               update_entity_dyn: std::mem::transmute::<usize, UpdateEntityDynFn>(
                  tf.update_entity_dyn,
               ),
               arc_drop_slow: std::mem::transmute::<usize, ArcDropSlowFn>(tf.arc_drop_slow),
               text_input_new: std::mem::transmute::<usize, TextInputNewFn>(tf.text_input_new),
               text_input_into_any: std::mem::transmute::<usize, TextInputIntoAnyFn>(
                  tf.text_input_into_any,
               ),
               text_area_new: std::mem::transmute::<usize, TextAreaNewFn>(tf.text_area_new),
               text_area_into_any: std::mem::transmute::<usize, TextAreaIntoAnyFn>(
                  tf.text_area_into_any,
               ),
               layout: tf.layout,
            }
         };
         let _ = FIELD_SYM.set(sym);
         log::info!(
            "desktop modal: resolved native TextInput/TextArea (single={:#x}, multi={:#x}) {:x?}",
            tf.cx_new_single,
            tf.cx_new_multiline,
            tf.layout,
         );
      },
      Err(err) => {
         log::warn!("desktop modal: native TextInput/TextArea unavailable: {err}");
      },
   }

   let pro = desktop::Prologues::of(phoenix_hook::x86::Abi::of(image)).frame;
   // SAFETY: runs from `prepare` before Discord threads execute;
   // `overlay_render_detour` matches `Workspace::render_overlays`'s 5-arg sret
   // ABI returning `Vec<AnyElement>`.
   let trampoline = match unsafe {
      image.hook_entry(
         found.overlay_render,
         pro.pattern,
         pro.len,
         overlay_render_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("desktop modal: overlay render hook failed: {err}");
         return;
      },
   };
   // SAFETY: `trampoline` replays the 19-byte prologue and jumps to `+19`.
   let orig = unsafe { std::mem::transmute::<usize, OverlayRenderFn>(trampoline) };
   let _ = ORIG_OVERLAY_RENDER.set(orig);
   install_settings_layer(image, pro);
   log::info!(
      "desktop modal: hooked Workspace::render_overlays @ {:#x} (push={:#x}, val +{:#x}) {:x?}",
      found.overlay_render,
      found.vec_push_any,
      found.window_validation_offset,
      found.layout,
   );
}

/// Hooks the Settings layer's render, so modals opened over Settings draw
/// above it.
fn install_settings_layer(image: &Image<'static>, pro: desktop::Prologue) {
   let func = match desktop::settings_layer(image) {
      Ok(func) => func,
      Err(err) => {
         log::warn!("desktop modal: {err}, modals stay under Settings");
         return;
      },
   };
   // SAFETY: from `prepare`, before Discord's threads run; the detour has the
   // Settings layer render's sret signature.
   match unsafe {
      image.hook_entry(
         func,
         pro.pattern,
         pro.len,
         settings_layer_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => {
         // SAFETY: `trampoline` replays the prologue of `func`.
         let _ = ORIG_SETTINGS_LAYER
            .set(unsafe { std::mem::transmute::<usize, SettingsLayerFn>(trampoline) });
         log::info!("desktop modal: hooked Settings layer @ {func:#x}");
      },
      Err(err) => log::warn!("desktop modal: Settings layer hook failed: {err}"),
   }
}

// ---------------------------------------------------------------------------
// Native FieldState Helpers (GPUI Main Thread Only)
// ---------------------------------------------------------------------------

/// Lazily initializes a single-line or multi-line `Entity<FieldState>` on the
/// GPUI main thread and writes its static placeholder.
unsafe fn ensure_field_entity(
   app: *mut c_void,
   window: *mut c_void,
   multiline: bool,
   placeholder: &'static str,
) -> Option<RawEntity> {
   let tf = FIELD_SYM.get()?;
   if app.is_null() || window.is_null() {
      return None;
   }
   let mut guard = FIELD_ENTITIES
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
   let slot = if multiline {
      &mut guard.code
   } else {
      &mut guard.url
   };
   if let Some(ent) = *slot {
      return Some(ent);
   }
   // SAFETY: invoked on the GPUI main thread inside `WindowRoot::render` with
   // valid `app` and `window` pointers.
   let ent = unsafe {
      let mut out = MaybeUninit::<RawEntity>::uninit();
      if multiline {
         (tf.cx_new_multiline)(out.as_mut_ptr(), app, window);
      } else {
         (tf.cx_new_single)(out.as_mut_ptr(), app, window);
      }
      let ent = out.assume_init();
      let field = (tf.entity_map_get)(
         app.cast::<u8>()
            .add(tf.layout.app_entities)
            .cast::<c_void>(),
         ent.id_lo,
         ent.id_hi,
      );
      if !field.is_null() {
         for slot in tf.layout.placeholders {
            field
               .cast::<u8>()
               .add(slot)
               .cast::<SharedString>()
               .write_unaligned(SharedString::from_static(placeholder));
         }
      }
      ent
   };
   *slot = Some(ent);
   Some(ent)
}

unsafe fn set_field_text(app: *mut c_void, entity: &RawEntity, text: &str) {
   let Some(tf) = FIELD_SYM.get() else {
      return;
   };
   if app.is_null() {
      return;
   }
   let mut env = SetValueEnv {
      entity: ptr::from_ref(entity),
      text_ptr: text.as_ptr(),
      text_len: text.len(),
      tf: ptr::from_ref(tf),
   };
   // SAFETY: `update_entity_dyn` leases `FieldState`, calls
   // `set_value_trampoline` synchronously on this stack frame, ends the lease,
   // and flushes effects.
   unsafe {
      (tf.update_entity_dyn)(
         app,
         ptr::from_ref(entity),
         b"FieldState".as_ptr(),
         10,
         (&raw mut env).cast::<c_void>(),
         &raw const SET_VALUE_VTBL,
      );
   }
}

unsafe fn read_field_text(app: *mut c_void, entity: &RawEntity) -> Option<String> {
   let tf = FIELD_SYM.get()?;
   if app.is_null() {
      return None;
   }
   // SAFETY: invoked synchronously from a GPUI button callback with live `app`.
   unsafe {
      let field = (tf.entity_map_get)(
         app.cast::<u8>()
            .add(tf.layout.app_entities)
            .cast::<c_void>(),
         entity.id_lo,
         entity.id_hi,
      );
      if field.is_null() {
         return None;
      }
      let mut ss = MaybeUninit::<SharedString>::uninit();
      (tf.text_get)(ss.as_mut_ptr(), field, app);
      let mut ss = ss.assume_init();
      let result = decode_shared_string(&ss);
      if ss.tag >= 0x19 && !ss.ptr.is_null() {
         #[expect(clippy::cast_ptr_alignment, reason = "an `Arc` header is word aligned")]
         let strong = &*ss.ptr.cast::<AtomicUsize>();
         if strong.fetch_sub(1, Ordering::Release) == 1 {
            std::sync::atomic::fence(Ordering::Acquire);
            (tf.arc_drop_slow)(&raw mut ss.ptr);
         }
      }
      Some(result)
   }
}

unsafe fn decode_shared_string(ss: &SharedString) -> String {
   let tag = ss.tag;
   if tag <= 0x17 {
      // SAFETY: inline bytes live at offsets `1..=tag` inside `SharedString`.
      let bytes =
         unsafe { slice::from_raw_parts(ptr::from_ref(ss).cast::<u8>().add(1), usize::from(tag)) };
      return String::from_utf8_lossy(bytes).into_owned();
   }
   if tag == 0x18 {
      if ss.ptr.is_null() || ss.len == 0 {
         return String::new();
      }
      // SAFETY: static slice `ptr..ptr + len`.
      let bytes = unsafe { slice::from_raw_parts(ss.ptr, ss.len) };
      return String::from_utf8_lossy(bytes).into_owned();
   }
   if tag == 0x19 {
      if ss.ptr.is_null() || ss.len == 0 {
         return String::new();
      }
      // SAFETY: `ArcInner` has `strong: usize` (+0), `weak: usize` (+8), UTF-8
      // payload at `+0x10` for `ss.len` bytes.
      let bytes = unsafe { slice::from_raw_parts(ss.ptr.add(0x10), ss.len) };
      return String::from_utf8_lossy(bytes).into_owned();
   }
   String::new()
}

fn read_code_field(app: *mut c_void) -> Option<String> {
   let ent = FIELD_ENTITIES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .code?;
   // SAFETY: called from GPUI button callback with live `app`.
   unsafe { read_field_text(app, &ent) }
}

fn read_url_field(app: *mut c_void) -> Option<String> {
   let ent = FIELD_ENTITIES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .url?;
   // SAFETY: called from GPUI button callback with live `app`.
   unsafe { read_field_text(app, &ent) }
}

// ---------------------------------------------------------------------------
// Workspace::render_overlays Detour & Native Modal Builder
// ---------------------------------------------------------------------------

unsafe extern "C" fn overlay_render_detour(
   sret: *mut RawVecAny,
   workspace: *mut c_void,
   arg3: *mut c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawVecAny {
   let Some(orig) = ORIG_OVERLAY_RENDER.get().copied() else {
      return sret;
   };
   // SAFETY: always render Discord's own overlay stack first.
   let out = unsafe { orig(sret, workspace, arg3, window, cx) };
   if !MODAL_OPEN.load(Ordering::Acquire) {
      IN_SETTINGS.store(false, Ordering::Release);
      return out;
   }
   // ponytail: closing Settings under an open modal hides the modal until
   // Settings opens again or the modal is replaced; nothing tells this render
   // apart from one where Settings only replays its last frame.
   if IN_SETTINGS.load(Ordering::Acquire) || sret.is_null() || cx.is_null() {
      return out;
   }
   // Settings rendering after this in the same frame would build the modal a
   // second time, so leave it to Settings while it was just on screen, and look
   // again once that has passed in case it closed.
   // ponytail: Settings left idle past this before a modal opens from a timer
   // still gets one frame with the modal built in both places.
   let at = SETTINGS_AT.load(Ordering::Acquire);
   if at != 0 && epoch_ms().saturating_sub(at) < SETTINGS_RECENT_MS {
      if !RECHECK.swap(true, Ordering::AcqRel) {
         std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(SETTINGS_RECENT_MS + 100));
            RECHECK.store(false, Ordering::Release);
            force_redraw();
         });
      }
      return out;
   }

   let _ = std::panic::catch_unwind(|| {
      // SAFETY: `[cx]` is `*mut App` inside `Context<Workspace>`.
      let app = unsafe { *(cx.cast::<*mut c_void>()) };
      let Some(msym) = MODAL_SYM.get() else {
         return;
      };
      let Some(scrim) = build_overlay(window, app) else {
         return;
      };
      let mut scrim_any = MaybeUninit::<RawAnyElement>::uninit();
      gpui::write_any(scrim, scrim_any.as_mut_ptr());
      // SAFETY: `scrim_any` was initialized by `div_into_any` and
      // `vec_push_any` is Discord's own `Vec::<AnyElement>::push`.
      unsafe {
         let mut scrim_any = scrim_any.assume_init();
         (msym.vec_push_any)(sret, &raw mut scrim_any);
      }
   });
   out
}

unsafe extern "C" fn settings_layer_detour(
   sret: *mut RawAnyElement,
   layer: *mut c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawAnyElement {
   let Some(orig) = ORIG_SETTINGS_LAYER.get().copied() else {
      return sret;
   };
   // SAFETY: Discord's own render first, with its own arguments.
   let out = unsafe { orig(sret, layer, window, cx) };
   SETTINGS_AT.store(epoch_ms().max(1), Ordering::Release);
   if !MODAL_OPEN.load(Ordering::Acquire) {
      IN_SETTINGS.store(false, Ordering::Release);
      return out;
   }
   if sret.is_null() || cx.is_null() {
      return out;
   }
   IN_SETTINGS.store(true, Ordering::Release);
   let _ = std::panic::catch_unwind(|| {
      // SAFETY: `[cx]` is `*mut App` inside the layer's `Context`.
      let app = unsafe { *(cx.cast::<*mut c_void>()) };
      let Some(scrim) = build_overlay(window, app) else {
         return;
      };
      let Some(mut over) = gpui::raw_div() else {
         return;
      };
      gpui::style_div(&mut over, |s| {
         s.set(s.at.position, POSITION_ABSOLUTE);
         s.lengths(&s.at.inset, gpui_px(0.0));
         s.lengths(&s.at.size, W_FULL);
      });
      // SAFETY: takes the rendered Settings `AnyElement` out of `sret`; a
      // replacement is written back below either way.
      let mut settings = unsafe { ptr::read(sret) };
      if !gpui::push_any(&mut over, &mut settings) {
         // SAFETY: puts the untouched Settings element back.
         unsafe { ptr::write(sret, settings) };
         return;
      }
      gpui::write_any(gpui::push_div(over, scrim), sret);
   });
   out
}

const WHITE_SPACE_NOWRAP: u8 = 1;
const FLEX_NOWRAP: u8 = 0;
const FLEX_WRAP: u8 = 1;
const OVERFLOW_SCROLL: u8 = 3;
const POSITION_ABSOLUTE: u8 = 1;
const DISPLAY_FLEX: u8 = 1;
const ALIGN_CENTER: u8 = 4;

/// Lines the plugin code editor shows; it scrolls past them.
const EDITOR_ROWS: u8 = 30;
/// Rows of a multiline setting's field.
const SETTING_ROWS: u8 = 8;

fn set_mono_font(s: &Style, font_px: f32, line_px: f32) {
   s.length(s.at.font_size, gpui_px(font_px));
   s.length(s.at.line_height, gpui_px(line_px));
   s.shared_string(s.at.font_family, SharedString::from_static(MONO_FONT));
   s.set(s.at.white_space, WHITE_SPACE_NOWRAP);
}

fn div_with_id(msym: &ModalSymbols, mut div: RawDiv, id: &str) -> RawDiv {
   let id_s = intern_str(id);
   let mut out = RawDiv::zeroed();
   // SAFETY: `Div::id` moves `div` into `out` and enables native mouse-wheel
   // scrolling via `Interactivity.element_id`.
   unsafe { (msym.div_id)(&raw mut out, &raw mut div, id_s.as_ptr(), id_s.len()) };
   out
}

fn make_modal_button(
   id: &str,
   label: &str,
   variant: BtnVariant,
   size: BtnSize,
   action: ModalAction,
) -> Option<RawButton> {
   let action_id = register_modal_action(action);
   gpui::build_raw_button(
      id,
      label,
      variant,
      size,
      &raw const MODAL_BTN_VTBL,
      action_id,
   )
}

fn push_modal_button(
   parent: RawDiv,
   id: &str,
   label: &str,
   variant: BtnVariant,
   size: BtnSize,
   action: ModalAction,
) -> RawDiv {
   let Some(btn) = make_modal_button(id, label, variant, size, action) else {
      return parent;
   };
   gpui::push_raw_button(parent, btn)
}

fn mono_span(text: &str, argb: u32) -> Option<RawDiv> {
   let mut d = gpui::raw_div()?;
   let hsla = argb_to_hsla(argb);
   gpui::style_div(&mut d, |s| {
      set_mono_font(s, 12.5, 18.0);
      s.hsla(s.at.text_color, hsla);
      // Prevent token spans from shrinking inside flex-row lines.
      s.float(s.at.flex_shrink, 0.0);
   });
   let mut any = gpui::str_to_any(text)?;
   if !gpui::push_any(&mut d, &mut any) {
      return None;
   }
   Some(d)
}

fn plain_text_div(text: &str, argb: u32, font_px: f32) -> Option<RawDiv> {
   let mut d = gpui::raw_div()?;
   let hsla = argb_to_hsla(argb);
   gpui::style_div(&mut d, |s| {
      s.length(s.at.font_size, gpui_px(font_px));
      s.length(s.at.line_height, gpui_px(font_px + 5.0));
      s.hsla(s.at.text_color, hsla);
   });
   let mut any = gpui::str_to_any(text)?;
   if !gpui::push_any(&mut d, &mut any) {
      return None;
   }
   Some(d)
}

/// Joins same-colored neighbors and folds whitespace into a neighbor. Every
/// span is a full GPUI `Div` rebuilt each frame, so a JSON line drops from
/// about six to three or four.
fn coalesce<'a>(tokens: impl Iterator<Item = (u32, &'a str)>) -> Vec<(u32, String)> {
   let mut spans: Vec<(u32, String)> = Vec::new();
   for (color, tok) in tokens {
      let blank = tok.trim().is_empty();
      match spans.last_mut() {
         Some((last, text)) if blank || *last == color || text.trim().is_empty() => {
            if !blank {
               *last = color;
            }
            text.push_str(tok);
         },
         _ => spans.push((color, tok.to_owned())),
      }
   }
   spans
}

fn build_code_box(
   msym: &ModalSymbols,
   box_id: &str,
   text: &str,
   monospace: bool,
   do_highlight: bool,
) -> Option<RawDiv> {
   let mut box_div = gpui::v_stack(1.0)?;
   box_div = div_with_id(msym, box_div, box_id);
   gpui::style_div(&mut box_div, |s| {
      // Dark Discord code-block background + border + horizontal scroll.
      s.solid_background([0.62, 0.10, 0.08, 1.0]);
      s.lengths(&s.at.border_widths, gpui_px(1.0));
      s.lengths(&s.at.corner_radii, gpui_px(8.0));
      s.lengths(&s.at.padding, gpui_px(12.0));
      s.length(s.at.size[0], W_FULL);
      s.hsla(s.at.border_color, [0.62, 0.08, 0.45, 0.25]);
      s.set(s.at.overflow_x, OVERFLOW_SCROLL);
      if monospace {
         set_mono_font(s, 12.5, 18.0);
      }
   });

   let is_json = do_highlight && highlight::looks_like_json(text);
   for (idx, line) in text.split('\n').enumerate() {
      let line = line.strip_suffix('\r').unwrap_or(line);
      if line.is_empty() {
         if let Some(sp) = mono_span(" ", COL_PLAIN) {
            box_div = gpui::push_div(box_div, sp);
         }
         continue;
      }
      if do_highlight && idx < MAX_HIGHLIGHT_LINES {
         let Some(mut row) = gpui::h_row(0.0, false) else {
            continue;
         };
         gpui::style_div(&mut row, |s| {
            s.set(s.at.white_space, WHITE_SPACE_NOWRAP);
            s.set(s.at.flex_wrap, FLEX_NOWRAP);
         });
         let spans = if is_json {
            coalesce(highlight::json(line).into_iter().map(|(kind, tok)| {
               let color = match kind {
                  JsonKind::Key => COL_KEY,
                  JsonKind::String => COL_STRING,
                  JsonKind::Number => COL_NUMBER,
                  JsonKind::Bool => COL_BOOL,
                  JsonKind::Null => COL_NULL,
                  JsonKind::Punctuation => COL_PUNCT,
                  JsonKind::Plain => COL_PLAIN,
               };
               (color, tok)
            }))
         } else {
            coalesce(highlight::js(line).into_iter().map(|(kind, tok)| {
               let color = match kind {
                  Kind::Keyword => COL_KEYWORD,
                  Kind::String => COL_STRING,
                  Kind::Number => COL_NUMBER,
                  Kind::Comment => COL_COMMENT,
                  Kind::Plain => COL_PLAIN,
               };
               (color, tok)
            }))
         };
         for (color, text) in &spans {
            if let Some(span) = mono_span(text, *color) {
               row = gpui::push_div(row, span);
            }
         }
         box_div = gpui::push_div(box_div, row);
      } else if monospace {
         if let Some(span) = mono_span(line, COL_PLAIN) {
            box_div = gpui::push_div(box_div, span);
         }
      } else if let Some(div) = plain_text_div(line, COL_PLAIN, 13.0) {
         box_div = gpui::push_div(box_div, div);
      }
   }
   Some(box_div)
}

fn build_page_body(msym: &ModalSymbols, spec: &PageModalSpec) -> Option<RawDiv> {
   let mut body = gpui::v_stack(14.0)?;
   gpui::style_div(&mut body, |s| s.length(s.at.size[0], W_FULL));
   for (sec_idx, section) in spec.sections.iter().enumerate() {
      match section {
         ModalSection::Buttons { title, buttons } => {
            let mut block = gpui::v_stack(8.0)?;
            if let Some(t) = title
               && !t.is_empty()
               && let Some(hdr) = plain_text_div(t, COL_MUTED, 12.0)
            {
               block = gpui::push_div(block, hdr);
            }
            let mut row = gpui::h_row(8.0, false)?;
            gpui::style_div(&mut row, |s| s.set(s.at.flex_wrap, FLEX_WRAP));
            for (btn_idx, b) in buttons.iter().enumerate() {
               row = push_modal_button(
                  row,
                  &format!("ruby-modal-btn-{sec_idx}-{btn_idx}"),
                  &b.label,
                  if b.danger {
                     BtnVariant::Critical
                  } else {
                     BtnVariant::Secondary
                  },
                  BtnSize::Small,
                  ModalAction::PageButton(Arc::clone(&b.on_press)),
               );
            }
            block = gpui::push_div(block, row);
            body = gpui::push_div(body, block);
         },
         ModalSection::Text { title, text } => {
            let mut block = gpui::v_stack(6.0)?;
            if let Some(t) = title
               && !t.is_empty()
               && let Some(hdr) = plain_text_div(t, COL_MUTED, 12.0)
            {
               block = gpui::push_div(block, hdr);
            }
            for line in text.lines() {
               if let Some(d) = plain_text_div(line, COL_PLAIN, 13.5) {
                  block = gpui::push_div(block, d);
               }
            }
            body = gpui::push_div(body, block);
         },
         ModalSection::Code {
            title,
            text,
            monospace,
            highlight,
         } => {
            let mut block = gpui::v_stack(6.0)?;
            if let Some(t) = title
               && !t.is_empty()
               && let Some(hdr) = plain_text_div(t, COL_MUTED, 12.0)
            {
               block = gpui::push_div(block, hdr);
            }
            if let Some(code_box) = build_code_box(
               msym,
               &format!("ruby-code-{sec_idx}"),
               text,
               *monospace,
               *highlight,
            ) {
               block = gpui::push_div(block, code_box);
            }
            body = gpui::push_div(body, block);
         },
      }
   }
   Some(body)
}

#[expect(
   clippy::too_many_lines,
   reason = "assembles the single active native Mana Modal and scrim overlay"
)]
fn build_overlay(window: *mut c_void, app: *mut c_void) -> Option<RawDiv> {
   let msym = MODAL_SYM.get()?;

   // Snapshot active modal & take any one-shot `pending_init` text to push into
   // the native GPUI `FieldState`.
   let modal_snapshot = {
      let mut guard = ACTIVE_MODAL.lock().unwrap_or_else(PoisonError::into_inner);
      let Some(active) = guard.as_mut() else {
         MODAL_OPEN.store(false, Ordering::Release);
         return None;
      };
      let snapshot = match active {
         ActiveModal::Page { spec, .. } | ActiveModal::Choice { spec, .. } => {
            Snapshot::Page(spec.clone())
         },
         ActiveModal::CodeEditor(ed) => Snapshot::CodeEditor(CodeEditorModal {
            plugin: ed.plugin.clone(),
            pending_init: ed.pending_init.take(),
            status: ed.status.clone(),
         }),
         ActiveModal::InstallUrl(inst) => Snapshot::InstallUrl(InstallUrlModal {
            target: inst.target.clone(),
            pending_init: inst.pending_init.take(),
            status: inst.status.clone(),
            busy: inst.busy,
         }),
      };
      drop(guard);
      snapshot
   };

   MODAL_ACTIONS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clear();

   let (title, subtitle, size_code, body_opt, footer_buttons): (
      String,
      Option<String>,
      u8,
      Option<RawDiv>,
      Vec<RawButton>,
   ) = match modal_snapshot {
      Snapshot::Page(spec) => {
         let body = build_page_body(msym, &spec);
         let mut footer = Vec::new();
         if let Some(close_btn) = make_modal_button(
            "ruby-modal-close",
            "Close",
            BtnVariant::Secondary,
            BtnSize::Medium,
            ModalAction::Close,
         ) {
            footer.push(close_btn);
         }
         (spec.title, None, 3, body, footer)
      },
      Snapshot::CodeEditor(ed) => {
         let mut body = gpui::v_stack(10.0);
         if let Some(ref mut b) = body {
            gpui::style_div(b, |s| s.length(s.at.size[0], W_FULL));
            if let Some((msg, is_err)) = &ed.status
               && let Some(st) = plain_text_div(msg, if *is_err { COL_ERROR } else { COL_OK }, 13.0)
            {
               *b = gpui::push_div(std::mem::replace(b, RawDiv::zeroed()), st);
            }
            if let Some(tf) = FIELD_SYM.get()
               && let Some(ent) = unsafe {
                  ensure_field_entity(app, window, true, "// Plugin JavaScript source...")
               }
            {
               if let Some(init) = &ed.pending_init {
                  unsafe { set_field_text(app, &ent, init) };
               }
               // SAFETY: constructs hosted `TextArea` from
               // `&Entity<FieldState>` in a buffer that fits it and converts to
               // `AnyElement`.
               let mut ta_any = unsafe {
                  let mut ta = RawTextArea::zeroed();
                  (tf.text_area_new)(&raw mut ta, &raw const ent);
                  // Unset, Mana shows a few lines; fix it at a page of code.
                  if let Some(rows) = tf.layout.text_area_rows {
                     ta.put(rows, [1u8, EDITOR_ROWS]);
                     ta.put(rows + 2, [1u8, EDITOR_ROWS]);
                  }
                  let mut any = MaybeUninit::<RawAnyElement>::uninit();
                  (tf.text_area_into_any)(any.as_mut_ptr(), &raw mut ta);
                  any.assume_init()
               };
               if let Some(mut wrap) = gpui::v_stack(4.0) {
                  gpui::style_div(&mut wrap, |s| {
                     s.length(s.at.size[0], W_FULL);
                     s.shared_string(s.at.font_family, SharedString::from_static(MONO_FONT));
                     s.length(s.at.font_size, gpui_px(12.5));
                  });
                  if gpui::push_any(&mut wrap, &mut ta_any) {
                     *b = gpui::push_div(std::mem::replace(b, RawDiv::zeroed()), wrap);
                  }
               }
            } else if let Some(msg) = plain_text_div(
               "Native TextArea could not be resolved on this Discord build.",
               COL_ERROR,
               13.5,
            ) {
               *b = gpui::push_div(std::mem::replace(b, RawDiv::zeroed()), msg);
            }
         }
         let mut footer = Vec::new();
         if let Some(btn) = make_modal_button(
            "ruby-edit-cancel",
            "Cancel",
            BtnVariant::Secondary,
            BtnSize::Medium,
            ModalAction::Close,
         ) {
            footer.push(btn);
         }
         if let Some(btn) = make_modal_button(
            "ruby-edit-copy",
            "Copy Source",
            BtnVariant::Secondary,
            BtnSize::Medium,
            ModalAction::CopyCode,
         ) {
            footer.push(btn);
         }
         if let Some(btn) = make_modal_button(
            "ruby-edit-save",
            "Save & Reload",
            BtnVariant::Primary,
            BtnSize::Medium,
            ModalAction::SaveCode(ed.plugin.clone()),
         ) {
            footer.push(btn);
         }
         (
            format!("Edit Plugin · {}", ed.plugin),
            None,
            3,
            body,
            footer,
         )
      },
      Snapshot::InstallUrl(inst) => {
         let lines =
            matches!(&inst.target, FieldTarget::Setting(field) if field.kind == FieldKind::Lines);
         let mut body = gpui::v_stack(10.0);
         if let Some(ref mut b) = body {
            gpui::style_div(b, |s| s.length(s.at.size[0], W_FULL));
            if let Some(tf) = FIELD_SYM.get()
               && let Some(ent) = unsafe {
                  ensure_field_entity(
                     app,
                     window,
                     lines,
                     match &inst.target {
                        FieldTarget::Install => "https://example.com/plugin.js",
                        FieldTarget::Setting(field) if field.kind == FieldKind::Secret => {
                           "Leave empty to clear"
                        },
                        FieldTarget::Setting(_) | FieldTarget::Prompt(_) => "",
                     },
                  )
               }
            {
               if let Some(init) = &inst.pending_init {
                  unsafe { set_field_text(app, &ent, init) };
               }
               // SAFETY: constructs hosted `TextInput`, or `TextArea` for a
               // multiline setting, from `&Entity<FieldState>` in a buffer that
               // fits it and converts to `AnyElement`.
               let mut ti_any = unsafe {
                  let mut any = MaybeUninit::<RawAnyElement>::uninit();
                  if lines {
                     let mut ta = RawTextArea::zeroed();
                     (tf.text_area_new)(&raw mut ta, &raw const ent);
                     if let Some(rows) = tf.layout.text_area_rows {
                        ta.put(rows, [1u8, SETTING_ROWS]);
                        ta.put(rows + 2, [1u8, SETTING_ROWS]);
                     }
                     (tf.text_area_into_any)(any.as_mut_ptr(), &raw mut ta);
                  } else {
                     let mut ti = RawTextInput::zeroed();
                     (tf.text_input_new)(&raw mut ti, &raw const ent);
                     (tf.text_input_into_any)(any.as_mut_ptr(), &raw mut ti);
                  }
                  any.assume_init()
               };
               if let Some(mut wrap) = gpui::v_stack(4.0) {
                  gpui::style_div(&mut wrap, |s| s.length(s.at.size[0], W_FULL));
                  if gpui::push_any(&mut wrap, &mut ti_any) {
                     *b = gpui::push_div(std::mem::replace(b, RawDiv::zeroed()), wrap);
                  }
               }
            } else if let Some(msg) = plain_text_div(
               "Native TextInput could not be resolved on this Discord build.",
               COL_ERROR,
               13.5,
            ) {
               *b = gpui::push_div(std::mem::replace(b, RawDiv::zeroed()), msg);
            }
            if let Some((msg, is_err)) = &inst.status
               && let Some(st) = plain_text_div(msg, if *is_err { COL_ERROR } else { COL_OK }, 13.0)
            {
               *b = gpui::push_div(std::mem::replace(b, RawDiv::zeroed()), st);
            }
         }
         let mut footer = Vec::new();
         if let Some(btn) = make_modal_button(
            "ruby-url-cancel",
            "Cancel",
            BtnVariant::Secondary,
            BtnSize::Medium,
            ModalAction::Close,
         ) {
            footer.push(btn);
         }
         if !inst.busy
            && let Some(btn) = make_modal_button(
               "ruby-url-install",
               match inst.target {
                  FieldTarget::Install => "Install Plugin",
                  FieldTarget::Setting(_) => "Save",
                  FieldTarget::Prompt(_) => "Send",
               },
               BtnVariant::Primary,
               BtnSize::Medium,
               ModalAction::SubmitInstallUrl,
            )
         {
            footer.push(btn);
         }
         let (title, subtitle) = match &inst.target {
            FieldTarget::Setting(field) => (field.title.clone(), Some(field.plugin.clone())),
            FieldTarget::Prompt(prompt) => (prompt.title.clone(), None),
            FieldTarget::Install => (
               "Install Plugin from URL".to_owned(),
               Some("Paste an https:// URL pointing to a Ruby .js plugin bundle.".to_owned()),
            ),
         };
         (title, subtitle, 2, body, footer)
      },
   };

   let mut body_div = body_opt?;
   let close_rc = gpui::alloc_rc_u64(0)?;

   // Build `mana_html::hosted::modal::Modal`.
   let at = &msym.layout;
   let title_ss = SharedString::from_str(&title);
   let mut m = RawModal::zeroed();
   // SAFETY: `Modal::new` builds into a buffer that fits a `Modal`.
   unsafe { (msym.modal_new)(&raw mut m, &raw const title_ss) };
   m.put(at.scale, size_code);
   if let Some(sub) = subtitle {
      m.put(at.subtitle, SharedString::from_str(&sub));
   }
   m.put(at.on_close, close_rc);
   m.put(at.on_close + 8, &raw const MODAL_CLOSE_VTBL);
   // SAFETY: `modal_child_div`, `modal_footer_button` and `modal_into_any`
   // take the `Modal` by value and return it through buffers that fit it.
   let mut modal_any = unsafe {
      let mut next = RawModal::zeroed();
      (msym.modal_child_div)(&raw mut next, &raw mut m, &raw mut body_div);
      let mut m = next;

      for mut btn in footer_buttons {
         let mut next = RawModal::zeroed();
         (msym.modal_footer_button)(&raw mut next, &raw mut m, &raw mut btn);
         m = next;
      }

      let mut any = MaybeUninit::<RawAnyElement>::uninit();
      (msym.modal_into_any)(any.as_mut_ptr(), &raw mut m);
      any.assume_init()
   };

   // Build full-window scrim overlay (`position: absolute; inset: 0; flex;
   // center; bg rgba(0,0,0,0.68)`).
   let mut scrim = gpui::raw_div()?;
   gpui::style_div(&mut scrim, |s| {
      s.set(s.at.position, POSITION_ABSOLUTE);
      s.lengths(&s.at.inset, gpui_px(0.0));
      s.lengths(&s.at.size, W_FULL);
      s.set(s.at.display, DISPLAY_FLEX);
      s.set(s.at.align_items, ALIGN_CENTER);
      s.set(s.at.justify_content, ALIGN_CENTER);
      s.solid_background([0.0, 0.0, 0.0, 0.68]);
   });
   // The card wraps the modal at its own size, so a press inside it can be told
   // apart from one on the scrim around it.
   let mut card = gpui::raw_div()?;
   gpui::style_div(&mut card, |s| s.set(s.at.display, DISPLAY_FLEX));
   if !gpui::push_any(&mut card, &mut modal_any) {
      return None;
   }
   on_mouse_down(&mut scrim, &SCRIM_PRESS_VTBL);
   on_mouse_down(&mut card, &CARD_PRESS_VTBL);

   Some(gpui::push_div(scrim, card))
}

#[cfg(test)]
mod tests {
   #[test]
   fn coalesce_folds_whitespace_and_same_colors() {
      let tokens = [
         (1, "  "),
         (2, "\"k\""),
         (3, ":"),
         (1, " "),
         (4, "1"),
         (4, "2"),
         (3, ","),
      ];
      assert_eq!(
         super::coalesce(tokens.into_iter()),
         vec![
            (2, "  \"k\"".to_owned()),
            (3, ": ".to_owned()),
            (4, "12".to_owned()),
            (3, ",".to_owned()),
         ]
      );
   }
}
