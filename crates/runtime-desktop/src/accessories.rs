//! Native GPUI message accessories (`phoenix.ui.addMessageAccessory`) for
//! Desktop Phoenix. A detour on `MessageRow::render` wraps the rendered row in
//! a `Div` with the plugins' lines underneath and an optional tinted accent
//! bar, and a detour on the body builder recolors the body text for a tint.

use std::{
   collections::HashMap,
   ffi::c_void,
   ptr,
   sync::{
      LazyLock, OnceLock, RwLock,
      atomic::{AtomicUsize, Ordering},
      mpsc,
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

use crate::{
   double_click,
   gpui::{self, RawAnyElement},
};

/// Offset of the pending/sending message text color inside `RowColors`
/// (verified at `0x14a741c1f`: `mov edx, [r12 + 0x34]`).
const ROW_COLORS_MUTED_DELTA: usize = 0x34;

type OptimisticDeleteFn =
   unsafe extern "C" fn(store: *mut c_void, args: *const *const u64, meta: *const c_void) -> u8;

type BuildMessageBodyFn = unsafe extern "C" fn(
   msg: *const c_void,
   config: *mut c_void,
   a2: u64,
   a3: u64,
   s0: u64,
   s1: u64,
) -> *mut c_void;

type RenderRowFn = unsafe extern "C" fn(
   sret: *mut RawAnyElement,
   row: *const c_void,
   a2: usize,
   a3: usize,
   s0: u64,
   s1: u64,
   s2: u64,
   s3: u64,
   s4: u64,
   s5: u64,
   s6: u64,
) -> *mut RawAnyElement;

/// One rendered plugin message accessory line for Desktop GPUI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedAccessory {
   pub priority: i32,
   pub text: String,
   pub color: Option<u32>,
   pub tint: Option<u32>,
   /// Discord's own SVG icons with their colors, drawn in place of `text`
   /// where icons can be drawn.
   pub icons: Vec<(&'static str, Option<u32>)>,
}

impl RenderedAccessory {
   pub fn piece(&self) -> gpui::Piece {
      gpui::Piece {
         text: self.text.clone(),
         color: self.color,
         icons: self.icons.clone(),
      }
   }
}

#[derive(PartialEq, Eq)]
enum CacheEntry {
   Pending,
   Ready(Vec<RenderedAccessory>),
}

static ORIG_RENDER_ROW: OnceLock<RenderRowFn> = OnceLock::new();
static ORIG_BUILD_BODY: OnceLock<BuildMessageBodyFn> = OnceLock::new();
static ORIG_OPT_DELETE: OnceLock<OptimisticDeleteFn> = OnceLock::new();
static MSG_ID_OFFSET: AtomicUsize = AtomicUsize::new(0);
static BODY_MSG_ID_OFFSET: AtomicUsize = AtomicUsize::new(0);
static ROW_COLORS_OFFSET: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
static CACHE: LazyLock<RwLock<HashMap<u64, CacheEntry>>> =
   LazyLock::new(|| RwLock::new(HashMap::new()));
static WORKER_TX: OnceLock<mpsc::Sender<u64>> = OnceLock::new();

const MAX_CACHE_ENTRIES: usize = 8192;

/// Caches `entry` for `message_id`, dropping everything once the cache is full,
/// and returns what it replaced.
fn cache(
   map: &mut HashMap<u64, CacheEntry>,
   message_id: u64,
   entry: CacheEntry,
) -> Option<CacheEntry> {
   if map.len() > MAX_CACHE_ENTRIES {
      map.clear();
   }
   map.insert(message_id, entry)
}

pub fn set_active_count(count: usize) {
   ACTIVE_COUNT.store(count, Ordering::Release);
   if count > 0 {
      ensure_worker();
   }
}

pub fn clear_cache() {
   if let Ok(mut map) = CACHE.write() {
      map.clear();
   }
}

/// Stores freshly evaluated accessories for `message_id` and requests a repaint
/// of Discord's window so edits/deletes appear immediately.
pub fn set_rendered(message_id: u64, mut items: Vec<RenderedAccessory>) {
   items.sort_by_key(|a| a.priority);
   let changed = CACHE.write().is_ok_and(|mut map| {
      let prev = cache(&mut map, message_id, CacheEntry::Ready(items));
      prev.as_ref() != map.get(&message_id)
   });
   if changed {
      invalidate_discord_windows();
   }
}

fn ensure_worker() -> &'static mpsc::Sender<u64> {
   WORKER_TX.get_or_init(|| {
      let (tx, rx) = mpsc::channel::<u64>();
      let _ = std::thread::Builder::new()
         .name("ruby-msg-acc".to_owned())
         .spawn(move || {
            while let Ok(id) = rx.recv() {
               // Coalesce burst requests for the same message ID.
               let mut batch = vec![id];
               while let Ok(more) = rx.try_recv() {
                  if !batch.contains(&more) {
                     batch.push(more);
                  }
               }
               let mut any_visible = false;
               for mid in batch {
                  if ACTIVE_COUNT.load(Ordering::Acquire) == 0 {
                     break;
                  }
                  let mut items = phoenix_runtime::engine()
                     .map_or_default(|engine| crate::js::ui::render_accessories(&engine, mid));
                  items.sort_by_key(|a| a.priority);
                  let non_empty = !items.is_empty();
                  if let Ok(mut map) = CACHE.write() {
                     cache(&mut map, mid, CacheEntry::Ready(items));
                  }
                  any_visible |= non_empty;
               }
               if any_visible {
                  invalidate_discord_windows();
               }
            }
         });
      tx
   })
}

fn lookup_cached(message_id: u64) -> Option<Vec<RenderedAccessory>> {
   if let Ok(map) = CACHE.read()
      && let Some(entry) = map.get(&message_id)
   {
      return match entry {
         CacheEntry::Pending => None,
         CacheEntry::Ready(items) if items.is_empty() => None,
         CacheEntry::Ready(items) => Some(items.clone()),
      };
   }
   let should_queue = CACHE.write().is_ok_and(|mut map| {
      if map.contains_key(&message_id) {
         return false;
      }
      cache(&mut map, message_id, CacheEntry::Pending);
      true
   });
   if should_queue {
      let _ = ensure_worker().send(message_id);
   }
   None
}

/// Fast read-only tint lookup for `build_body_detour` (called inside
/// `MessageRow::render`). Cache misses are queued right afterward by
/// `render_row_detour`, which triggers a repaint.
fn lookup_cached_tint(message_id: u64) -> Option<u32> {
   match CACHE.read().ok()?.get(&message_id)? {
      CacheEntry::Ready(items) => items.iter().rev().find_map(|item| item.tint),
      CacheEntry::Pending => None,
   }
}

/// Converts plugin ARGB (`0xAARRGGBB`, with `0x00RRGGBB` treated as opaque)
/// into Discord GPUI's packed `0xRRGGBBAA` `u32` token format.
pub const fn argb_to_rgba(argb: u32) -> u32 {
   let raw_a = (argb >> 24) & 0xFF;
   let a = if raw_a == 0 && argb != 0 { 0xFF } else { raw_a };
   ((argb & 0x00FF_FFFF) << 8) | a
}

/// Requests an immediate GPUI re-render on all visible Discord windows so
/// updated message accessories appear right away.
pub fn invalidate_discord_windows() {
   crate::modal::force_redraw();
}

/// Locates and hooks Discord's GPUI `MessageRow::render` from `DllMain`.
pub fn install(image: &Image<'static>) {
   if !gpui::supports_accessories() {
      log::warn!("desktop accessories: GPUI text/push primitives unavailable, skipping");
      return;
   }

   if !crate::X86_CODE {
      #[cfg(target_os = "macos")]
      arm64::install(image);
      return;
   }

   let (render_row, offset) = match desktop::message_row(image) {
      Ok(row) => (row.func, row.msg_id_offset),
      Err(err) => {
         log::warn!("desktop accessories: {err}, skipping");
         return;
      },
   };
   MSG_ID_OFFSET.store(offset, Ordering::Release);

   // SAFETY: runs from `prepare` before Discord threads execute;
   // `render_row_detour` matches `MessageRow::render`'s exact 11-argument sret
   // ABI.
   let Some(orig) = (unsafe {
      crate::hook::<RenderRowFn>(
         image,
         |pro| pro.row,
         render_row,
         render_row_detour as *const () as usize,
         "desktop accessories: MessageRow hook failed",
      )
   }) else {
      return;
   };
   let _ = ORIG_RENDER_ROW.set(orig);
   log::info!(
      "desktop accessories: hooked MessageRow render @ {render_row:#x} (msg_id +{offset:#x})"
   );

   install_body_tint(image, render_row);
}

/// Hooks Discord's inner message body builder (`build_message_body`, called
/// once per row from `MessageRow::render`) so message tints also recolor the
/// rendered message body text natively via `config.row_colors` (`0xRRGGBBAA`).
fn install_body_tint(image: &Image<'static>, render_row: usize) {
   let body = match desktop::message_body(image, render_row) {
      Ok(body) => body,
      Err(err) => {
         log::warn!("desktop accessories: {err}, skipping body tint");
         return;
      },
   };
   let (body_fn, msg_id_off, colors_off) = (body.func, body.msg_id_offset, body.row_colors_offset);
   BODY_MSG_ID_OFFSET.store(msg_id_off, Ordering::Release);
   ROW_COLORS_OFFSET.store(colors_off, Ordering::Release);

   // SAFETY: runs from `prepare` before Discord threads execute;
   // `build_body_detour` matches `build_message_body`'s verified 6-argument
   // ABI.
   let Some(orig) = (unsafe {
      crate::hook::<BuildMessageBodyFn>(
         image,
         |pro| pro.row,
         body_fn,
         build_body_detour as *const () as usize,
         "desktop accessories: message body tint hook failed",
      )
   }) else {
      return;
   };
   let _ = ORIG_BUILD_BODY.set(orig);
   log::info!(
      "desktop accessories: hooked message body tint @ {body_fn:#x} (msg_id +{msg_id_off:#x}, \
       row_colors +{colors_off:#x})"
   );
}

unsafe extern "C" fn build_body_detour(
   msg: *const c_void,
   config: *mut c_void,
   a2: u64,
   a3: u64,
   s0: u64,
   s1: u64,
) -> *mut c_void {
   let Some(orig) = ORIG_BUILD_BODY.get().copied() else {
      return ptr::null_mut();
   };
   if ACTIVE_COUNT.load(Ordering::Relaxed) == 0 || msg.is_null() || config.is_null() {
      // SAFETY: forwards all arguments verbatim to the original body builder.
      return unsafe { orig(msg, config, a2, a3, s0, s1) };
   }

   let msg_off = BODY_MSG_ID_OFFSET.load(Ordering::Relaxed);
   // SAFETY: `msg` is Discord's live `&MessageViewData` and `msg_off` was
   // decoded from `build_message_body`'s own `message_id` read.
   let message_id = unsafe { ptr::read_unaligned(msg.cast::<u8>().add(msg_off).cast::<u64>()) };
   let Some(argb) = (message_id != 0)
      .then(|| lookup_cached_tint(message_id))
      .flatten()
   else {
      // SAFETY: no tint for this message; forward verbatim.
      return unsafe { orig(msg, config, a2, a3, s0, s1) };
   };

   let rgba = argb_to_rgba(argb);
   let base_off = ROW_COLORS_OFFSET.load(Ordering::Relaxed);
   // SAFETY: `config` lives on `MessageRow::render`'s thread-local stack frame;
   // we temporarily override the two POD `u32` `0xRRGGBBAA` body text color
   // slots (`row_colors + 0x00` default and `+ 0x34` pending/muted) and restore
   // them on return.
   unsafe {
      let default_ptr = config.cast::<u8>().add(base_off);
      let muted_ptr = config.cast::<u8>().add(base_off + ROW_COLORS_MUTED_DELTA);
      let saved_default = default_ptr.cast::<u32>().read_unaligned();
      let saved_muted = muted_ptr.cast::<u32>().read_unaligned();
      default_ptr.cast::<u32>().write_unaligned(rgba);
      muted_ptr.cast::<u32>().write_unaligned(rgba);
      let out = orig(msg, config, a2, a3, s0, s1);
      default_ptr.cast::<u32>().write_unaligned(saved_default);
      muted_ptr.cast::<u32>().write_unaligned(saved_muted);
      out
   }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn render_row_detour(
   sret: *mut RawAnyElement,
   row: *const c_void,
   a2: usize,
   a3: usize,
   s0: u64,
   s1: u64,
   s2: u64,
   s3: u64,
   s4: u64,
   s5: u64,
   s6: u64,
) -> *mut RawAnyElement {
   let Some(orig) = ORIG_RENDER_ROW.get().copied() else {
      return sret;
   };
   // SAFETY: forwards all arguments verbatim to the original
   // `MessageRow::render`.
   let out = unsafe { orig(sret, row, a2, a3, s0, s1, s2, s3, s4, s5, s6) };

   if row.is_null() || sret.is_null() {
      return out;
   }

   let offset = MSG_ID_OFFSET.load(Ordering::Relaxed);
   // SAFETY: `row` is Discord's live `&MessageRow` and `offset` was decoded
   // from its `message-content-{id}` formatting read.
   let message_id = unsafe { row.cast::<u8>().add(offset).cast::<u64>().read_unaligned() };
   decorate(sret, message_id);
   out
}

/// Wraps the row `sret` rendered for `message_id` with the accessories
/// plugins returned for it.
fn decorate(sret: *mut RawAnyElement, message_id: u64) {
   if message_id == 0 {
      return;
   }
   double_click::wrap_row(sret, message_id);
   if ACTIVE_COUNT.load(Ordering::Relaxed) == 0 {
      return;
   }

   let Some(items) = lookup_cached(message_id) else {
      return;
   };

   let tint = items.iter().rev().find_map(|item| item.tint);
   let lines = items
      .into_iter()
      .filter(|item| !item.text.is_empty())
      .map(|item| (item.text, item.color))
      .collect::<Vec<_>>();
   if lines.is_empty() && tint.is_none() {
      return;
   }

   let _ = std::panic::catch_unwind(|| {
      gpui::wrap_with_accessories(sret, &lines, tint);
   });
}

/// Locates and hooks Desktop Phoenix's same-client optimistic message delete
/// (`Store::delete_message`), so deleting a message from this client respects
/// `MESSAGE_DELETE` `before` hooks (such as `message-logger`).
pub fn install_optimistic_delete(image: &Image<'static>) {
   let Some(del_fn) = phoenix_runtime::gateway::original_handler("MESSAGE_DELETE") else {
      log::warn!(
         "desktop accessories: MESSAGE_DELETE handler not found, skipping optimistic delete hook"
      );
      return;
   };
   let found = if crate::X86_CODE {
      desktop::optimistic_delete(image, del_fn)
   } else {
      phoenix_sigs::macos::optimistic_delete(image, del_fn)
   };
   let opt_del = match found {
      Ok(opt_del) => opt_del,
      Err(err) => {
         log::warn!("desktop accessories: {err}, skipping optimistic delete hook");
         return;
      },
   };

   // SAFETY: runs from `prepare` before Discord threads execute;
   // `optimistic_delete_detour` matches `Store::delete_message`'s exact
   // `(store, args, meta) -> bool` ABI.
   let Some(orig) = (unsafe {
      crate::hook::<OptimisticDeleteFn>(
         image,
         |pro| pro.frame,
         opt_del,
         optimistic_delete_detour as *const () as usize,
         "desktop accessories: optimistic delete hook failed",
      )
   }) else {
      return;
   };
   let _ = ORIG_OPT_DELETE.set(orig);
   log::info!("desktop accessories: hooked same-client optimistic delete @ {opt_del:#x}");
}

unsafe extern "C" fn optimistic_delete_detour(
   store: *mut c_void,
   args: *const *const u64,
   meta: *const c_void,
) -> u8 {
   if !args.is_null() {
      // SAFETY: `args` points to `[&channel_id, &message_id]` in
      // `Command::DeleteMessage`.
      let (ch_ptr, msg_ptr) = unsafe { (ptr::read(args), ptr::read(args.add(1))) };
      if !ch_ptr.is_null() && !msg_ptr.is_null() {
         let (channel_id, message_id) = unsafe { (ptr::read(ch_ptr), ptr::read(msg_ptr)) };
         if channel_id != 0
            && message_id != 0
            && std::panic::catch_unwind(|| {
               phoenix_runtime::gateway::check_optimistic_delete(channel_id, message_id)
            })
            .unwrap_or(false)
         {
            return 0;
         }
      }
   }
   let Some(orig) = ORIG_OPT_DELETE.get().copied() else {
      return 0;
   };
   // SAFETY: forwards all arguments verbatim to the original optimistic delete.
   unsafe { orig(store, args, meta) }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn argb_to_rgba_preserves_channels_and_alpha() {
      assert_eq!(argb_to_rgba(0xFFF2_3F43), 0xF23F_43FF);
      assert_eq!(argb_to_rgba(0x8058_65F2), 0x5865_F280);
      assert_eq!(argb_to_rgba(0x00F2_3F43), 0xF23F_43FF);
      assert_eq!(argb_to_rgba(0), 0);
   }
}

/// `MessageRow::render` on macOS, arm64 ([`phoenix_sigs::macos::message_row`]).
///
/// It takes `x0` the row, `x1`..`x7`, `d0` and one stack word, and returns
/// the 0x20-byte `AnyElement` through `x8`, which a Rust `extern "C"` function
/// returning [`RawAnyElement`] does too, so the detour is native. Extra stack
/// words are read from the caller's frame and passed back unchanged.
#[cfg(target_os = "macos")]
mod arm64 {
   use std::sync::{OnceLock, atomic::Ordering};

   use phoenix_hook::image::Image;

   use super::{ACTIVE_COUNT, RawAnyElement, decorate};

   type RenderFn = unsafe extern "C" fn(
      row: *const u8,
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
      s0: usize,
      s1: usize,
      s2: usize,
      s3: usize,
   ) -> RawAnyElement;

   static ORIG: OnceLock<RenderFn> = OnceLock::new();
   static ROW: OnceLock<phoenix_sigs::macos::MessageRow> = OnceLock::new();

   pub fn install(image: &Image<'static>) {
      let row = match phoenix_sigs::macos::message_row(image) {
         Ok(row) => row,
         Err(err) => {
            log::warn!("desktop accessories: {err}, skipping");
            return;
         },
      };
      // SAFETY: runs from `prepare` before Discord's threads; `detour` has the
      // render's arm64 signature.
      let Some(orig) = (unsafe {
         crate::hook::<RenderFn>(
            image,
            |pro| pro.frame,
            row.func,
            detour as *const () as usize,
            "desktop accessories: MessageRow hook failed",
         )
      }) else {
         return;
      };
      let _ = ORIG.set(orig);
      let _ = ROW.set(row);
      log::info!("desktop accessories: hooked MessageRow render {row:x?}");
      install_body_tint(image, row.func);
   }

   type BodyFn = unsafe extern "C" fn(*const u8, *mut u8, usize, usize, usize, usize) -> Returned;

   /// What the builder leaves in `x0` and `x1`, passed back as is.
   #[repr(C)]
   struct Returned(usize, usize);

   static BODY: OnceLock<(BodyFn, phoenix_sigs::macos::MessageBody)> = OnceLock::new();

   fn install_body_tint(image: &Image<'static>, render_row: usize) {
      let body = match phoenix_sigs::macos::message_body(image, render_row) {
         Ok(body) => body,
         Err(err) => {
            log::warn!("desktop accessories: {err}, skipping body tint");
            return;
         },
      };
      // SAFETY: runs from `prepare` before Discord's threads; `body_detour` has
      // the builder's arm64 signature.
      let Some(orig) = (unsafe {
         crate::hook::<BodyFn>(
            image,
            |pro| pro.frame,
            body.func,
            body_detour as *const () as usize,
            "desktop accessories: message body tint hook failed",
         )
      }) else {
         return;
      };
      log::info!("desktop accessories: hooked message body tint {body:x?}");
      let _ = BODY.set((orig, body));
   }

   /// Points every text color the body may pick at the tint for this call.
   unsafe extern "C" fn body_detour(
      view: *const u8,
      config: *mut u8,
      a2: usize,
      a3: usize,
      a4: usize,
      a5: usize,
   ) -> Returned {
      let (orig, body) = BODY.get().expect("the detour only runs once BODY is set");
      let tint = (ACTIVE_COUNT.load(Ordering::Relaxed) > 0 && !view.is_null() && !config.is_null())
         // SAFETY: `view` is the live message view the builder reads the id from.
         .then(|| unsafe { view.add(body.message_id).cast::<u64>().read_unaligned() })
         .filter(|&id| id != 0)
         .and_then(super::lookup_cached_tint);
      let Some(argb) = tint else {
         // SAFETY: forwards every argument to the original builder.
         return unsafe { orig(view, config, a2, a3, a4, a5) };
      };
      let rgba = super::argb_to_rgba(argb);
      // SAFETY: `config` is the render's stack frame for this call, and these
      // `u32` colors are restored before it returns.
      unsafe {
         let saved = body
            .colors
            .iter()
            .map(|&at| config.add(at).cast::<u32>().read_unaligned())
            .collect::<Vec<_>>();
         for &at in &body.colors {
            config.add(at).cast::<u32>().write_unaligned(rgba);
         }
         let out = orig(view, config, a2, a3, a4, a5);
         for (&at, color) in body.colors.iter().zip(saved) {
            config.add(at).cast::<u32>().write_unaligned(color);
         }
         out
      }
   }

   /// The message id the row renders: `row.rc -> Rc<Message> -> id`.
   ///
   /// # Safety
   ///
   /// `row` is the live row the render was called with.
   unsafe fn message_id(row: *const u8) -> u64 {
      let Some(at) = ROW.get() else {
         return 0;
      };
      // SAFETY: the render loads the `Rc` from `row + rc` and hands
      // `rc + value` to the helper that formats `id`.
      unsafe {
         let rc = row.add(at.rc).cast::<*const u8>().read_unaligned();
         if rc.is_null() {
            return 0;
         }
         rc.add(at.value + at.message_id)
            .cast::<u64>()
            .read_unaligned()
      }
   }

   unsafe extern "C" fn detour(
      row: *const u8,
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
      s0: usize,
      s1: usize,
      s2: usize,
      s3: usize,
   ) -> RawAnyElement {
      let orig = *ORIG.get().expect("the detour only runs once ORIG is set");
      // SAFETY: forwards every register and the stack words to the render.
      let mut out = unsafe {
         orig(
            row, a1, a2, a3, a4, a5, a6, a7, f0, f1, f2, f3, s0, s1, s2, s3,
         )
      };
      if ACTIVE_COUNT.load(Ordering::Relaxed) > 0 && !row.is_null() {
         // SAFETY: `row` is the render's live row.
         let id = unsafe { message_id(row) };
         if (1 << 32..1 << 63).contains(&id) {
            decorate(&raw mut out, id);
         }
      }
      out
   }
}
