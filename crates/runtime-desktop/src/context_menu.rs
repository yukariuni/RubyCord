//! Native GPUI message context menu (`phoenix.ui.addMessageAction`) for
//! Desktop Phoenix.
//!
//! Hooks Discord's `MessageContextMenu` item builder (called by both the
//! right-click context menu and the `...` message action popover), reads
//! `message_id` and `channel_id` from the input before Discord consumes it,
//! and appends a native `MenuItem::Separator` plus each visible plugin
//! `MenuItem::Action` with a native `Rc<dyn Fn>` click callback.

use std::{
   collections::{HashMap, VecDeque},
   ffi::c_void,
   ptr,
   sync::{
      LazyLock, Mutex, OnceLock, PoisonError, RwLock,
      atomic::{AtomicU64, AtomicUsize, Ordering},
      mpsc,
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop::{self, MenuLayout, icon_names, layout};

use crate::{
   double_click,
   gpui::{self, RcFnVtbl, Scratch, SharedString},
};

/// `Vec<MenuItem>` in Discord's Rust ABI (`[cap, ptr, len]`).
#[repr(C)]
pub struct RawMenuVec {
   cap: usize,
   ptr: *mut c_void,
   len: usize,
}

type RawMenuItem = Scratch<{ layout::SCRATCH_CAP }>;

type BuildMenuFn = unsafe extern "C" fn(
   sret: *mut RawMenuVec,
   input: *const c_void,
   a2: usize,
   a3: usize,
   s0: u64,
   s1: u64,
   s2: u64,
   s3: u64,
) -> *mut RawMenuVec;

type PropsNewFn = unsafe extern "C" fn(*mut u8, *const SharedString, u8, u8) -> *mut u8;
type WithIdFn =
   unsafe extern "C" fn(*mut RawMenuItem, *mut RawMenuItem, *const u8, usize) -> *mut RawMenuItem;
type WithIconFn = unsafe extern "C" fn(*mut RawMenuItem, *mut RawMenuItem, u16) -> *mut RawMenuItem;
type WithColorFn = unsafe extern "C" fn(*mut RawMenuItem, *mut RawMenuItem, u8) -> *mut RawMenuItem;
type VecPushFn = unsafe extern "C" fn(*mut RawMenuVec, *mut RawMenuItem);

struct MenuSymbols {
   /// x86's [`BuildMenuFn`]; arm64 keeps its own in `arm64::ORIG`.
   orig_build: Option<BuildMenuFn>,
   msg_ptr_offset: usize,
   msg_id_offset: usize,
   channel_id_offset: usize,
   props_new: PropsNewFn,
   with_id: WithIdFn,
   with_icon: WithIconFn,
   /// Not located on arm64, where danger items keep the default color.
   with_color: Option<WithColorFn>,
   vec_push: VecPushFn,
   /// `MenuItem::Separator`'s tag, `None` when arm64's push doesn't decode.
   separator_tag: Option<u64>,
   layout: MenuLayout,
   /// Where `with_id` keeps a short id inline, its length byte first.
   id_offset: Option<usize>,
}

// SAFETY: function pointers and plain struct offsets into `Discord.exe`.
unsafe impl Send for MenuSymbols {}
unsafe impl Sync for MenuSymbols {}

static MENU_SYMS: OnceLock<MenuSymbols> = OnceLock::new();

/// Snapshot of one registered `phoenix.ui.addMessageAction` item, read on the
/// GPUI thread when a message context menu opens.
#[derive(Clone, Debug)]
pub struct ActionMeta {
   pub id: u64,
   pub label: String,
   pub icon: Option<String>,
   pub danger: bool,
   pub priority: i32,
   pub has_visible: bool,
}

static ACTIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
static ACTIONS: LazyLock<RwLock<Vec<ActionMeta>>> = LazyLock::new(|| RwLock::new(Vec::new()));

/// Maps an opaque click token stored inside `RcInner::action_id` to the
/// `(action_id, message_id, channel_id)` to invoke when the user clicks it.
#[derive(Clone, Copy)]
struct PendingClick {
   action: u64,
   message: u64,
   channel: u64,
}

const MAX_PENDING_CLICKS: usize = 512;

struct ClickTable {
   order: VecDeque<u64>,
   map: HashMap<u64, PendingClick>,
}

static NEXT_CLICK_TOKEN: AtomicU64 = AtomicU64::new(1);
static CLICKS: LazyLock<Mutex<ClickTable>> = LazyLock::new(|| {
   Mutex::new(ClickTable {
      order: VecDeque::new(),
      map: HashMap::new(),
   })
});
static WORKER_TX: OnceLock<mpsc::Sender<PendingClick>> = OnceLock::new();

static MENU_ACTION_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size: size_of::<u64>(),
   align: align_of::<u64>(),
   call_once: menu_action_callback as *const c_void,
   call_mut: menu_action_callback as *const c_void,
   call: menu_action_callback as *const c_void,
   extra: ptr::null(),
};

/// Updates the menu-thread snapshot of registered `addMessageAction` entries.
pub fn sync_actions(mut items: Vec<ActionMeta>) {
   items.sort_by_key(|item| (item.priority, item.id));
   let len = items.len();
   if let Ok(mut guard) = ACTIONS.write() {
      *guard = items;
   }
   ACTIVE_COUNT.store(len, Ordering::Release);
   if len > 0 {
      ensure_worker();
   }
}

fn ensure_worker() -> &'static mpsc::Sender<PendingClick> {
   WORKER_TX.get_or_init(|| {
      let (tx, rx) = mpsc::channel::<PendingClick>();
      let _ = std::thread::Builder::new()
         .name("rubycord-msg-act".to_owned())
         .spawn(move || {
            while let Ok(click) = rx.recv() {
               if let Some(engine) = phoenix_runtime::engine() {
                  crate::js::ui::invoke_message_action(
                     &engine,
                     click.action,
                     click.message,
                     click.channel,
                  );
               }
            }
         });
      tx
   })
}

fn register_click(click: PendingClick) -> u64 {
   let token = NEXT_CLICK_TOKEN.fetch_add(1, Ordering::Relaxed);
   let mut table = CLICKS.lock().unwrap_or_else(PoisonError::into_inner);
   while table.order.len() >= MAX_PENDING_CLICKS {
      if let Some(old) = table.order.pop_front() {
         table.map.remove(&old);
      }
   }
   table.order.push_back(token);
   table.map.insert(token, click);
   token
}

unsafe extern "C" fn menu_action_callback(
   data: *const u64,
   _event: *const c_void,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   if data.is_null() {
      return;
   }
   // SAFETY: `data` points to `RcInner::action_id` allocated by `alloc_rc_u64`.
   let token = unsafe { ptr::read_unaligned(data) };
   let click = CLICKS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .map
      .get(&token)
      .copied();
   if let Some(click) = click {
      let _ = ensure_worker().send(click);
   }
}

/// Mana's icon names in id order, from [`icon_names`].
static ICON_NAMES: OnceLock<Vec<String>> = OnceLock::new();

/// A plugin's icon as a Mana icon id: `res://ic_mana_angle_brackets` is
/// `AngleBrackets`. Without an icon, the puzzle piece mobile shows too.
fn map_icon(icon: Option<&str>) -> Option<u16> {
   let Some(names) = ICON_NAMES.get() else {
      return legacy_icon(icon?);
   };
   let id = |name: &str| {
      names
         .binary_search_by(|probe| probe.as_str().cmp(name))
         .ok()
         .and_then(|at| u16::try_from(at).ok())
   };
   let Some(icon) = icon else {
      return id("PuzzlePiecePlus");
   };
   let snake = icon.rsplit('/').next().unwrap_or(icon);
   let snake = snake.strip_prefix("ic_mana_").unwrap_or(snake);
   let camel: String = snake
      .split('_')
      .map(|word| {
         let mut chars = word.chars();
         chars.next().map_or_else(String::new, |first| {
            first.to_ascii_uppercase().to_string() + chars.as_str()
         })
      })
      .collect();
   id(&camel)
      .or_else(|| id(alias(snake)?))
      .or_else(|| id("PuzzlePiecePlus"))
}

/// The icon Mana names differently from mobile's drawable.
fn alias(icon: &str) -> Option<&'static str> {
   if icon.contains("delete") || icon.contains("trash") {
      Some("Trash")
   } else if icon.contains("copy_id") {
      Some("Id")
   } else if icon.contains("link") {
      Some("Link")
   } else if icon.contains("copy") || icon.contains("code") {
      Some("Copy")
   } else {
      None
   }
}

/// Mana icon ids seen on 2026-11 Windows builds, for a build whose icon table
/// isn't found.
fn legacy_icon(icon: &str) -> Option<u16> {
   if icon.contains("delete") || icon.contains("trash") {
      Some(0x280)
   } else if icon.contains("copy_id") || icon.contains("id") {
      Some(0x15B)
   } else if icon.contains("link") {
      Some(0x177)
   } else if icon.contains("copy") || icon.contains("angle_brackets") || icon.contains("code") {
      Some(0x099)
   } else {
      None
   }
}

/// Locates and hooks `MessageContextMenu` from `DllMain`.
pub fn install(image: &Image<'static>) {
   match icon_names(image) {
      Ok(names) => {
         let _ = ICON_NAMES.set(names);
      },
      Err(err) => log::warn!("desktop context menu: {err}, menu icons from the built-in list"),
   }
   if !crate::X86_CODE {
      #[cfg(target_os = "macos")]
      arm64::install(image);
      return;
   }
   let menu = match desktop::message_context_menu(image) {
      Ok(menu) => menu,
      Err(err) => {
         log::warn!("desktop context menu: {err}, skipping");
         return;
      },
   };

   // SAFETY: runs from `prepare` before Discord threads execute;
   // `build_context_menu_detour` matches `MessageContextMenu`'s 8-arg sret ABI.
   let Some(orig_build) = (unsafe {
      crate::hook::<BuildMenuFn>(
         image,
         |pro| pro.row,
         menu.build_fn,
         build_context_menu_detour as *const () as usize,
         "desktop context menu: hook failed",
      )
   }) else {
      return;
   };

   // SAFETY: every address was validated by `desktop::message_context_menu`.
   let syms = unsafe {
      MenuSymbols {
         orig_build: Some(orig_build),
         msg_ptr_offset: menu.msg_ptr_offset,
         msg_id_offset: menu.msg_id_offset,
         channel_id_offset: menu.channel_id_offset,
         props_new: std::mem::transmute::<usize, PropsNewFn>(menu.props_new),
         with_id: std::mem::transmute::<usize, WithIdFn>(menu.with_id),
         with_icon: std::mem::transmute::<usize, WithIconFn>(menu.with_icon),
         with_color: Some(std::mem::transmute::<usize, WithColorFn>(menu.with_color)),
         vec_push: std::mem::transmute::<usize, VecPushFn>(menu.vec_push),
         separator_tag: Some(layout::MENU_ITEM_SEPARATOR_TAG),
         layout: menu.layout,
         id_offset: None,
      }
   };
   let syms = MenuSymbols {
      id_offset: inline_id_offset(&syms),
      ..syms
   };
   let _ = MENU_SYMS.set(syms);
   log::info!(
      "desktop context menu: hooked MessageContextMenu @ {:#x} (msg +{:#x}, id +{:#x}, ch +{:#x}) \
       {:x?}",
      menu.build_fn,
      menu.msg_ptr_offset,
      menu.msg_id_offset,
      menu.channel_id_offset,
      menu.layout,
   );
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn build_context_menu_detour(
   sret: *mut RawMenuVec,
   input: *const c_void,
   a2: usize,
   a3: usize,
   s0: u64,
   s1: u64,
   s2: u64,
   s3: u64,
) -> *mut RawMenuVec {
   let Some(syms) = MENU_SYMS.get() else {
      return sret;
   };

   // Read `message_id` and `channel_id` BEFORE calling `orig_build`, because
   // `MessageContextMenu` takes `input` by value and drops it before returning.
   let wanted = ACTIVE_COUNT.load(Ordering::Acquire) > 0 || double_click::pending();
   let (message_id, channel_id) = if wanted && !input.is_null() {
      // SAFETY: `input` is a live `MessageContextMenuInput`; offsets were
      // decoded from `MessageContextMenu` and its permission check callee.
      unsafe {
         let bytes = input.cast::<u8>();
         let mid = bytes.add(syms.msg_id_offset).cast::<u64>().read_unaligned();
         let msg_ptr = bytes
            .add(syms.msg_ptr_offset)
            .cast::<*const u8>()
            .read_unaligned();
         let cid = if msg_ptr.is_null() {
            0
         } else {
            remember_message(msg_ptr, mid);
            msg_ptr
               .add(syms.channel_id_offset)
               .cast::<u64>()
               .read_unaligned()
         };
         (mid, cid)
      }
   } else {
      (0, 0)
   };

   let Some(orig_build) = syms.orig_build else {
      return sret;
   };
   // SAFETY: forwards all arguments verbatim to the original menu builder.
   let out = unsafe { orig_build(sret, input, a2, a3, s0, s1, s2, s3) };

   if !sret.is_null()
      && let Some(item) = double_click::take_press(message_id)
   {
      let _ = std::panic::catch_unwind(|| {
         press_item(syms, sret, &item, a3 as *mut c_void, s2 as *mut c_void);
      });
   }

   if message_id == 0 || sret.is_null() || ACTIVE_COUNT.load(Ordering::Acquire) == 0 {
      return out;
   }

   let _ = std::panic::catch_unwind(|| {
      append_plugin_actions(syms, sret, message_id, channel_id);
   });
   out
}

/// The JSON of the message the last menu opened on, read from Discord's memory
/// while the menu's input still holds it.
static MENU_MESSAGE: Mutex<Option<(u64, std::time::Instant, String)>> = Mutex::new(None);
/// GPUI rebuilds an open menu every frame, so its message is serialized again
/// only past this age, which still catches an edit before the next menu.
const MENU_MESSAGE_FRESH: std::time::Duration = std::time::Duration::from_millis(500);

/// Serializes the message at `message` for the menu built for it.
///
/// # Safety
///
/// `message` points to the live `Message` the menu is built for.
unsafe fn remember_message(message: *const u8, id: u64) {
   if MENU_MESSAGE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_ref()
      .is_some_and(|(held, at, _)| *held == id && at.elapsed() < MENU_MESSAGE_FRESH)
   {
      return;
   }
   // SAFETY: the caller's `message` is live for this call.
   if let Some(json) = unsafe { crate::message::json(message, id) } {
      *MENU_MESSAGE.lock().unwrap_or_else(PoisonError::into_inner) =
         Some((id, std::time::Instant::now(), json));
   }
}

/// The JSON of message `id`, when the last menu was opened on it.
pub fn menu_message(id: u64) -> Option<String> {
   MENU_MESSAGE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_ref()
      .filter(|(held, ..)| *held == id)
      .map(|(.., json)| json.clone())
}

fn append_plugin_actions(
   syms: &MenuSymbols,
   vec: *mut RawMenuVec,
   message_id: u64,
   channel_id: u64,
) {
   let actions: Vec<ActionMeta> = ACTIONS
      .read()
      .map_or_else(|_| Vec::new(), |guard| guard.clone());
   if actions.is_empty() {
      return;
   }

   let visible: Vec<ActionMeta> = actions
      .into_iter()
      .filter(|action| {
         if !action.has_visible {
            return true;
         }
         phoenix_runtime::engine().is_none_or(|engine| {
            crate::js::ui::check_action_visible(&engine, action.id, message_id, channel_id)
         })
      })
      .collect();
   if visible.is_empty() {
      return;
   }

   // Push a native separator first if the menu already has Discord's own items.
   // SAFETY: `vec` is the live `Vec<MenuItem>` just returned by `orig_build`.
   let has_existing = unsafe { (*vec).len > 0 };
   if has_existing && let Some(tag) = syms.separator_tag {
      let mut sep = RawMenuItem::zeroed();
      sep.put(0, tag);
      // SAFETY: `vec_push` copies one `MenuItem`, which fits `sep`.
      unsafe { (syms.vec_push)(vec, &raw mut sep) };
   }
   let at = &syms.layout;

   for action in visible {
      let token = register_click(PendingClick {
         action: action.id,
         message: message_id,
         channel: channel_id,
      });
      let rc_ptr = if crate::X86_CODE {
         let Some(rc_ptr) = gpui::alloc_rc_u64(token) else {
            continue;
         };
         rc_ptr
      } else {
         gpui::pinned_rc_u64(token)
      };
      let label_ss = SharedString::from_str(&action.label);
      let id_str = gpui::intern_str(&format!("rubycord-msg-action-{}", action.id));

      // A native `MenuItem::Action` with our `Rc<dyn Fn>` click handler, then
      // Discord's `MenuItemProps`, id, optional icon and optional danger color
      // (`2`), at the offsets `sigs` decoded.
      let mut item = RawMenuItem::zeroed();
      item.put(0, at.action_tag);
      item.put(at.on_click_tag, 0u64);
      item.put(at.on_click, rc_ptr);
      item.put(at.on_click + 8, &raw const MENU_ACTION_VTBL);
      // SAFETY: `props_new` fills `props_size` bytes at `props`, inside the
      // item; the builders take the item by value and return it through
      // buffers that fit it.
      unsafe {
         (syms.props_new)(item.as_mut_ptr().add(at.props), &raw const label_ss, 0, 0);

         let mut next = RawMenuItem::zeroed();
         (syms.with_id)(&raw mut next, &raw mut item, id_str.as_ptr(), id_str.len());
         let mut item = next;

         if let Some(icon_id) = map_icon(action.icon.as_deref()) {
            let mut next = RawMenuItem::zeroed();
            (syms.with_icon)(&raw mut next, &raw mut item, icon_id);
            item = next;
         }

         if action.danger
            && let Some(with_color) = syms.with_color
         {
            let mut next = RawMenuItem::zeroed();
            with_color(&raw mut next, &raw mut item, 2);
            item = next;
         }

         (syms.vec_push)(vec, &raw mut item);
      }
   }
}

/// `MessageContextMenu` on macOS, arm64
/// ([`phoenix_sigs::macos::message_context_menu`]).
///
/// The builder takes `x0` input, `x1` the context the item constructors take,
/// `x2`..`x5` and `d0`, and returns `Vec<MenuItem>` through `x8`, which a
/// Rust `extern "C"` function returning a 24-byte struct does too, so the
/// detour is native. The item constructors take their result slot in `x8`
/// and are called through `image::sret_caller` thunks, so
/// [`append_plugin_actions`] calls them the x86 way.
#[cfg(target_os = "macos")]
mod arm64 {
   use std::{
      ffi::c_void,
      sync::{OnceLock, atomic::Ordering},
   };

   use phoenix_hook::image::{Image, sret_caller};

   use super::{ACTIVE_COUNT, MENU_SYMS, MenuSymbols, RawMenuVec, append_plugin_actions};

   type BuildFn = unsafe extern "C" fn(
      input: *const c_void,
      a1: usize,
      a2: usize,
      a3: usize,
      a4: usize,
      a5: usize,
      a6: usize,
      a7: usize,
      f0: f64,
      f1: f64,
      f2: f64,
      f3: f64,
   ) -> RawMenuVec;

   static ORIG: OnceLock<BuildFn> = OnceLock::new();
   static IDS: OnceLock<(usize, usize, usize)> = OnceLock::new();

   /// Discord ids are snowflakes: a millisecond timestamp past 2015 in the top
   /// 42 bits, so anything far smaller or in the future isn't one.
   fn snowflake(value: u64) -> bool {
      (1 << 32..1 << 63).contains(&value)
   }

   pub fn install(image: &Image<'static>) {
      let menu = match phoenix_sigs::macos::message_context_menu(image) {
         Ok(menu) => menu,
         Err(err) => {
            log::warn!("desktop context menu: {err}, skipping");
            return;
         },
      };
      let thunks = (
         sret_caller(menu.props_new),
         sret_caller(menu.with_id),
         sret_caller(menu.with_icon),
      );
      let (Some(props_new), Some(with_id), Some(with_icon)) = thunks else {
         log::warn!("desktop context menu: no memory for the constructor thunks");
         return;
      };
      // SAFETY: runs from `prepare` before Discord's threads; `detour` has the
      // builder's arm64 signature.
      let Some(orig) = (unsafe {
         crate::hook::<BuildFn>(
            image,
            |pro| pro.frame,
            menu.build_fn,
            detour as *const () as usize,
            "desktop context menu: hook failed",
         )
      }) else {
         return;
      };
      let _ = ORIG.set(orig);
      // SAFETY: the thunks take the constructors' arguments the x86 way, result
      // slot first, and `vec_push` is plain.
      unsafe {
         let _ = MENU_SYMS.set(MenuSymbols {
            orig_build: None,
            msg_ptr_offset: 0,
            msg_id_offset: menu.message_id_offset,
            channel_id_offset: menu.channel_id_offset,
            props_new: std::mem::transmute::<usize, super::PropsNewFn>(props_new),
            with_id: std::mem::transmute::<usize, super::WithIdFn>(with_id),
            with_icon: std::mem::transmute::<usize, super::WithIconFn>(with_icon),
            with_color: None,
            vec_push: std::mem::transmute::<usize, super::VecPushFn>(menu.vec_push),
            separator_tag: menu.separator_tag,
            layout: menu.layout,
            id_offset: None,
         });
      }
      let _ = IDS.set((menu.message_id_offset, menu.channel_id_offset, menu.message));
      log::info!("desktop context menu: hooked MessageContextMenu {menu:x?}");
   }

   unsafe extern "C" fn detour(
      input: *const c_void,
      a1: usize,
      a2: usize,
      a3: usize,
      a4: usize,
      a5: usize,
      a6: usize,
      a7: usize,
      f0: f64,
      f1: f64,
      f2: f64,
      f3: f64,
   ) -> RawMenuVec {
      let orig = *ORIG.get().expect("the detour only runs once ORIG is set");
      // The input is consumed by the builder, so its ids are read first.
      let ids = match (
         IDS.get(),
         input.is_null(),
         ACTIVE_COUNT.load(Ordering::Acquire),
      ) {
         (Some(&(id_at, channel_at, message_at)), false, 1..) => {
            // SAFETY: `input` is the live `MessageContextMenu` input, whose
            // ids and `&Message` the builder loads from these offsets.
            let (id, channel, message) = unsafe {
               (
                  input.byte_add(id_at).cast::<u64>().read_unaligned(),
                  input.byte_add(channel_at).cast::<u64>().read_unaligned(),
                  input
                     .byte_add(message_at)
                     .cast::<*const u8>()
                     .read_unaligned(),
               )
            };
            if snowflake(id) && !message.is_null() {
               // SAFETY: the input holds the live `&Message` the menu is for.
               unsafe { super::remember_message(message, id) };
            }
            (snowflake(id) && snowflake(channel)).then_some((id, channel))
         },
         _ => None,
      };
      // SAFETY: forwards every register to the original builder.
      let mut out = unsafe { orig(input, a1, a2, a3, a4, a5, a6, a7, f0, f1, f2, f3) };
      if let (Some((message, channel)), Some(syms)) = (ids, MENU_SYMS.get()) {
         let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            append_plugin_actions(syms, &raw mut out, message, channel);
         }));
      }
      out
   }
}

/// Finds where `with_id` stores a short id by giving one to a blank action
/// item. The id moves with the item's variant.
fn inline_id_offset(syms: &MenuSymbols) -> Option<usize> {
   const PROBE: &[u8] = b"zq";
   let mut blank = RawMenuItem::zeroed();
   blank.put(0, syms.layout.action_tag);
   let mut named = RawMenuItem::zeroed();
   // SAFETY: `with_id` reads one item and writes one into buffers that fit
   // it. A two-byte id stays inline, so `named` owns nothing to free.
   unsafe { (syms.with_id)(&raw mut named, &raw mut blank, PROBE.as_ptr(), PROBE.len()) };
   // SAFETY: `named` is `SCRATCH_CAP` bytes, which `layout.size` fits.
   let bytes = unsafe { std::slice::from_raw_parts(named.as_mut_ptr(), syms.layout.size) };
   bytes
      .windows(PROBE.len() + 1)
      .position(|window| window[0] as usize == PROBE.len() && &window[1..] == PROBE)
}

/// Runs the click handler of the item with id `id` in the menu `vec`, with
/// the builder's window and app. Pressing an item inside the builder leaves
/// the menu unshown, the same as a click on it.
fn press_item(
   syms: &MenuSymbols,
   vec: *mut RawMenuVec,
   id: &str,
   window: *mut c_void,
   app: *mut c_void,
) {
   type CallFn = unsafe extern "C" fn(*const u8, *const u8, *mut c_void, *mut c_void);

   let (Some(id_at), Ok(len)) = (syms.id_offset, u8::try_from(id.len())) else {
      return;
   };
   let at = &syms.layout;
   // SAFETY: `vec` is the `Vec<MenuItem>` the builder just returned, items
   // `at.size` bytes apart.
   let items = unsafe { std::slice::from_raw_parts((*vec).ptr.cast::<u8>(), (*vec).len * at.size) };
   let Some(item) = items.chunks_exact(at.size).find(|item| {
      item.get(id_at) == Some(&len)
         && item.get(id_at + 1..id_at + 1 + id.len()) == Some(id.as_bytes())
   }) else {
      log::warn!("double click: the menu has no {id:?}");
      return;
   };
   let word = |offset: usize| {
      item.get(offset..offset + 8).map_or(0, |bytes| {
         usize::from_le_bytes(bytes.try_into().unwrap_or_default())
      })
   };
   let (rc, vtbl) = (word(at.on_click), word(at.on_click + 8) as *const RcFnVtbl);
   if word(0) as u64 != at.action_tag || word(at.on_click_tag) != 0 || rc == 0 || vtbl.is_null() {
      return;
   }
   // The handler takes the app behind one more pointer than the builder gets
   // it, and no event. Passing the builder's app directly segfaults.
   let mut app = app;
   // SAFETY: an `Rc<dyn Fn>` Discord built, whose value follows the two counts
   // at the vtable's alignment.
   unsafe {
      let align = (*vtbl).align.max(1);
      let value = (rc + 16usize.div_ceil(align) * align) as *const u8;
      let call = std::mem::transmute::<*const c_void, CallFn>((*vtbl).call);
      call(value, ptr::null(), window, (&raw mut app).cast());
   }
}
