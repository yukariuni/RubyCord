//! Localhost-only HTTP debug shell on `127.0.0.1:4242`, for `adb forward
//! tcp:4242 tcp:4242` on a phone. The platform adds its own endpoints through
//! [`crate::Platform::route`].
//!
//! Only requests from its own origin, or from outside a browser, get through,
//! since `/eval` runs with the Discord session.

use std::{
   collections::VecDeque,
   io::{
      BufRead,
      BufReader,
      Read,
      Write,
   },
   net::{
      SocketAddr,
      TcpListener,
      TcpStream,
   },
   path::Path,
   sync::{
      LazyLock,
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
   thread,
   time::Duration,
};

use serde::{
   Deserialize,
   Serialize,
};
use serde_json::value::RawValue;

use crate::{
   Chosen,
   Control,
   PluginInfo,
   Setting,
   update::{
      self,
      Catalog,
      Listed,
   },
};

pub const ADDR: &str = "127.0.0.1:4242";
const MAX_BODY: usize = 4 << 20;
const MAX_EVENTS_LOG: usize = 256;

static ENABLED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static BOUND_ADDR: OnceLock<String> = OnceLock::new();
static EVENTS: Ring = Ring::new(MAX_EVENTS_LOG);

/// The last entries of a debug log, as JSON.
pub struct Ring {
   entries: LazyLock<Mutex<VecDeque<Box<RawValue>>>>,
   max:     usize,
}

impl Ring {
   #[must_use]
   pub const fn new(max: usize) -> Self {
      Self {
         entries: LazyLock::new(|| Mutex::new(VecDeque::new())),
         max,
      }
   }

   /// Keeps `entry` when the debug shell is on, dropping the oldest past the
   /// capacity.
   pub fn push(&self, entry: &impl Serialize) {
      if !enabled() {
         return;
      }
      let entry = match serde_json::value::to_raw_value(entry) {
         Ok(entry) => entry,
         Err(err) => {
            log::warn!("remote: a debug entry did not serialize: {err}");
            return;
         },
      };
      let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
      if entries.len() == self.max {
         entries.pop_front();
      }
      entries.push_back(entry);
   }

   pub fn clear(&self) {
      self
         .entries
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .clear();
   }

   /// The newest entries, filtered by the `match` and `limit` query
   /// parameters.
   #[must_use]
   pub fn recent(&self, query: &str, default_limit: usize) -> Reply {
      let filter = query_param(query, "match");
      let limit = query_param(query, "limit")
         .and_then(|limit| limit.parse::<usize>().ok())
         .unwrap_or(default_limit);
      ok_json(
         &self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .rev()
            .filter(|entry| {
               filter
                  .as_ref()
                  .is_none_or(|needle| entry.get().contains(needle.as_str()))
            })
            .take(limit)
            .collect::<Vec<_>>(),
      )
   }
}

/// A status line, a content type and a body.
pub type Reply = (&'static str, &'static str, Vec<u8>);

/// An event for the debug ring buffer. Never put HTTP headers, bodies,
/// message content or credentials in one.
#[derive(Serialize)]
#[serde(tag = "source", rename_all = "camelCase")]
pub enum Trace<'a> {
   /// A gateway event, by name and payload size.
   Gateway { event: &'a str, bytes: usize },
   /// One of Discord's own API requests.
   Http {
      method: &'a str,
      path:   &'a str,
      bytes:  usize,
   },
}

/// Records `trace` into the debug event log when the debug shell is on.
pub fn record(trace: &impl Serialize) {
   EVENTS.push(trace);
}

/// Empties the debug event log.
pub fn clear_events() {
   EVENTS.clear();
}

#[must_use]
pub fn enabled() -> bool {
   ENABLED.load(Ordering::Relaxed)
}

pub fn set_flag(on: bool) {
   ENABLED.store(on, Ordering::Relaxed);
}

pub fn set_enabled(on: bool) {
   ENABLED.store(on, Ordering::Relaxed);
   if on {
      ensure_started();
   }
}

/// A random token every request must carry, once a platform asks for one.
static TOKEN: OnceLock<String> = OnceLock::new();
static TOKEN_FILE: OnceLock<std::path::PathBuf> = OnceLock::new();
/// The port `dev.ps1` and the token file are for. Another instance that starts
/// while this one runs binds the next port and hands off to this one.
const SHELL_PORT: u16 = 4242;
const TOKEN_COOKIE: &str = "rubycord_token";

/// Makes every request carry a random per-start token, and writes it to
/// `token_file` for local tools like `scripts/dev.ps1`.
///
/// The token comes as `?token=`, an `X-RubyCord-Token` header or the cookie
/// `/ui?token=` sets. Platforms whose shell is always on call this: any local
/// process or app could otherwise reach `/eval` and run JS with the session.
pub fn require_token(token_file: &Path) {
   TOKEN.get_or_init(random_token);
   let _ = TOKEN_FILE.set(token_file.to_owned());
   // The runtime may have started the shell before the platform asked for a
   // token.
   write_token_file();
}

/// Writes the token where `dev.ps1` reads it, once this process owns the
/// shell's port: a second launch that only hands off to the running client
/// would otherwise replace the token the running shell checks.
fn write_token_file() {
   use std::io::Write as _;
   let (Some(token), Some(token_file)) = (TOKEN.get(), TOKEN_FILE.get()) else {
      return;
   };
   let owns_port = BOUND_ADDR
      .get()
      .and_then(|addr| addr.parse::<SocketAddr>().ok())
      .is_some_and(|addr| addr.port() == SHELL_PORT);
   if !owns_port {
      return;
   }
   let mut options = std::fs::OpenOptions::new();
   options.write(true).create(true).truncate(true);
   // Other users on the machine must not read it out of a shared home; the
   // mode only applies to a new file.
   #[cfg(unix)]
   {
      let _ = std::fs::remove_file(token_file);
      std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
   }
   if let Err(err) = options
      .open(token_file)
      .and_then(|mut file| file.write_all(token.as_bytes()))
   {
      log::warn!("remote shell: writing {}: {err}", token_file.display());
   }
}

/// The dashboard's URL, with the token when one is required.
#[must_use]
pub fn dashboard_url() -> String {
   let url = format!("http://{}/ui", bound_addr());
   match TOKEN.get() {
      Some(token) => format!("{url}?token={token}"),
      None => url,
   }
}

/// 128 bits from std's OS-seeded `SipHash` keys, so no RNG crate is needed.
fn random_token() -> String {
   use std::{
      fmt::Write as _,
      hash::{
         BuildHasher,
         Hasher,
      },
   };
   let state = std::hash::RandomState::new();
   (0u64..2).fold(String::with_capacity(32), |mut out, at| {
      let mut hasher = state.build_hasher();
      hasher.write_u64(at);
      let _ = write!(out, "{:016x}", hasher.finish());
      out
   })
}

/// Compares in time independent of where the first difference is.
fn same_token(given: &str, token: &str) -> bool {
   given.len() == token.len()
      && given
         .bytes()
         .zip(token.bytes())
         .fold(0, |diff, (a, b)| diff | (a ^ b))
         == 0
}

/// Whether `req` carries `token`, and whether it came in the query.
fn token_given(req: &Request, token: &str) -> (bool, bool) {
   let in_query = req
      .query
      .split('&')
      .filter_map(|pair| pair.strip_prefix("token="))
      .any(|given| same_token(given, token));
   let elsewhere = req
      .token_header
      .as_deref()
      .is_some_and(|given| same_token(given, token))
      || req.cookie.as_deref().is_some_and(|cookie| {
         cookie
            .split(';')
            .filter_map(|pair| pair.trim().strip_prefix(TOKEN_COOKIE)?.strip_prefix('='))
            .any(|given| same_token(given, token))
      });
   (in_query || elsewhere, in_query)
}

/// Returns the actual bound `host:port` of the localhost server, falling back
/// to [`ADDR`] if not started yet.
#[must_use]
pub fn bound_addr() -> &'static str {
   BOUND_ADDR.get().map_or(ADDR, String::as_str)
}

fn bind_localhost() -> std::io::Result<(TcpListener, SocketAddr)> {
   for port in SHELL_PORT..=SHELL_PORT + 10 {
      if let Ok(listener) = TcpListener::bind(("127.0.0.1", port))
         && let Ok(addr) = listener.local_addr()
      {
         return Ok((listener, addr));
      }
   }
   let listener = TcpListener::bind("127.0.0.1:0")?;
   let addr = listener.local_addr()?;
   Ok((listener, addr))
}

pub fn ensure_started() {
   if !enabled() {
      return;
   }
   if RUNNING.swap(true, Ordering::AcqRel) {
      return;
   }
   let (listener, addr) = match bind_localhost() {
      Ok(bound) => bound,
      Err(err) => {
         log::error!("remote shell: bind 127.0.0.1 failed: {err}");
         RUNNING.store(false, Ordering::Release);
         return;
      },
   };
   let addr_str = addr.to_string();
   let _ = BOUND_ADDR.set(addr_str.clone());
   write_token_file();
   let _ = listener.set_nonblocking(true);
   log::info!("debug shell listening on http://{addr_str}");

   thread::spawn(move || {
      while enabled() {
         match listener.accept() {
            Ok((stream, _)) => {
               thread::spawn(move || handle(stream));
            },
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
               thread::sleep(Duration::from_millis(100));
            },
            Err(err) => {
               log::warn!("remote shell accept: {err}");
               thread::sleep(Duration::from_millis(250));
            },
         }
      }
      log::info!("remote shell stopped");
      RUNNING.store(false, Ordering::Release);
   });
}

/// A debug shell request.
pub struct Request {
   pub method:     String,
   pub path:       String,
   pub query:      String,
   pub accept:     String,
   host:           Option<String>,
   origin:         Option<String>,
   sec_fetch_site: Option<String>,
   cookie:         Option<String>,
   token_header:   Option<String>,
   pub body:       Vec<u8>,
}

fn is_loopback_host(host: &str) -> bool {
   let name = host.split(':').next().unwrap_or(host);
   name.eq_ignore_ascii_case("127.0.0.1") || name.eq_ignore_ascii_case("localhost")
}

/// Whether `origin` is this server's own, so another local web server's page
/// cannot drive it.
fn is_allowed_origin(origin: &str, port: u16) -> bool {
   origin
      .strip_prefix("http://")
      .and_then(|rest| rest.rsplit_once(':'))
      .is_some_and(|(host, at)| {
         (host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost"))
            && at.parse::<u16>() == Ok(port)
      })
}

const fn is_allowed_fetch_site(site: &str) -> bool {
   site.eq_ignore_ascii_case("same-origin") || site.eq_ignore_ascii_case("none")
}

fn handle(mut stream: TcpStream) {
   let port = stream.local_addr().map_or(0, |addr| addr.port());
   let _ = stream.set_nonblocking(false);
   let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
   let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

   let Ok(req) = read_request(&stream) else {
      return;
   };
   if req
      .host
      .as_deref()
      .is_some_and(|host| !is_loopback_host(host))
      || req
         .origin
         .as_deref()
         .is_some_and(|origin| !is_allowed_origin(origin, port))
      || req
         .sec_fetch_site
         .as_deref()
         .is_some_and(|site| !is_allowed_fetch_site(site))
   {
      let (status, content_type, response) = err("403 Forbidden", "loopback origin only");
      write_response(&mut stream, status, content_type, &response, None);
      return;
   }
   let mut cookie = None;
   if let Some(token) = TOKEN.get() {
      let (given, in_query) = token_given(&req, token);
      if !given {
         let (status, content_type, response) = err("403 Forbidden", "missing debug shell token");
         write_response(&mut stream, status, content_type, &response, None);
         return;
      }
      // The dashboard's own requests then carry the token without it in
      // every URL.
      if in_query {
         cookie = Some(format!(
            "{TOKEN_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/"
         ));
      }
   }
   let (status, content_type, response) = dispatch(&req);
   write_response(
      &mut stream,
      status,
      content_type,
      &response,
      cookie.as_deref(),
   );
}

fn write_response(
   stream: &mut TcpStream,
   status: &str,
   content_type: &str,
   response: &[u8],
   cookie: Option<&str>,
) {
   let cookie = cookie.map_or_else(String::new, |cookie| format!("Set-Cookie: {cookie}\r\n"));
   let header = format!(
      "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nCache-Control: \
       no-store\r\n{cookie}Content-Length: {}\r\nConnection: close\r\n\r\n",
      response.len()
   );
   let _ = stream.write_all(header.as_bytes());
   let _ = stream.write_all(response);
}

fn read_request(stream: &TcpStream) -> std::io::Result<Request> {
   let mut reader = BufReader::new(stream);
   let mut request_line = String::new();
   if reader.read_line(&mut request_line)? == 0 {
      return Err(std::io::Error::new(
         std::io::ErrorKind::UnexpectedEof,
         "eof",
      ));
   }
   let mut parts = request_line.split_whitespace();
   let method = parts.next().unwrap_or_default().to_owned();
   let raw_path = parts.next().unwrap_or("/");
   let (path, query) = match raw_path.split_once('?') {
      Some((path, query)) => (path.to_owned(), query.to_owned()),
      None => (raw_path.to_owned(), String::new()),
   };

   let mut content_length: usize = 0;
   let mut accept = String::new();
   let mut host = None;
   let mut origin = None;
   let mut sec_fetch_site = None;
   let mut cookie = None;
   let mut token_header = None;
   loop {
      let mut line = String::new();
      if reader.read_line(&mut line)? == 0 {
         break;
      }
      let trimmed = line.trim_end_matches(['\r', '\n']);
      if trimmed.is_empty() {
         break;
      }
      if let Some((name, value)) = trimmed.split_once(':') {
         let name = name.trim();
         let value = value.trim();
         if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse::<usize>().unwrap_or(0).min(MAX_BODY);
         } else if name.eq_ignore_ascii_case("accept") {
            value.clone_into(&mut accept);
         } else if name.eq_ignore_ascii_case("host") {
            host = Some(value.to_owned());
         } else if name.eq_ignore_ascii_case("origin") {
            origin = Some(value.to_owned());
         } else if name.eq_ignore_ascii_case("sec-fetch-site") {
            sec_fetch_site = Some(value.to_owned());
         } else if name.eq_ignore_ascii_case("cookie") {
            cookie = Some(value.to_owned());
         } else if name.eq_ignore_ascii_case("x-rubycord-token") {
            token_header = Some(value.to_owned());
         }
      }
   }
   let mut body = vec![0u8; content_length];
   if content_length > 0 {
      reader.read_exact(&mut body)?;
   }
   Ok(Request {
      method,
      path,
      query,
      accept,
      host,
      origin,
      sec_fetch_site,
      cookie,
      token_header,
      body,
   })
}

/// Percent-decodes a URI component. A `%` not followed by two hex digits stays
/// as it is.
fn percent_decode(raw: &str) -> String {
   let bytes = raw.as_bytes();
   let mut out = Vec::with_capacity(bytes.len());
   let mut at = 0;
   while let Some(&byte) = bytes.get(at) {
      let escaped = bytes
         .get(at + 1..at + 3)
         .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
         .and_then(|hex| u8::from_str_radix(str::from_utf8(hex).ok()?, 16).ok());
      match (byte, escaped) {
         (b'%', Some(decoded)) => {
            out.push(decoded);
            at += 3;
         },
         (b'+', _) => {
            out.push(b' ');
            at += 1;
         },
         _ => {
            out.push(byte);
            at += 1;
         },
      }
   }
   String::from_utf8_lossy(&out).into_owned()
}

/// The percent-decoded value of `key`, as `encodeURIComponent` or a form wrote
/// it.
#[must_use]
pub fn query_param(query: &str, key: &str) -> Option<String> {
   let (_, raw) = query
      .split('&')
      .filter_map(|pair| pair.split_once('='))
      .find(|(name, _)| *name == key)?;
   Some(percent_decode(raw))
}

fn dispatch(req: &Request) -> Reply {
   if let Some(reply) = crate::platform().and_then(|platform| platform.route(req)) {
      return reply;
   }
   let method = req.method.as_str();
   let path = req.path.as_str();
   let query = req.query.as_str();
   let body = req.body.as_slice();

   match (method, path) {
      ("GET", "/favicon.ico") => ("204 No Content", "image/x-icon", Vec::new()),
      ("GET", "/" | "/status") => {
         let plugins = crate::plugins();
         ok_json(&Status {
            version:      crate::update::VERSION,
            staff:        crate::staff_enabled(),
            remote_shell: enabled(),
            plugins:      plugins.iter().map(PluginJson::from).collect(),
         })
      },
      ("GET", "/plugins") => {
         let plugins = crate::plugins();
         ok_json(&plugins.iter().map(PluginJson::from).collect::<Vec<_>>())
      },
      ("POST", "/plugins/reload-all") => {
         crate::reload_all();
         crate::refresh_settings();
         ok_json(&Done::Started)
      },
      ("GET", "/catalog") => catalog_state(),
      ("POST", "/catalog/refresh") => {
         update::fetch_catalog();
         ok_json(&Done::Started)
      },
      ("POST", "/catalog/install") => catalog_install(body),
      ("GET", "/logs") => {
         let mut lines = crate::console::lines();
         lines.reverse();
         let text = lines
            .into_iter()
            .map(|line| format!("{} [{}] {}", line.time, line.level, line.text))
            .collect::<Vec<_>>()
            .join("\n");
         ("200 OK", "text/plain; charset=utf-8", text.into_bytes())
      },
      ("POST", "/eval") => {
         let Ok(code) = std::str::from_utf8(body) else {
            return err("400 Bad Request", "eval body must be UTF-8");
         };
         match crate::evaluate_capture(code) {
            Ok(result) => ("200 OK", "text/plain; charset=utf-8", result.into_bytes()),
            Err(message) => err("500 Internal Server Error", &message),
         }
      },
      ("GET", "/events") => EVENTS.recent(query, 50),
      ("POST", "/clear") => {
         clear_events();
         ok_json(&Done::Cleared)
      },
      ("GET", "/update") => update_state(),
      ("POST", "/update/check") => {
         crate::update::check_now();
         ok_json(&Done::Started)
      },
      ("POST", "/update/apply") => {
         crate::update::apply();
         ok_json(&Done::Started)
      },
      ("POST", "/debug/staff") => {
         let text = String::from_utf8_lossy(body);
         let on = matches!(text.trim(), "1" | "true" | "on");
         crate::set_staff_enabled(on);
         crate::refresh_settings();
         ok_json(&Done::Staff(crate::staff_enabled()))
      },
      _ if path.starts_with("/plugins/") => plugin_route(method, &path["/plugins/".len()..], body),
      _ => {
         let routes = crate::platform().map_or("", |platform| platform.routes());
         let help = format!(
            "endpoints: GET /status, GET /plugins, POST /plugins/reload-all, GET|PUT|DELETE \
             /plugins/<name>, POST /plugins/<name>/{{reload,enable,disable,setting,press}}, GET \
             /catalog, POST /catalog/{{refresh,install}}, GET /update, POST \
             /update/{{check,apply}}, POST /eval, GET /logs, GET /events[?match=&limit=], POST \
             /clear{routes}"
         );
         err("404 Not Found", &help)
      },
   }
}

#[derive(Serialize)]
struct Status<'a> {
   version:      &'static str,
   staff:        bool,
   remote_shell: bool,
   plugins:      Vec<PluginJson<'a>>,
}

/// A plugin and its state, for `/status` and `/plugins`.
#[derive(Serialize)]
struct PluginJson<'a> {
   name:        &'a str,
   enabled:     bool,
   installed:   bool,
   error:       Option<&'a str>,
   version:     Option<&'a str>,
   author:      Option<&'a str>,
   description: Option<&'a str>,
   settings:    Vec<SettingJson<'a>>,
}

#[derive(Serialize)]
struct ChoiceJson<'a> {
   value: &'a str,
   label: &'a str,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ControlJson<'a> {
   Switch {
      value: bool,
   },
   Text {
      value: &'a str,
   },
   Secret {
      has_value: bool,
   },
   Select {
      value:   &'a str,
      options: Vec<ChoiceJson<'a>>,
   },
   Number {
      value: f64,
   },
   Button,
}

#[derive(Serialize)]
struct SettingJson<'a> {
   key:         &'a str,
   title:       &'a str,
   description: Option<&'a str>,
   #[serde(skip_serializing_if = "std::ops::Not::not")]
   multiline:   bool,
   #[serde(flatten)]
   control:     ControlJson<'a>,
}

impl<'a> From<&'a Setting> for SettingJson<'a> {
   fn from(setting: &'a Setting) -> Self {
      let control = match &setting.control {
         Control::Switch(value) => ControlJson::Switch { value: *value },
         Control::Text(value) => ControlJson::Text { value },
         Control::Secret(value) => {
            ControlJson::Secret {
               has_value: !value.is_empty(),
            }
         },
         Control::Select { options, chosen } => {
            ControlJson::Select {
               value:   chosen,
               options: options
                  .iter()
                  .map(|choice| {
                     ChoiceJson {
                        value: &choice.value,
                        label: &choice.label,
                     }
                  })
                  .collect(),
            }
         },
         Control::Number(value) => ControlJson::Number { value: *value },
         Control::Button => ControlJson::Button,
      };
      Self {
         key: &setting.key,
         title: &setting.title,
         description: setting.description.as_deref(),
         multiline: setting.multiline,
         control,
      }
   }
}

impl<'a> From<&'a PluginInfo> for PluginJson<'a> {
   fn from(plugin: &'a PluginInfo) -> Self {
      Self {
         name:        &plugin.name,
         enabled:     plugin.enabled,
         installed:   plugin.installed,
         error:       plugin.error.as_deref(),
         version:     plugin.header.version.as_deref(),
         author:      plugin.header.author.as_deref(),
         description: plugin.header.description.as_deref(),
         settings:    plugin.settings.iter().map(SettingJson::from).collect(),
      }
   }
}

/// What a mutating endpoint did.
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Done<'a> {
   Cleared,
   Started,
   Staff(bool),
   Installed(&'a str),
   Uninstalled(&'a str),
   Restored(&'a str),
   Reloaded(&'a str),
   Enabled(&'a str),
   Disabled(&'a str),
   Saved(&'a str),
   Pressed(&'a str),
}

fn update_state() -> Reply {
   #[derive(Serialize)]
   struct Found {
      loader:  Option<String>,
      plugins: Vec<String>,
   }

   #[derive(Serialize)]
   struct State {
      version: &'static str,
      busy:    bool,
      status:  Option<String>,
      found:   Option<Found>,
   }

   ok_json(&State {
      version: crate::update::VERSION,
      busy:    crate::update::busy(),
      status:  crate::update::status(),
      found:   crate::update::found().map(|found| {
         Found {
            loader:  found.loader,
            plugins: found.plugins,
         }
      }),
   })
}

fn plugin_route(method: &str, rest: &str, body: &[u8]) -> Reply {
   let (raw_name, action) = rest
      .split_once('/')
      .map_or((rest, None), |(name, action)| (name, Some(action)));
   let name = percent_decode(raw_name);
   let name = name.as_str();
   let valid = !name.is_empty()
      && name
         .bytes()
         .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
   if !valid {
      return err("400 Bad Request", "invalid plugin name");
   }
   match (method, action) {
      ("GET", None) => {
         crate::source(name).map_or_else(
            || err("404 Not Found", "plugin not found"),
            |source| {
               (
                  "200 OK",
                  "text/javascript; charset=utf-8",
                  source.into_bytes(),
               )
            },
         )
      },
      ("PUT" | "POST", None) => {
         let Ok(source) = String::from_utf8(body.to_vec()) else {
            return err("400 Bad Request", "plugin source must be UTF-8");
         };
         match crate::save_source(name, source) {
            Ok(()) => {
               crate::refresh_settings();
               ok_json(&Done::Installed(name))
            },
            Err(message) => err("500 Internal Server Error", &message.to_string()),
         }
      },
      ("DELETE", None) => {
         match crate::uninstall(name) {
            Ok(outcome) => {
               crate::refresh_settings();
               ok_json(&match outcome {
                  crate::Uninstalled::Removed => Done::Uninstalled(name),
                  crate::Uninstalled::RestoredBundled => Done::Restored(name),
               })
            },
            Err(message) => err("500 Internal Server Error", &message.to_string()),
         }
      },
      ("POST", Some("reload")) => {
         crate::reload(name);
         crate::refresh_settings();
         ok_json(&Done::Reloaded(name))
      },
      ("POST", Some("enable")) => {
         crate::set_enabled(name, true);
         crate::refresh_settings();
         ok_json(&Done::Enabled(name))
      },
      ("POST", Some("disable")) => {
         crate::set_enabled(name, false);
         crate::refresh_settings();
         ok_json(&Done::Disabled(name))
      },
      ("POST", Some("setting")) => {
         #[derive(Deserialize)]
         struct SetRequest {
            key:   String,
            value: Chosen,
         }
         let Ok(req) = serde_json::from_slice::<SetRequest>(body) else {
            return err("400 Bad Request", "expected JSON {key, value}");
         };
         crate::set_setting(name, &req.key, req.value);
         crate::refresh_settings();
         ok_json(&Done::Saved(&req.key))
      },
      ("POST", Some("press")) => {
         #[derive(Deserialize)]
         struct PressRequest {
            key: String,
         }
         let Ok(req) = serde_json::from_slice::<PressRequest>(body) else {
            return err("400 Bad Request", "expected JSON {key}");
         };
         crate::press(name, &req.key);
         ok_json(&Done::Pressed(&req.key))
      },
      _ => err("404 Not Found", "unknown plugin route"),
   }
}

fn catalog_state() -> Reply {
   #[derive(Serialize)]
   #[serde(tag = "state", rename_all = "lowercase")]
   enum CatalogJson {
      Unfetched,
      Loading,
      Loaded { plugins: Vec<Listed> },
      Failed { error: String },
   }

   let current = update::catalog();
   if matches!(current, Catalog::Unfetched) {
      update::fetch_catalog();
   }
   match current {
      Catalog::Unfetched => ok_json(&CatalogJson::Unfetched),
      Catalog::Loading => ok_json(&CatalogJson::Loading),
      Catalog::Loaded(plugins) => ok_json(&CatalogJson::Loaded { plugins }),
      Catalog::Failed(error) => ok_json(&CatalogJson::Failed { error }),
   }
}

fn catalog_install(body: &[u8]) -> Reply {
   #[derive(Deserialize)]
   struct InstallRequest {
      name: Option<String>,
      url:  Option<String>,
   }

   let Ok(req) = serde_json::from_slice::<InstallRequest>(body) else {
      return err("400 Bad Request", "expected JSON with a name or a url");
   };
   let outcome = match (req.name, req.url) {
      (Some(name), _) => update::install_listed(&name),
      (None, Some(url)) => crate::install(&url),
      (None, None) => return err("400 Bad Request", "provide either name or url"),
   };
   match outcome {
      Ok(installed) => {
         crate::refresh_settings();
         ok_json(&Done::Installed(&installed.name))
      },
      Err(message) => err("400 Bad Request", &message.to_string()),
   }
}

pub fn ok_json(value: &impl Serialize) -> Reply {
   match serde_json::to_vec_pretty(value) {
      Ok(body) => ("200 OK", "application/json; charset=utf-8", body),
      Err(error) => err("500 Internal Server Error", &error.to_string()),
   }
}

#[must_use]
pub fn err(status: &'static str, message: &str) -> Reply {
   (
      status,
      "text/plain; charset=utf-8",
      message.as_bytes().to_vec(),
   )
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn validates_loopback_hosts_and_origins() {
      assert!(is_loopback_host("127.0.0.1:4242"));
      assert!(is_loopback_host("localhost:4242"));
      assert!(!is_loopback_host("evil.example:4242"));
      assert!(is_allowed_origin("http://127.0.0.1:4242", 4242));
      assert!(is_allowed_origin("http://localhost:4242", 4242));
      assert!(!is_allowed_origin("http://localhost:3000", 4242));
      assert!(!is_allowed_origin("https://evil.example", 4242));
      assert!(!is_allowed_origin(
         "http://127.0.0.1.evil.example:4242",
         4242
      ));
      assert!(is_allowed_fetch_site("same-origin"));
      assert!(is_allowed_fetch_site("none"));
      assert!(!is_allowed_fetch_site("same-site"));
      assert!(!is_allowed_fetch_site("cross-site"));
   }

   #[test]
   fn takes_the_token_from_query_header_or_cookie() {
      let request = |query: &str, cookie: Option<&str>, header: Option<&str>| {
         Request {
            method:         "GET".to_owned(),
            path:           "/ui".to_owned(),
            query:          query.to_owned(),
            accept:         String::new(),
            host:           None,
            origin:         None,
            sec_fetch_site: None,
            cookie:         cookie.map(str::to_owned),
            token_header:   header.map(str::to_owned),
            body:           Vec::new(),
         }
      };
      let token = "abc123";
      assert_eq!(
         token_given(&request("x=1&token=abc123", None, None), token),
         (true, true)
      );
      assert_eq!(
         token_given(&request("", Some("a=b; rubycord_token=abc123"), None), token),
         (true, false)
      );
      assert_eq!(
         token_given(&request("", None, Some("abc123")), token),
         (true, false)
      );
      assert!(!token_given(&request("token=abc12", None, None), token).0);
      assert!(!token_given(&request("", Some("rubycord_tokenx=abc123"), None), token).0);
      assert!(!token_given(&request("", None, None), token).0);
      assert_eq!(random_token().len(), 32);
      assert_ne!(random_token(), random_token());
   }

   #[test]
   fn decodes_percent_encoded_names() {
      assert_eq!(percent_decode("silent-typing"), "silent-typing");
      assert_eq!(percent_decode("my%20plugin"), "my plugin");
   }
}
