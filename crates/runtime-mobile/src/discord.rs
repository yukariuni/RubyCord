//! Discord's HTTP API through Phoenix's own `Networking` host module, so a
//! plugin's request leaves on the app's `OkHttp` client with the app's session
//! headers.
//!
//! - Phoenix's Rust side sets every header, `authorization` included, and Java
//!   only sends them. [`intercept_call`] hands Phoenix's own requests to the
//!   shared session and hooks in [`super`].
//! - [`send`] issues `Networking.request` with ids from [`FIRST_ID`], and
//!   [`filter`] takes the `@http` replies for those ids out of the dispatch
//!   stream before Phoenix sees them.
//!
//! Module, method and field numbers come from [`crate::host`].

use std::{
   collections::HashMap,
   sync::{
      LazyLock,
      Mutex,
      PoisonError,
      atomic::{
         AtomicI32,
         AtomicU64,
         Ordering,
      },
      mpsc::{
         self,
         Sender,
      },
   },
   time::{
      Duration,
      Instant,
   },
};

use phoenix_runtime::{
   console::url_encoded,
   discord::{
      AUTHORIZE_TIMEOUT,
      Authorization,
      DiscordError,
      InFlight,
      RequestAction,
      TIMEOUT,
      observe_request,
      responses_pending,
      run_response_hooks,
      session_request,
      take_response,
      track_response,
      watch_response,
   },
   fetch::Response,
};
use phoenix_wire::{
   DispatchEvent,
   HostCall,
   Key,
   Value,
   decode_dispatch,
   edit_frames,
};
use serde::Deserialize;

use crate::{
   bridge::{
      self,
      Action,
   },
   host::{
      Event,
      Field,
      Method,
   },
};

/// `Networking.request`'s numbers in the running build.
#[derive(Clone, Copy)]
struct Numbers {
   module:  u16,
   request: u16,
   http:    u16,
   error:   Key,
   id:      Key,
   method:  Key,
   url:     Key,
   headers: Key,
   status:  Key,
}

impl Numbers {
   fn get() -> Option<Self> {
      let host = crate::host::table()?;
      let (module, request) = host.call(Method::NetworkingRequest)?;
      Some(Self {
         module,
         request,
         http: host.event(Event::Http)?,
         error: host.key(Field::Error)?,
         id: host.key(Field::Id)?,
         method: host.key(Field::Method)?,
         url: host.key(Field::Url)?,
         headers: host.key(Field::Headers)?,
         status: host.key(Field::Status)?,
      })
   }
}

/// Far above the counter Phoenix numbers its own requests with.
const FIRST_ID: i32 = 0x5000_0000;

/// A plugin waiting for the user to authorize `client_id` in Phoenix's
/// authorize screen, and Phoenix's request id once it sends the authorization.
struct Authorizing {
   client_id: String,
   call:      Option<i32>,
   waiter:    Sender<String>,
   /// Tells this wait from a newer one.
   turn:      u64,
}

static AUTHORIZING: Mutex<Option<Authorizing>> = Mutex::new(None);
static TURN: AtomicU64 = AtomicU64::new(0);
/// The user whose profile Phoenix fetched last, and when.
static LAST_PROFILE: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// The user `url` fetches the profile of, `…/users/<id>/profile`.
fn profile_of(url: &str) -> Option<String> {
   let after = &url[url.find("/users/")? + "/users/".len()..];
   let (id, rest) = after.split_once('/')?;
   (rest.starts_with("profile") && !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
      .then(|| id.to_owned())
}

/// The user whose profile Phoenix fetched in the last few seconds, for a
/// profile screen that shows no user id of its own.
pub fn last_profile() -> Option<String> {
   let (id, at) = LAST_PROFILE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()?;
   (at.elapsed() < Duration::from_secs(5)).then_some(id)
}

/// `@http`'s fields and body.
struct Reply {
   status: i32,
   error:  Option<String>,
   body:   Vec<u8>,
}

static PENDING: LazyLock<Mutex<HashMap<i32, Sender<Reply>>>> = LazyLock::new(Mutex::default);
static NEXT_ID: AtomicI32 = AtomicI32::new(FIRST_ID);

/// The shared in-flight key of Phoenix's request `id`.
fn key(id: i32) -> u64 {
   u64::from(id.cast_unsigned())
}

/// `Networking.request`'s `headers`, an array of `[name, value]` pairs.
fn headers(args: &Value, key: Key) -> Vec<(String, String)> {
   let Some(Value::Arr(pairs)) = args.field(key) else {
      return Vec::new();
   };
   pairs
      .iter()
      .filter_map(|pair| {
         let Value::Arr(parts) = pair else {
            return None;
         };
         match parts.as_slice() {
            [Value::Str(name), Value::Str(value)] => Some((name.clone(), value.clone())),
            _ => None,
         }
      })
      .collect()
}

/// Whether an `invoke` frame starting with `head` is one [`observe_call`]
/// reads.
pub fn wants(head: &[u8]) -> bool {
   Numbers::get().is_some_and(|keys| HostCall::target(head) == Some((keys.module, keys.request)))
}

/// Hands one of Phoenix's own API requests to the shared session and
/// `phoenix.discord.onRequest` hooks.
pub fn intercept_call(frame: &[u8]) -> Action {
   let Some(keys) = Numbers::get() else {
      return Action::Pass;
   };
   let Ok(mut call) = HostCall::decode(frame) else {
      return Action::Pass;
   };
   let (Some(method), Some(url)) = (
      call.args.field(keys.method).and_then(Value::as_str),
      call.args.field(keys.url).and_then(Value::as_str),
   ) else {
      return Action::Pass;
   };
   let (method, url) = (method.to_owned(), url.to_owned());
   observe_call(&call, keys, &method, &url);
   let headers = headers(&call.args, keys.headers);
   match observe_request(&method, &url, &headers, &call.body) {
      RequestAction::Pass => Action::Pass,
      RequestAction::Drop => Action::Drop,
      RequestAction::Replace(body) => {
         call.body = body;
         // Drop any stale content-length header so `OkHttp` uses the new body
         // length.
         if let Value::Obj(ref mut fields) = call.args {
            for (key, value) in fields.iter_mut() {
               if *key == keys.headers
                  && let Value::Arr(pairs) = value
               {
                  pairs.retain(|pair| {
                     !matches!(pair, Value::Arr(parts) if matches!(parts.first(), Some(Value::Str(name)) if name.eq_ignore_ascii_case("content-length")))
                  });
               }
            }
         }
         Action::Replace(call.encode())
      },
   }
}

/// Notes what Android-only features want from one of Phoenix's own requests:
/// its response for `onResponse` hooks, a profile it fetches, and the
/// authorization a plugin waits for.
fn observe_call(call: &HostCall, keys: Numbers, method: &str, url: &str) {
   let id = call.args.field(keys.id).and_then(Value::as_i32);
   if let Some(id) = id
      && let Some(in_flight) = watch_response(method, url)
   {
      track_response(key(id), in_flight);
   }
   if let Some(user) = profile_of(url) {
      *LAST_PROFILE.lock().unwrap_or_else(PoisonError::into_inner) = Some((user, Instant::now()));
   }
   if url.contains("/oauth2/authorize?")
      && method == "POST"
      && let Some(authorizing) = AUTHORIZING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .as_mut()
      && url
         .split(['?', '&'])
         .any(|pair| pair.strip_prefix("client_id=") == Some(authorizing.client_id.as_str()))
   {
      authorizing.call = id;
   }
}

/// Takes the `@http` replies to phoenix's requests out of a `nativeDispatch`
/// call, and defers replies matched by `phoenix.discord.onResponse` hooks.
pub fn filter(bytes: &[u8]) -> Action {
   let Some(keys) = Numbers::get() else {
      return Action::Pass;
   };
   let watching = AUTHORIZING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_ref()
      .is_some_and(|authorizing| authorizing.call.is_some());
   if !watching
      && !responses_pending()
      && PENDING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .is_empty()
   {
      return Action::Pass;
   }
   let Ok(items) = decode_dispatch(bytes) else {
      return Action::Pass;
   };
   let count = items.len();
   let taken = items
      .into_iter()
      .filter_map(|item| {
         let DispatchEvent::Host { event, args, body } = item.event else {
            return None;
         };
         if event != keys.http {
            return None;
         }
         let id = args.field(keys.id).and_then(Value::as_i32)?;
         if id < FIRST_ID {
            let frame = &bytes[item.offset..item.offset + item.len];
            if let Some(new) = authorized(id, frame, body) {
               return Some((item.offset, Some(new)));
            }
            if let Some(in_flight) = take_response(key(id)) {
               let status = args
                  .field(keys.status)
                  .and_then(Value::as_i32)
                  .unwrap_or_default();
               let owned_frame = frame.to_vec();
               let owned_body = body.to_vec();
               std::thread::spawn(move || {
                  redispatch(in_flight, status, owned_frame, owned_body);
               });
               return Some((item.offset, None));
            }
            return None;
         }
         let waiter = PENDING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
         // With no waiter the request timed out, and the reply goes nowhere.
         if let Some(waiter) = waiter {
            let _ = waiter.send(Reply {
               status: args
                  .field(keys.status)
                  .and_then(Value::as_i32)
                  .unwrap_or_default(),
               error:  args
                  .field(keys.error)
                  .and_then(Value::as_str)
                  .map(str::to_owned),
               body:   body.to_vec(),
            });
         }
         Some((item.offset, None))
      })
      .collect::<Vec<_>>();

   let dropped = taken.iter().filter(|(_, new)| new.is_none()).count();
   match taken.len() {
      0 => Action::Pass,
      _ if dropped == count => Action::Drop,
      _ => Action::Replace(edit_frames(bytes, &taken)),
   }
}

/// Runs matched `phoenix.discord.onResponse` hooks off the `wasm-guest` thread,
/// then re-dispatches the `@http` frame with the body they return.
fn redispatch(in_flight: InFlight, status: i32, frame: Vec<u8>, body: Vec<u8>) {
   let head = frame.len().saturating_sub(body.len());
   // The status stays whatever the host frame says: mobile rewrites bodies only.
   let (_, body) = run_response_hooks(in_flight, status, body);
   let mut out = frame;
   out.truncate(head);
   out.extend_from_slice(&body);
   bridge::dispatch_deferred(out);
}

/// Sends `method path` with Phoenix's session headers, any of them replaced
/// by `headers`, and waits for the answer. Runs on a worker thread.
pub fn send(
   method: &str,
   path: &str,
   headers: Vec<(String, String)>,
   body: Vec<u8>,
) -> Result<Response, DiscordError> {
   let (url, merged) = session_request(path, headers)?;
   let keys = Numbers::get().ok_or(DiscordError::NoHostNames)?;
   let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
   let pairs = merged
      .into_iter()
      .map(|(name, value)| Value::Arr(vec![Value::Str(name), Value::Str(value)]))
      .collect();
   let call = HostCall {
      module: keys.module,
      method: keys.request,
      args: Value::Obj(vec![
         (keys.id, Value::I32(id)),
         (keys.method, Value::Str(method.to_owned())),
         (keys.url, Value::Str(url)),
         (keys.headers, Value::Arr(pairs)),
      ]),
      body,
   };

   let (sender, receiver) = mpsc::channel();
   PENDING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .insert(id, sender);
   let forget = || {
      PENDING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .remove(&id);
   };
   if !bridge::invoke(&call.encode()) {
      forget();
      return Err(DiscordError::NoBridge);
   }
   let Ok(reply) = receiver.recv_timeout(TIMEOUT) else {
      forget();
      return Err(DiscordError::Timeout {
         path: path.to_owned(),
      });
   };
   if reply.status == 0 {
      return Err(DiscordError::Network {
         path:    path.to_owned(),
         message: reply.error.unwrap_or_else(|| "no response".to_owned()),
      });
   }
   Ok(Response {
      status: reply.status,
      body:   reply.body,
   })
}

/// Opens Phoenix's own `OAuth2` authorize screen for `request` and waits for
/// the user to authorize. Phoenix's authorize request is then answered with
/// Discord's own "authorized" page instead of the redirect, so the code stays
/// in the app, and the redirect URL with the code comes back here. The URL
/// holds a credential and is never logged.
pub fn authorize(request: &Authorization) -> Result<String, DiscordError> {
   let link = format!(
      "discord://action/oauth2/authorize?client_id={}&response_type={}&redirect_uri={}&scope={}",
      url_encoded(&request.client_id),
      url_encoded(&request.response_type),
      url_encoded(&request.redirect_uri),
      url_encoded(&request.scopes.join(" ")),
   );
   let turn = TURN.fetch_add(1, Ordering::Relaxed);
   let (sender, receiver) = mpsc::channel();
   // A newer authorize drops the older one's sender, which rejects it.
   *AUTHORIZING.lock().unwrap_or_else(PoisonError::into_inner) = Some(Authorizing {
      client_id: request.client_id.clone(),
      call: None,
      waiter: sender,
      turn,
   });
   if let Err(message) = crate::picker::open(&link) {
      AUTHORIZING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .take();
      return Err(DiscordError::Open { message });
   }
   let location = receiver.recv_timeout(AUTHORIZE_TIMEOUT);
   let mut authorizing = AUTHORIZING.lock().unwrap_or_else(PoisonError::into_inner);
   if authorizing
      .as_ref()
      .is_some_and(|current| current.turn == turn)
   {
      authorizing.take();
   }
   drop(authorizing);
   location.map_err(|_| DiscordError::NotAuthorized)
}

/// Discord's answer to an authorize request.
#[derive(Deserialize)]
struct Authorized {
   location: String,
}

/// Rewrites the reply to Phoenix's own authorize request while a plugin waits
/// for it. Returns the replacement frame.
fn authorized(id: i32, frame: &[u8], body: &[u8]) -> Option<Vec<u8>> {
   let mut authorizing = AUTHORIZING.lock().unwrap_or_else(PoisonError::into_inner);
   if authorizing.as_ref()?.call != Some(id) {
      return None;
   }
   let current = authorizing.take()?;
   drop(authorizing);
   let reply = serde_json::from_slice::<Authorized>(body).ok()?;
   let _ = current.waiter.send(reply.location);
   let head = &frame[..frame.len() - body.len()];
   let mut out = head.to_vec();
   out.extend_from_slice(br#"{"location":"https://discord.com/oauth2/authorized"}"#);
   Some(out)
}
