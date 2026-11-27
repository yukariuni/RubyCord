//! Discord's HTTP API hooks and client requests.
//!
//! Everything but the transport lives here for every platform: the session
//! Discord's own requests carry, `onRequest` filters and hooks, the requests
//! whose responses `onResponse` hooks wait for, and a plugin request's URL and
//! headers. A platform reads Discord's requests and responses, hands them to
//! [`observe_request`], [`watch_response`], [`track_response`] and
//! [`take_response`], and sends plugin requests through [`crate::Platform`].

use std::{
   collections::HashMap,
   sync::{
      LazyLock,
      Mutex,
      PoisonError,
      atomic::{
         AtomicUsize,
         Ordering,
      },
      mpsc,
   },
   time::{
      Duration,
      Instant,
   },
};

use crate::fetch::Response;

pub const TIMEOUT: Duration = Duration::from_secs(30);
pub const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, flub::Error)]
pub enum DiscordError {
   #[error("Phoenix has not made an API request yet, so there is no session")]
   NoSession,
   #[error("{path}: not a path under the API base")]
   Path { path: String },
   #[error("Phoenix's bridge is not up yet")]
   NoBridge,
   #[error("this platform cannot reach Phoenix's session yet")]
   Unsupported,
   #[error("Phoenix's host names are not read yet")]
   NoHostNames,
   #[error("{path}: no answer in {} seconds", TIMEOUT.as_secs())]
   Timeout { path: String },
   #[error("{path}: {message}")]
   Network { path: String, message: String },
   #[error("authorize: could not open Phoenix's authorize screen: {message}")]
   Open { message: String },
   #[error("authorize: the user did not authorize in {} minutes", AUTHORIZE_TIMEOUT.as_secs() / 60)]
   NotAuthorized,
   #[error("authorize: the user declined")]
   Declined,
   #[error("phoenix has not started")]
   NotStarted,
}

/// Rust-side filter for `phoenix.discord.onRequest` and `onResponse` so
/// non-matching API calls never enter `QuickJS`.
#[derive(Clone, Default)]
pub struct RequestFilter {
   pub method:        Option<String>,
   pub path_prefix:   Option<String>,
   pub path_suffix:   Option<String>,
   pub path_includes: Option<String>,
}

impl RequestFilter {
   #[must_use]
   pub fn matches(&self, method: &str, path: &str) -> bool {
      if let Some(expected) = &self.method
         && !method.eq_ignore_ascii_case(expected)
      {
         return false;
      }
      let clean = path.split(['?', '#']).next().unwrap_or(path);
      if let Some(prefix) = &self.path_prefix
         && !clean.starts_with(prefix.as_str())
      {
         return false;
      }
      if let Some(suffix) = &self.path_suffix
         && !clean.ends_with(suffix.as_str())
      {
         return false;
      }
      if let Some(needle) = &self.path_includes
         && !path.contains(needle.as_str())
      {
         return false;
      }
      true
   }
}

struct RequestHook {
   pub id:     u64,
   pub plugin: String,
   pub filter: RequestFilter,
}

static REQUEST_HOOKS: LazyLock<Mutex<Vec<RequestHook>>> = LazyLock::new(Mutex::default);
static REQUEST_HOOK_COUNT: AtomicUsize = AtomicUsize::new(0);
static RESPONSE_HOOKS: LazyLock<Mutex<Vec<RequestHook>>> = LazyLock::new(Mutex::default);
static RESPONSE_HOOK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Outcome of running matching `phoenix.discord.onRequest` hooks.
pub enum RequestAction {
   Pass,
   Drop,
   Replace(Vec<u8>),
}

/// Headers Discord sets per request, or the transport sets itself, which a
/// plugin's request must not inherit.
const PER_REQUEST: [&str; 8] = [
   "accept-encoding",
   "connection",
   "content-length",
   "content-type",
   "host",
   "transfer-encoding",
   "x-audit-log-reason",
   "x-context-properties",
];

/// What Discord's own API requests carry, in memory only.
#[derive(Clone)]
struct Session {
   /// Up to and including the version, like `https://discord.com/api/v9`.
   base:    String,
   headers: Vec<(String, String)>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// A request whose response `onResponse` hooks wait for.
pub struct InFlight {
   method: String,
   path:   String,
   hooks:  Vec<u64>,
   at:     Instant,
}

/// By the platform's id for the request.
static IN_FLIGHT: LazyLock<Mutex<HashMap<u64, InFlight>>> = LazyLock::new(Mutex::default);
static IN_FLIGHT_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Keeps the base and session headers of one of Discord's own API requests.
/// Never logs them.
fn record_session(url: &str, headers: &[(String, String)]) {
   let Some(base) = api_base(url) else {
      return;
   };
   let headers = headers
      .iter()
      .filter(|(name, _)| !PER_REQUEST.contains(&name.to_ascii_lowercase().as_str()))
      .cloned()
      .collect::<Vec<_>>();
   if !headers
      .iter()
      .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
   {
      return;
   }
   let mut session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
   if session.is_none() {
      log::info!("discord: API session found at {base}");
   }
   *session = Some(Session {
      base: base.to_owned(),
      headers,
   });
}

/// Keeps the session of one of Discord's own API requests and runs matching
/// `phoenix.discord.onRequest` hooks on it, filtered here so other requests
/// never enter `QuickJS`. Session headers never reach JS.
#[must_use]
pub fn observe_request(
   method: &str,
   url: &str,
   headers: &[(String, String)],
   body: &[u8],
) -> RequestAction {
   record_session(url, headers);
   let Some(path) = api_path(url) else {
      return RequestAction::Pass;
   };
   // The query can hold search terms, so the trace keeps the path only.
   crate::remote::record(&crate::remote::Trace::Http {
      method,
      path: path.split('?').next().unwrap_or(path),
      bytes: body.len(),
   });
   let matched = matching_request_hooks(method, path);
   if matched.is_empty() {
      return RequestAction::Pass;
   }
   crate::intercept_request(&matched, method, path, body)
}

/// The `onResponse` hooks waiting for the response to `method url`, if any.
#[must_use]
pub fn watch_response(method: &str, url: &str) -> Option<InFlight> {
   let path = api_path(url)?;
   let hooks = matching_response_hooks(method, path);
   (!hooks.is_empty()).then(|| {
      InFlight {
         method: method.to_owned(),
         path: path.to_owned(),
         hooks,
         at: Instant::now(),
      }
   })
}

/// Holds `in_flight` until [`take_response`] asks for request `id`.
pub fn track_response(id: u64, in_flight: InFlight) {
   let mut tracked = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
   if tracked.len() >= 64 {
      tracked.retain(|_, entry| entry.at.elapsed() < Duration::from_secs(60));
   }
   tracked.insert(id, in_flight);
   IN_FLIGHT_COUNT.store(tracked.len(), Ordering::Relaxed);
}

/// Whether any request waits for [`take_response`].
pub fn responses_pending() -> bool {
   IN_FLIGHT_COUNT.load(Ordering::Relaxed) > 0
}

/// The hooks tracked for request `id`, once its response arrives.
pub fn take_response(id: u64) -> Option<InFlight> {
   if !responses_pending() {
      return None;
   }
   let mut tracked = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
   let in_flight = tracked.remove(&id);
   IN_FLIGHT_COUNT.store(tracked.len(), Ordering::Relaxed);
   in_flight
}

/// Runs `in_flight`'s `onResponse` hooks on `body` and returns the status and
/// body to deliver, the originals when they take longer than 4 seconds. Blocks.
#[must_use]
pub fn run_response_hooks(in_flight: InFlight, status: i32, body: Vec<u8>) -> (i32, Vec<u8>) {
   let original = body.clone();
   let (sender, receiver) = mpsc::channel::<(i32, Vec<u8>)>();
   crate::intercept_response(
      in_flight.hooks,
      in_flight.method.clone(),
      in_flight.path.clone(),
      status,
      body,
      sender,
   );
   receiver
      .recv_timeout(Duration::from_secs(4))
      .unwrap_or_else(|_| {
         log::warn!(
            "discord onResponse: {} {} timed out, delivering original",
            in_flight.method,
            in_flight.path
         );
         (status, original)
      })
}

/// The URL and headers of a plugin's request to `path`: Discord's session
/// headers, any of them replaced by `headers`.
pub fn session_request(
   path: &str,
   headers: Vec<(String, String)>,
) -> Result<(String, Vec<(String, String)>), DiscordError> {
   if !path.starts_with('/') {
      return Err(DiscordError::Path {
         path: path.to_owned(),
      });
   }
   let Session {
      base,
      headers: mut merged,
   } = SESSION
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
      .ok_or(DiscordError::NoSession)?;
   merged.retain(|(name, _)| {
      !headers
         .iter()
         .any(|(own, _)| own.eq_ignore_ascii_case(name))
   });
   merged.extend(headers);
   Ok((format!("{base}{path}"), merged))
}

/// What [`authorize`] asks Discord for.
pub struct Authorization {
   pub client_id:     String,
   pub redirect_uri:  String,
   pub scopes:        Vec<String>,
   pub response_type: String,
}

pub fn add_request_hook(id: u64, plugin: String, filter: RequestFilter) {
   let mut hooks = REQUEST_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   hooks.push(RequestHook { id, plugin, filter });
   REQUEST_HOOK_COUNT.store(hooks.len(), Ordering::Relaxed);
}

pub fn remove_request_hook(id: u64) {
   let mut hooks = REQUEST_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   hooks.retain(|hook| hook.id != id);
   REQUEST_HOOK_COUNT.store(hooks.len(), Ordering::Relaxed);
}

pub fn forget_request_hooks(plugin: &str) {
   let mut hooks = REQUEST_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   hooks.retain(|hook| hook.plugin != plugin);
   REQUEST_HOOK_COUNT.store(hooks.len(), Ordering::Relaxed);
}

pub fn add_response_hook(id: u64, plugin: String, filter: RequestFilter) {
   let mut hooks = RESPONSE_HOOKS
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
   hooks.push(RequestHook { id, plugin, filter });
   RESPONSE_HOOK_COUNT.store(hooks.len(), Ordering::Relaxed);
}

pub fn remove_response_hook(id: u64) {
   let mut hooks = RESPONSE_HOOKS
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
   hooks.retain(|hook| hook.id != id);
   RESPONSE_HOOK_COUNT.store(hooks.len(), Ordering::Relaxed);
}

pub fn forget_response_hooks(plugin: &str) {
   let mut hooks = RESPONSE_HOOKS
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
   hooks.retain(|hook| hook.plugin != plugin);
   RESPONSE_HOOK_COUNT.store(hooks.len(), Ordering::Relaxed);
}

/// Returns the registered `onRequest` hook ids matching `(method, path)`.
#[must_use]
pub fn matching_request_hooks(method: &str, path: &str) -> Vec<u64> {
   if REQUEST_HOOK_COUNT.load(Ordering::Relaxed) == 0 {
      return Vec::new();
   }
   let hooks = REQUEST_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   hooks
      .iter()
      .filter(|hook| hook.filter.matches(method, path))
      .map(|hook| hook.id)
      .collect()
}

/// Returns the registered `onResponse` hook ids matching `(method, path)`.
#[must_use]
pub fn matching_response_hooks(method: &str, path: &str) -> Vec<u64> {
   if RESPONSE_HOOK_COUNT.load(Ordering::Relaxed) == 0 {
      return Vec::new();
   }
   let hooks = RESPONSE_HOOKS
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
   hooks
      .iter()
      .filter(|hook| hook.filter.matches(method, path))
      .map(|hook| hook.id)
      .collect()
}

/// `https://host/api/v9` out of an API URL.
#[must_use]
pub fn api_base(url: &str) -> Option<&str> {
   let after = url.find("/api/v")? + "/api/v".len();
   let digits = url[after..]
      .find(|char: char| !char.is_ascii_digit())
      .unwrap_or(url.len() - after);
   (url.starts_with("https://") && digits > 0).then(|| &url[..after + digits])
}

/// Relative path under the API base (e.g. `https://discord.com/api/v9/foo` -> `/foo`).
#[must_use]
pub fn api_path(url: &str) -> Option<&str> {
   let base = api_base(url)?;
   let rest = &url[base.len()..];
   rest.starts_with('/').then_some(rest)
}

/// Sends `method path` with Phoenix's session headers and waits for the answer.
pub fn send(
   method: &str,
   path: &str,
   headers: Vec<(String, String)>,
   body: Vec<u8>,
) -> Result<Response, DiscordError> {
   crate::platform()
      .ok_or(DiscordError::NotStarted)?
      .discord(method, path, headers, body)
}

/// Opens Phoenix's `OAuth2` authorize flow for `request` and returns the
/// redirect URL.
pub fn authorize(request: &Authorization) -> Result<String, DiscordError> {
   crate::platform()
      .ok_or(DiscordError::NotStarted)?
      .authorize(request)
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn parses_api_base_and_path() {
      assert_eq!(
         api_base("https://discord.com/api/v9/users/@me/badges"),
         Some("https://discord.com/api/v9")
      );
      assert_eq!(
         api_path("https://discord.com/api/v9/users/@me/badges"),
         Some("/users/@me/badges")
      );
      assert_eq!(api_path("https://example.com/users/1/badges"), None);
   }

   #[test]
   fn matches_request_filter() {
      let filter = RequestFilter {
         method:        Some("GET".to_owned()),
         path_prefix:   Some("/users/".to_owned()),
         path_suffix:   Some("/badges".to_owned()),
         path_includes: None,
      };
      assert!(filter.matches("get", "/users/@me/badges?with_hidden=true"));
      assert!(!filter.matches("POST", "/users/@me/badges"));
      assert!(!filter.matches("GET", "/users/@me/profile"));
   }
}
