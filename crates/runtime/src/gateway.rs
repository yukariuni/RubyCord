use std::{
   cell::Cell,
   ffi::c_void,
   panic::{
      self,
      AssertUnwindSafe,
   },
   slice,
   str,
   sync::{
      LazyLock,
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         AtomicU64,
         AtomicUsize,
         Ordering,
      },
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::{
   GATEWAY_HANDLER_FIELD,
   GatewayEvent,
   GatewayHandler,
   HandlerResult,
   REST_WORDS,
   RawStr,
   registry::{
      EventId,
      GatewayRegistry,
   },
};

use crate::{
   Error,
   STATE,
   State,
   js::Before,
};

/// The registry's original handlers, in [`GatewayRegistry::iter`] order, and
/// which events a plugin listens to.
struct Gateway {
   registry:  GatewayRegistry,
   originals: Vec<GatewayHandler>,
   hooked:    Vec<AtomicBool>,
}

static GATEWAY: OnceLock<Gateway> = OnceLock::new();
/// Events plugins fed Phoenix from outside a hook. They run on the store
/// thread ahead of the next event Phoenix handles, since only a handler call
/// carries the store.
static QUEUE: Mutex<Vec<(EventId, String)>> = Mutex::new(Vec::new());
static QUEUED: AtomicBool = AtomicBool::new(false);

// identify (2) and resume (6) carry session creds, never expose them to plugins
pub const BLOCKED_SEND_OPS: [u32; 2] = [2, 6];

#[derive(Debug, PartialEq, Eq)]
pub enum SendAction {
   Pass,
   Drop,
   Replace(String),
}

struct SendHook {
   id:     u64,
   plugin: String,
   op:     u32,
}

static SEND_HOOKS: LazyLock<Mutex<Vec<SendHook>>> = LazyLock::new(Mutex::default);
static SEND_HOOK_COUNT: AtomicUsize = AtomicUsize::new(0);
static NEXT_SEND_HOOK_ID: AtomicU64 = AtomicU64::new(1);

#[must_use]
pub fn register_send_hook(plugin: &str, op: u32) -> u64 {
   let id = NEXT_SEND_HOOK_ID.fetch_add(1, Ordering::Relaxed);
   let mut list = SEND_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   list.push(SendHook {
      id,
      plugin: plugin.to_owned(),
      op,
   });
   SEND_HOOK_COUNT.store(list.len(), Ordering::Release);
   id
}

pub fn unregister_send_hook(id: u64) {
   let mut list = SEND_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   list.retain(|hook| hook.id != id);
   SEND_HOOK_COUNT.store(list.len(), Ordering::Release);
}

pub fn forget_send_hooks(plugin: &str) {
   let mut list = SEND_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   list.retain(|hook| hook.plugin != plugin);
   SEND_HOOK_COUNT.store(list.len(), Ordering::Release);
}

#[must_use]
pub(crate) fn matching_send_hooks(op: u32) -> Vec<u64> {
   if BLOCKED_SEND_OPS.contains(&op) || SEND_HOOK_COUNT.load(Ordering::Acquire) == 0 {
      return Vec::new();
   }
   let list = SEND_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   list
      .iter()
      .filter(|hook| hook.op == op)
      .map(|hook| hook.id)
      .collect()
}

#[derive(serde::Deserialize)]
struct FrameOp {
   op: u32,
}

#[must_use]
pub fn observe_send(frame_json: &str) -> SendAction {
   if SEND_HOOK_COUNT.load(Ordering::Acquire) == 0 || IN_JS.get() {
      return SendAction::Pass;
   }
   let Ok(FrameOp { op }) = serde_json::from_str::<FrameOp>(frame_json) else {
      return SendAction::Pass;
   };
   let matched = matching_send_hooks(op);
   if matched.is_empty() {
      return SendAction::Pass;
   }
   let Some(state) = STATE.get() else {
      return SendAction::Pass;
   };
   run_js(state, |engine| {
      Ok(engine.intercept_gateway_send(&matched, op, frame_json))
   })
   .unwrap_or(SendAction::Pass)
}

thread_local! {
   #[allow(
      clippy::missing_const_for_thread_local,
      reason = "fires on Android only, where the initializer is already const"
   )]
   static IN_JS: Cell<bool> = const { Cell::new(false) };
   #[allow(
      clippy::missing_const_for_thread_local,
      reason = "fires on Android only, where the initializer is already const"
   )]
   static HANDLING: Cell<Option<Handling>> = const { Cell::new(None) };
}

/// The host's handler arguments for the event whose hooks are running, so a
/// hook can feed Phoenix another event on the same store thread.
#[derive(Clone, Copy)]
struct Handling {
   rest:  [u64; REST_WORDS],
   store: *mut c_void,
   ctx:   *mut c_void,
}

/// Points every registry entry at [`detour`].
///
/// Phoenix looks handlers up in the registry on each dispatch, so a pointer
/// swap catches every event without patching code, and events no plugin
/// listens to cost one atomic load.
pub fn swap(
   image: &Image<'static>,
   registry: GatewayRegistry,
) -> Result<&'static GatewayRegistry, Error> {
   let writes = registry
      .iter()
      .flat_map(|(_, entry)| {
         entry
            .slots
            .iter()
            .map(|&slot| (slot + GATEWAY_HANDLER_FIELD, detour as *const () as usize))
      })
      .collect::<Vec<(usize, usize)>>();
   let originals = registry
      .iter()
      // SAFETY: the registry walk only accepts entries whose handler starts a
      // function, and every registry handler has this ABI.
      .map(|(_, entry)| unsafe { std::mem::transmute::<usize, GatewayHandler>(entry.handler) })
      .collect();
   let hooked = registry.iter().map(|_| AtomicBool::new(false)).collect();

   GATEWAY
      .set(Gateway {
         registry,
         originals,
         hooked,
      })
      .map_err(|_| Error::AlreadyStarted)?;
   let gateway = GATEWAY.get().expect("GATEWAY was set above");

   // SAFETY: the slots are the handler fields of registry entries in `image`,
   // and the host has not started dispatching while its constructors are
   // pending.
   let previous = unsafe { image.swap_words(&writes) }?;
   let expected = gateway
      .registry
      .iter()
      .flat_map(|(_, entry)| entry.slots.iter().map(move |_| entry));
   for (entry, found) in expected.zip(previous) {
      if found != entry.handler {
         return Err(Error::SwapConflict {
            event: entry.name.clone(),
            expected: entry.handler,
            found,
         });
      }
   }
   Ok(&gateway.registry)
}

/// Runs `data` through Phoenix's own handler for `event`, skipping the
/// plugins. Returns whether the handler accepted it.
pub fn dispatch(event: EventId, data: &str) -> Option<bool> {
   let gateway = GATEWAY
      .get()
      .expect("start swaps the registry before plugins load");
   let handling = HANDLING.get()?;
   let name = &gateway.registry.entry(event).name;
   let synthetic = GatewayEvent {
      name: RawStr {
         ptr: name.as_ptr(),
         len: name.len(),
      },
      data: RawStr {
         ptr: data.as_ptr(),
         len: data.len(),
      },
      rest: handling.rest,
   };

   // SAFETY: `handling` holds the arguments of a handler call still on this
   // thread's stack, and the event borrows strings that outlive the call.
   let original = gateway.originals[event.index()];
   let result = unsafe { original(handling.store, &raw const synthetic, handling.ctx) };
   Some(result.is_ok())
}

/// The live registry entry named `name`, like `MESSAGE_CREATE`.
pub fn event(name: &str) -> Option<EventId> {
   GATEWAY.get()?.registry.id(name)
}

/// The host's original handler address for gateway event `name`.
pub fn original_handler(name: &str) -> Option<usize> {
   let gateway = GATEWAY.get()?;
   let id = gateway.registry.id(name)?;
   Some(gateway.registry.entry(id).handler)
}

/// Runs `MESSAGE_DELETE`'s JS `before` hooks for a same-client optimistic
/// delete. Returns `true` when a plugin requested dropping the deletion.
pub fn check_optimistic_delete(channel_id: u64, message_id: u64) -> bool {
   let Some(gateway) = GATEWAY.get() else {
      return false;
   };
   let Some(id) = gateway.registry.id("MESSAGE_DELETE") else {
      return false;
   };
   if !gateway.hooked[id.index()].load(Ordering::Acquire) || IN_JS.get() {
      return false;
   }
   let Some(state) = STATE.get() else {
      return false;
   };
   let json = format!("{{\"id\":\"{message_id}\",\"channel_id\":\"{channel_id}\"}}");
   matches!(
      run_js(state, |engine| engine.before(id, "MESSAGE_DELETE", &json)),
      Some(Before::Drop)
   )
}

/// Runs plugin hooks on `data` for an event Phoenix's session applies without
/// the registry, like `READY`, and returns the replacement JSON.
///
/// Its `after` hooks run before Phoenix applies it, and it cannot be dropped.
pub fn intercept(event: EventId, data: &str) -> Option<String> {
   let gateway = GATEWAY.get()?;
   if !gateway.hooked[event.index()].load(Ordering::Acquire) || IN_JS.get() {
      return None;
   }
   let state = STATE.get()?;
   let name = &gateway.registry.entry(event).name;
   let replaced = match run_js(state, |engine| engine.before(event, name, data))? {
      Before::Keep => None,
      Before::Drop => {
         log::warn!("gateway: {name} cannot be dropped, it went through");
         None
      },
      Before::Replace(json) => Some(json),
   };
   let applied = replaced.as_deref().unwrap_or(data);
   run_js(state, |engine| engine.after(event, name, applied, true));
   replaced
}

/// Whether a plugin hooks `event`.
pub fn listened(event: EventId) -> bool {
   GATEWAY
      .get()
      .is_some_and(|gateway| gateway.hooked[event.index()].load(Ordering::Acquire))
}

/// Runs `data` through Phoenix's own handler for `event` ahead of the next
/// event Phoenix handles.
pub fn enqueue(event: EventId, data: String) {
   QUEUE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push((event, data));
   QUEUED.store(true, Ordering::Release);
}

fn flush(gateway: &Gateway, store: *mut c_void, ctx: *mut c_void, rest: [u64; REST_WORDS]) {
   let queued = std::mem::take(&mut *QUEUE.lock().unwrap_or_else(PoisonError::into_inner));
   for (event, data) in queued {
      let name = &gateway.registry.entry(event).name;
      let synthetic = GatewayEvent {
         name: RawStr {
            ptr: name.as_ptr(),
            len: name.len(),
         },
         data: RawStr {
            ptr: data.as_ptr(),
            len: data.len(),
         },
         rest,
      };
      // SAFETY: `store` and `ctx` are the arguments of the handler call this
      // runs inside, and the event borrows strings that outlive the call.
      let original = gateway.originals[event.index()];
      if unsafe { original(store, &raw const synthetic, ctx) }.is_ok() {
         continue;
      }
      log::warn!("Phoenix rejected a queued {name}");
   }
}

/// Routes `event` through the plugins.
pub fn install(event: EventId) {
   let gateway = GATEWAY
      .get()
      .expect("start swaps the registry before plugins load");
   if !gateway.hooked[event.index()].swap(true, Ordering::AcqRel) {
      log::info!("hooked {}", gateway.registry.entry(event).name);
   }
}

/// Sends `event` straight to Phoenix again once no plugin listens to it.
pub fn uninstall(event: EventId) {
   let gateway = GATEWAY
      .get()
      .expect("start swaps the registry before plugins load");
   if gateway.hooked[event.index()].swap(false, Ordering::AcqRel) {
      log::info!("unhooked {}", gateway.registry.entry(event).name);
   }
}

unsafe extern "C" fn detour(
   store: *mut c_void,
   event: *const GatewayEvent,
   ctx: *mut c_void,
) -> HandlerResult {
   let gateway = GATEWAY
      .get()
      .expect("slots point here only after GATEWAY is set");
   // SAFETY: the host passes a live event for the duration of the call.
   let event = unsafe { &*event };
   if let Some(platform) = crate::platform() {
      platform.gateway(store);
   }
   if QUEUED.swap(false, Ordering::AcqRel) {
      flush(gateway, store, ctx, event.rest);
   }
   // SAFETY: name and data are borrowed strings that outlive the call.
   let (name, data) = unsafe { (text(event.name), text(event.data)) };
   let Some((id, name)) = name.and_then(|name| Some((gateway.registry.id(name)?, name))) else {
      log::error!("gateway event without a registry entry, dropped");
      return HandlerResult::OK;
   };
   crate::remote::record(&crate::remote::Trace::Gateway {
      event: name,
      bytes: data.map_or(0, str::len),
   });
   let original = gateway.originals[id.index()];
   // SAFETY: forwarded to the registry's own handler unchanged or with
   // replaced data.
   let call = |event: &GatewayEvent| unsafe { original(store, event, ctx) };

   let listened = gateway.hooked[id.index()].load(Ordering::Acquire);
   let (true, Some(state), Some(data), false) = (listened, STATE.get(), data, IN_JS.get()) else {
      return call(event);
   };
   HANDLING.set(Some(Handling {
      store,
      ctx,
      rest: event.rest,
   }));
   let original_data = data;
   let before = run_js(state, |engine| engine.before(id, name, data)).unwrap_or(Before::Keep);
   let (result, data) = match &before {
      Before::Drop => {
         HANDLING.set(None);
         return HandlerResult::OK;
      },
      Before::Keep => (call(event), data),
      Before::Replace(json) => {
         let replaced = GatewayEvent {
            data: RawStr {
               ptr: json.as_ptr(),
               len: json.len(),
            },
            ..*event
         };
         (call(&replaced), json.as_str())
      },
   };
   if let (Before::Replace(json), "MESSAGE_CREATE" | "MESSAGE_UPDATE") = (&before, name) {
      remember_original(original_data, json);
   }
   run_js(state, |engine| engine.after(id, name, data, result.is_ok()));
   HANDLING.set(None);
   result
}

#[derive(serde::Deserialize)]
struct Posted<'a> {
   #[serde(borrow)]
   content: Option<std::borrow::Cow<'a, str>>,
}

/// Tells the platform a message's text from before and after the plugin
/// hooks, when they changed it. Text a hook emptied is left out: the edit box
/// is empty whenever nothing is typed, so restoring from `""` would refill it
/// after every send.
fn remember_original(before: &str, after: &str) {
   let (Ok(before), Ok(after)) = (
      serde_json::from_str::<Posted<'_>>(before),
      serde_json::from_str::<Posted<'_>>(after),
   ) else {
      return;
   };
   if let (Some(original), Some(shown)) = (before.content, after.content)
      && original != shown
      && !shown.trim().is_empty()
      && let Some(platform) = crate::platform()
   {
      platform.rewritten(original.into_owned(), shown.into_owned());
   }
}

/// # Safety
///
/// `raw` describes live bytes.
unsafe fn text<'a>(raw: RawStr) -> Option<&'a str> {
   if raw.ptr.is_null() {
      return None;
   }
   // SAFETY: forwarded from the caller.
   str::from_utf8(unsafe { slice::from_raw_parts(raw.ptr, raw.len) }).ok()
}

/// Runs `work` on the engine, logging JS errors and panics instead of unwinding
/// into the host. Nested gateway calls made while JS runs go straight to the
/// original.
fn run_js<T>(
   state: &State,
   work: impl FnOnce(&mut crate::js::Engine) -> Result<T, Error>,
) -> Option<T> {
   IN_JS.set(true);
   let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
      let mut engine = state
         .js
         .lock()
         .unwrap_or_else(std::sync::PoisonError::into_inner);
      work(&mut engine)
   }));
   IN_JS.set(false);
   match outcome {
      Ok(Ok(value)) => Some(value),
      Ok(Err(err)) => {
         log::error!("{err}");
         None
      },
      Err(_) => {
         log::error!("a gateway hook panicked, the event went through unmodified");
         None
      },
   }
}
