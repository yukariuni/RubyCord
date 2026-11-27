//! HTTP through the system's libcurl, loaded with `dlopen` so the build needs
//! no curl headers or link-time dependency, and a missing libcurl only turns
//! fetch off. Headers stay in this process, never on a command line.

use std::{
   ffi::{CStr, CString, c_char, c_int, c_long, c_void},
   ptr,
   sync::OnceLock,
};

use phoenix_runtime::fetch::{FetchError, LIMIT, Request, Response, TIMEOUT_MS};

#[cfg(target_os = "linux")]
const LIBCURL: &CStr = c"libcurl.so.4";
/// Part of macOS, built against its Secure Transport and keychain.
#[cfg(target_os = "macos")]
const LIBCURL: &CStr = c"/usr/lib/libcurl.4.dylib";

const CURL_GLOBAL_DEFAULT: c_long = 3;
const CURLPROTO_HTTP: c_long = 1;
const CURLPROTO_HTTPS: c_long = 2;

const CURLOPT_WRITEDATA: c_int = 10_001;
const CURLOPT_URL: c_int = 10_002;
const CURLOPT_POSTFIELDS: c_int = 10_015;
const CURLOPT_USERAGENT: c_int = 10_018;
const CURLOPT_LOW_SPEED_LIMIT: c_int = 19;
const CURLOPT_LOW_SPEED_TIME: c_int = 20;
const CURLOPT_HTTPHEADER: c_int = 10_023;
const CURLOPT_CUSTOMREQUEST: c_int = 10_036;
const CURLOPT_NOBODY: c_int = 44;
const CURLOPT_FOLLOWLOCATION: c_int = 52;
const CURLOPT_MAXREDIRS: c_int = 68;
const CURLOPT_NOSIGNAL: c_int = 99;
const CURLOPT_CONNECTTIMEOUT_MS: c_int = 156;
const CURLOPT_PROTOCOLS: c_int = 181;
const CURLOPT_REDIR_PROTOCOLS: c_int = 182;
const CURLOPT_WRITEFUNCTION: c_int = 20_011;
const CURLOPT_POSTFIELDSIZE_LARGE: c_int = 30_120;
const CURLINFO_RESPONSE_CODE: c_int = 0x0020_0002;

const CURLE_UNSUPPORTED_PROTOCOL: c_int = 1;
const CURLE_URL_MALFORMAT: c_int = 3;

type Easy = *mut c_void;
type Slist = *mut c_void;
type WriteFn = extern "C" fn(*const c_char, usize, usize, *mut c_void) -> usize;

struct Curl {
   easy_init: unsafe extern "C" fn() -> Easy,
   easy_setopt: unsafe extern "C" fn(Easy, c_int, ...) -> c_int,
   easy_perform: unsafe extern "C" fn(Easy) -> c_int,
   easy_getinfo: unsafe extern "C" fn(Easy, c_int, ...) -> c_int,
   easy_cleanup: unsafe extern "C" fn(Easy),
   easy_strerror: unsafe extern "C" fn(c_int) -> *const c_char,
   slist_append: unsafe extern "C" fn(Slist, *const c_char) -> Slist,
   slist_free_all: unsafe extern "C" fn(Slist),
}

/// # Safety
///
/// `T` must be the C function pointer type of the symbol `name`.
unsafe fn sym<T>(lib: *mut c_void, name: &CStr) -> Option<T> {
   const { assert!(size_of::<T>() == size_of::<*mut c_void>()) };
   // SAFETY: `lib` is a live `dlopen` handle and `name` is NUL-terminated.
   let found = unsafe { libc::dlsym(lib, name.as_ptr()) };
   // SAFETY: the caller names `T` as this symbol's type, and the sizes match.
   (!found.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&found) })
}

/// libcurl stays loaded for the life of the process, so the pointers never
/// dangle.
fn curl() -> Option<&'static Curl> {
   static CURL: OnceLock<Option<Curl>> = OnceLock::new();
   CURL
      .get_or_init(|| {
         // SAFETY: each symbol is read with its type from `curl/curl.h`.
         let curl = unsafe {
            let lib = libc::dlopen(LIBCURL.as_ptr(), libc::RTLD_NOW);
            if lib.is_null() {
               log::warn!(
                  "fetch: {} is not installed, HTTP is off",
                  LIBCURL.to_string_lossy()
               );
               return None;
            }
            let global_init: unsafe extern "C" fn(c_long) -> c_int = sym(lib, c"curl_global_init")?;
            if global_init(CURL_GLOBAL_DEFAULT) != 0 {
               return None;
            }
            Curl {
               easy_init: sym(lib, c"curl_easy_init")?,
               easy_setopt: sym(lib, c"curl_easy_setopt")?,
               easy_perform: sym(lib, c"curl_easy_perform")?,
               easy_getinfo: sym(lib, c"curl_easy_getinfo")?,
               easy_cleanup: sym(lib, c"curl_easy_cleanup")?,
               easy_strerror: sym(lib, c"curl_easy_strerror")?,
               slist_append: sym(lib, c"curl_slist_append")?,
               slist_free_all: sym(lib, c"curl_slist_free_all")?,
            }
         };
         Some(curl)
      })
      .as_ref()
}

struct Handle<'a>(&'a Curl, Easy);

impl Drop for Handle<'_> {
   fn drop(&mut self) {
      // SAFETY: `self.1` is a live easy handle owned by this guard.
      unsafe { (self.0.easy_cleanup)(self.1) };
   }
}

struct Headers<'a>(&'a Curl, Slist);

impl Drop for Headers<'_> {
   fn drop(&mut self) {
      // SAFETY: `self.1` is null or a list built by `curl_slist_append`.
      unsafe { (self.0.slist_free_all)(self.1) };
   }
}

struct Sink {
   body: Vec<u8>,
   too_large: bool,
}

extern "C" fn write_body(
   data: *const c_char,
   size: usize,
   count: usize,
   sink: *mut c_void,
) -> usize {
   let len = size.saturating_mul(count);
   // SAFETY: `sink` is the `Sink` set as `CURLOPT_WRITEDATA` in `send`.
   let sink = unsafe { &mut *sink.cast::<Sink>() };
   if sink.body.len().saturating_add(len) > LIMIT {
      sink.too_large = true;
      return 0;
   }
   if len > 0 {
      // SAFETY: curl hands `len` readable bytes at `data`.
      sink
         .body
         .extend_from_slice(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) });
   }
   len
}

#[expect(clippy::too_many_lines, reason = "one curl request, opened to closed")]
pub fn send(request: &Request) -> Result<Response, FetchError> {
   let url = request.url.as_str();
   let fail = |message: String| FetchError::Platform {
      url: url.to_owned(),
      message,
   };
   let not_http = || FetchError::NotHttp {
      url: url.to_owned(),
   };
   let curl =
      curl().ok_or_else(|| fail(format!("{} is not available", LIBCURL.to_string_lossy())))?;
   let url_cstr = CString::new(url).map_err(|_| not_http())?;
   let method = CString::new(request.method.as_str())
      .map_err(|_| fail("the method has a NUL byte".to_owned()))?;

   let mut headers = Headers(curl, ptr::null_mut());
   let mut lines = Vec::with_capacity(request.headers.len() + 2);
   for (name, value) in &request.headers {
      // `Name:` with nothing after it tells curl to drop the header; `Name;`
      // sends it empty, as WinHTTP does.
      lines.push(if value.is_empty() {
         format!("{name};")
      } else {
         format!("{name}: {value}")
      });
   }
   // Keep curl from adding a form content type to bodies and from waiting
   // on `100-continue`.
   if !request
      .headers
      .iter()
      .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
   {
      lines.push("Content-Type:".to_owned());
   }
   lines.push("Expect:".to_owned());
   for line in lines {
      let line = CString::new(line).map_err(|_| fail("bad header".to_owned()))?;
      // SAFETY: curl copies `line`; `headers.1` is null or its own list.
      let next = unsafe { (curl.slist_append)(headers.1, line.as_ptr()) };
      if next.is_null() {
         return Err(fail("curl_slist_append failed".to_owned()));
      }
      headers.1 = next;
   }

   // SAFETY: `curl_easy_init` has no preconditions after `curl_global_init`.
   let easy = Handle(curl, unsafe { (curl.easy_init)() });
   if easy.1.is_null() {
      return Err(fail("curl_easy_init failed".to_owned()));
   }
   let set_ptr = |option: c_int, value: *const c_void| {
      // SAFETY: `option` takes a pointer that outlives `curl_easy_perform`.
      unsafe { (curl.easy_setopt)(easy.1, option, value) }
   };
   let set_long = |option: c_int, value: c_long| {
      // SAFETY: `option` takes a `long`.
      unsafe { (curl.easy_setopt)(easy.1, option, value) }
   };

   let mut sink = Sink {
      body: Vec::new(),
      too_large: false,
   };
   let empty: &[u8] = &[];
   let is_get = request.method.eq_ignore_ascii_case("GET");
   let is_head = request.method.eq_ignore_ascii_case("HEAD");
   let is_post = request.method.eq_ignore_ascii_case("POST");
   let body = request.body.as_deref().unwrap_or(empty);
   let body_len = i64::try_from(body.len()).map_err(|_| FetchError::TooLarge {
      url: url.to_owned(),
   })?;

   let mut codes = vec![
      set_ptr(CURLOPT_URL, url_cstr.as_ptr().cast()),
      set_long(CURLOPT_PROTOCOLS, CURLPROTO_HTTP | CURLPROTO_HTTPS),
      // Never from https down to http, where session headers would go out
      // in the clear, as WinHTTP refuses too.
      set_long(CURLOPT_REDIR_PROTOCOLS, CURLPROTO_HTTPS),
      set_long(CURLOPT_FOLLOWLOCATION, 1),
      set_long(CURLOPT_MAXREDIRS, 10),
      set_long(CURLOPT_NOSIGNAL, 1),
      set_long(CURLOPT_CONNECTTIMEOUT_MS, c_long::from(TIMEOUT_MS)),
      // WinHTTP's timeouts are per stall, not per request; match that.
      set_long(CURLOPT_LOW_SPEED_LIMIT, 1),
      set_long(CURLOPT_LOW_SPEED_TIME, c_long::from(TIMEOUT_MS / 1000)),
      set_ptr(CURLOPT_USERAGENT, c"Ruby".as_ptr().cast()),
      set_ptr(CURLOPT_HTTPHEADER, headers.1.cast_const()),
      set_ptr(
         CURLOPT_WRITEFUNCTION,
         (write_body as WriteFn) as *const c_void,
      ),
      set_ptr(CURLOPT_WRITEDATA, (&raw mut sink).cast_const().cast()),
   ];
   if is_head {
      codes.push(set_long(CURLOPT_NOBODY, 1));
   } else if !is_get {
      // Always send a body, even an empty one, so PUT and DELETE carry
      // `Content-Length: 0` the way Discord's API wants.
      codes.push(set_long(CURLOPT_POSTFIELDSIZE_LARGE, body_len));
      codes.push(set_ptr(CURLOPT_POSTFIELDS, body.as_ptr().cast()));
   }
   if !is_get && !is_head && !is_post {
      codes.push(set_ptr(CURLOPT_CUSTOMREQUEST, method.as_ptr().cast()));
   }
   let curl_err = |code: c_int| {
      // SAFETY: `curl_easy_strerror` returns a static NUL-terminated string.
      let text = unsafe { CStr::from_ptr((curl.easy_strerror)(code)) };
      fail(format!("curl: {}", text.to_string_lossy()))
   };
   if let Some(&code) = codes.iter().find(|&&code| code != 0) {
      return Err(curl_err(code));
   }

   // SAFETY: every pointer set above lives until this function returns.
   let code = unsafe { (curl.easy_perform)(easy.1) };
   if sink.too_large {
      return Err(FetchError::TooLarge {
         url: url.to_owned(),
      });
   }
   match code {
      0 => {},
      CURLE_UNSUPPORTED_PROTOCOL | CURLE_URL_MALFORMAT => return Err(not_http()),
      _ => return Err(curl_err(code)),
   }

   let mut status: c_long = 0;
   // SAFETY: `CURLINFO_RESPONSE_CODE` writes one `long`.
   let code = unsafe { (curl.easy_getinfo)(easy.1, CURLINFO_RESPONSE_CODE, &raw mut status) };
   if code != 0 {
      return Err(curl_err(code));
   }
   Ok(Response {
      status: i32::try_from(status).unwrap_or(0),
      body: sink.body,
   })
}
