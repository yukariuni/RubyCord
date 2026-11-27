//! Phoenix stand-in.
//!
//! Mirrors the parts of `liblibdiscord_wasm_host.so` that phoenix touches. The
//! gateway registry is a static array of `{name ptr, name len, handler}` in
//! `.data.rel.ro`, and every handler is `extern "C" fn(store, &Event, ctx) ->
//! Result`, where the 32-byte result travels through the hidden return pointer.
//! Handlers are not exported, so after `strip` the only way to them is a
//! signature. `stand_in_gateway_receive` plays the websocket, taking one
//! gateway frame `{"t": EVENT, "d": {...}}` and dispatching it.

use std::{
   ffi::c_void,
   hint::black_box,
   slice,
   str,
   sync::Mutex,
};

use phoenix_sigs::{
   HandlerResult,
   REST_WORDS,
};
use serde::Deserialize;
use serde_json::value::RawValue;

#[repr(C)]
#[derive(Clone, Copy)]
struct Str {
   ptr: *const u8,
   len: usize,
}

impl Str {
   const fn new(text: &'static str) -> Self {
      Self {
         ptr: text.as_ptr(),
         len: text.len(),
      }
   }

   const fn borrowed(text: &str) -> Self {
      Self {
         ptr: text.as_ptr(),
         len: text.len(),
      }
   }

   /// # Safety
   ///
   /// `ptr` and `len` describe live UTF-8.
   unsafe fn as_str<'a>(self) -> Option<&'a str> {
      // SAFETY: forwarded from the caller.
      str::from_utf8(unsafe { slice::from_raw_parts(self.ptr, self.len) }).ok()
   }
}

#[repr(C)]
struct Event {
   name: Str,
   data: Str,
   rest: [u64; REST_WORDS],
}

/// The result shape the loader expects on this target.
const fn bad_payload() -> HandlerResult {
   let mut result = HandlerResult::OK;
   result.0[0] = 0;
   result.0[1] = 1;
   result
}

type Handler = extern "C" fn(*mut Store, *const Event, *mut c_void) -> HandlerResult;

#[repr(C)]
struct Entry {
   name:    Str,
   handler: Handler,
}

struct Registry([Entry; 4]);

// SAFETY: every pointer in the table names immutable static data or code.
unsafe impl Sync for Registry {}

static REGISTRY: Registry = Registry([
   Entry {
      name:    Str::new("MESSAGE_CREATE"),
      handler: message_create,
   },
   Entry {
      name:    Str::new("MESSAGE_UPDATE"),
      handler: message_update,
   },
   Entry {
      name:    Str::new("MESSAGE_DELETE"),
      handler: message_delete,
   },
   Entry {
      name:    Str::new("MESSAGE_DELETE_BULK"),
      handler: message_delete,
   },
]);

#[derive(Deserialize)]
struct Author {
   username: String,
}

#[derive(Deserialize)]
struct Message {
   id:         String,
   channel_id: String,
   content:    String,
   author:     Author,
}

#[derive(Deserialize)]
struct Deleted {
   id: String,
}

struct Store {
   messages: Vec<Message>,
}

static STORE: Mutex<Store> = Mutex::new(Store {
   messages: Vec::new(),
});

/// # Safety
///
/// `event` points at a live `Event`.
unsafe fn payload<'a, T: Deserialize<'a>>(event: *const Event) -> Option<T> {
   // SAFETY: forwarded from the caller, the data outlives the handler call.
   let data = unsafe { (*event).data.as_str()? };
   serde_json::from_str(data).ok()
}

extern "C" fn message_create(
   store: *mut Store,
   event: *const Event,
   _ctx: *mut c_void,
) -> HandlerResult {
   // SAFETY: dispatch passes a live event.
   let Some(message) = (unsafe { payload::<Message>(event) }) else {
      return bad_payload();
   };
   println!(
      "[stand-in] #{} <{}> {}",
      message.channel_id, message.author.username, message.content
   );
   // SAFETY: dispatch passes the locked store.
   unsafe { &mut *store }.messages.push(message);
   HandlerResult::OK
}

extern "C" fn message_update(
   store: *mut Store,
   event: *const Event,
   _ctx: *mut c_void,
) -> HandlerResult {
   // SAFETY: dispatch passes a live event.
   let Some(update) = (unsafe { payload::<Message>(event) }) else {
      return bad_payload();
   };
   // SAFETY: dispatch passes the locked store.
   let store = unsafe { &mut *store };
   if let Some(message) = store
      .messages
      .iter_mut()
      .find(|message| message.id == update.id)
   {
      println!("[stand-in] edited {}: {}", update.id, update.content);
      *message = update;
   }
   HandlerResult::OK
}

extern "C" fn message_delete(
   store: *mut Store,
   event: *const Event,
   _ctx: *mut c_void,
) -> HandlerResult {
   // SAFETY: dispatch passes a live event.
   let Some(deleted) = (unsafe { payload::<Deleted>(event) }) else {
      return bad_payload();
   };
   // SAFETY: dispatch passes the locked store.
   unsafe { &mut *store }
      .messages
      .retain(|message| message.id != deleted.id);
   println!("[stand-in] deleted {}", deleted.id);
   HandlerResult::OK
}

#[derive(Deserialize)]
struct Frame<'a> {
   t: &'a str,
   #[serde(borrow)]
   d: &'a RawValue,
}

/// Dispatches one gateway frame. Returns false for malformed frames, unknown
/// events and handler errors.
///
/// # Safety
///
/// `frame` points at `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stand_in_gateway_receive(frame: *const u8, len: usize) -> bool {
   // SAFETY: forwarded from the caller.
   let bytes = unsafe { slice::from_raw_parts(frame, len) };
   let Ok(frame) = serde_json::from_slice::<Frame<'_>>(bytes) else {
      return false;
   };

   // black_box keeps LTO from folding the table into a switch with the handlers
   // inlined, which would leave nothing for the registry anchor to find.
   let registry = black_box(&REGISTRY);
   let Some(entry) = registry.0.iter().find(|entry| {
      // SAFETY: registry names are static strings.
      (unsafe { entry.name.as_str() }) == Some(frame.t)
   }) else {
      return false;
   };

   let event = Event {
      name: Str::borrowed(frame.t),
      data: Str::borrowed(frame.d.get()),
      rest: [0; REST_WORDS],
   };
   let mut store = STORE
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
   let result = (entry.handler)(&raw mut *store, &raw const event, std::ptr::null_mut());
   drop(store);
   result.is_ok()
}
