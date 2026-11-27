//! HTTP through Windows' native `WinHTTP` stack (`winhttp.dll`), using the
//! system's TLS certificates, proxy, and VPN without bundling TLS libraries.

use std::{ffi::c_void, ptr};

use phoenix_runtime::fetch::{FetchError, LIMIT, Request, Response, TIMEOUT_MS};

const WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const WINHTTP_ADDREQ_FLAG_ADD: u32 = 0x2000_0000;
const WINHTTP_ADDREQ_FLAG_REPLACE: u32 = 0x8000_0000;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;
const INTERNET_SCHEME_HTTP: i32 = 1;
const INTERNET_SCHEME_HTTPS: i32 = 2;

#[repr(C)]
struct UrlComponents {
   struct_size: u32,
   scheme: *mut u16,
   scheme_len: u32,
   scheme_kind: i32,
   host_name: *mut u16,
   host_name_len: u32,
   port: u16,
   user_name: *mut u16,
   user_name_len: u32,
   password: *mut u16,
   password_len: u32,
   url_path: *mut u16,
   url_path_len: u32,
   extra_info: *mut u16,
   extra_info_len: u32,
}

#[link(name = "winhttp")]
unsafe extern "system" {
   fn WinHttpOpen(
      agent: *const u16,
      access_type: u32,
      proxy: *const u16,
      proxy_bypass: *const u16,
      flags: u32,
   ) -> *mut c_void;
   fn WinHttpCloseHandle(handle: *mut c_void) -> i32;
   fn WinHttpSetTimeouts(
      handle: *mut c_void,
      resolve: i32,
      connect: i32,
      send: i32,
      receive: i32,
   ) -> i32;
   fn WinHttpCrackUrl(
      url: *const u16,
      url_len: u32,
      flags: u32,
      components: *mut UrlComponents,
   ) -> i32;
   fn WinHttpConnect(
      session: *mut c_void,
      server_name: *const u16,
      port: u16,
      reserved: u32,
   ) -> *mut c_void;
   fn WinHttpOpenRequest(
      connect: *mut c_void,
      verb: *const u16,
      object_name: *const u16,
      version: *const u16,
      referrer: *const u16,
      accept_types: *const *const u16,
      flags: u32,
   ) -> *mut c_void;
   fn WinHttpAddRequestHeaders(
      request: *mut c_void,
      headers: *const u16,
      headers_len: u32,
      modifiers: u32,
   ) -> i32;
   fn WinHttpSendRequest(
      request: *mut c_void,
      headers: *const u16,
      headers_len: u32,
      optional: *const c_void,
      optional_len: u32,
      total_len: u32,
      context: usize,
   ) -> i32;
   fn WinHttpReceiveResponse(request: *mut c_void, reserved: *mut c_void) -> i32;
   fn WinHttpQueryHeaders(
      request: *mut c_void,
      info_level: u32,
      name: *const u16,
      buffer: *mut c_void,
      buffer_len: *mut u32,
      index: *mut u32,
   ) -> i32;
   fn WinHttpQueryDataAvailable(request: *mut c_void, available: *mut u32) -> i32;
   fn WinHttpReadData(
      request: *mut c_void,
      buffer: *mut c_void,
      to_read: u32,
      read: *mut u32,
   ) -> i32;
}

struct Handle(*mut c_void);

impl Drop for Handle {
   fn drop(&mut self) {
      if !self.0.is_null() {
         // SAFETY: `self.0` is a valid WinHTTP handle owned by this guard.
         unsafe { WinHttpCloseHandle(self.0) };
      }
   }
}

fn wide(text: &str) -> Vec<u16> {
   text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn os_err(url: &str, context: &str) -> FetchError {
   FetchError::Platform {
      url: url.to_owned(),
      message: format!("{context}: {}", std::io::Error::last_os_error()),
   }
}

#[expect(
   clippy::too_many_lines,
   reason = "one WinHTTP request, opened to closed"
)]
pub fn send(request: &Request) -> Result<Response, FetchError> {
   let url = request.url.as_str();
   let wide_url = wide(url);
   let mut host_buf = vec![0u16; 256];
   let mut path_buf = vec![0u16; wide_url.len().max(256)];
   let mut extra_buf = vec![0u16; wide_url.len().max(256)];

   let mut parts = UrlComponents {
      struct_size: u32::try_from(size_of::<UrlComponents>()).unwrap_or(0),
      scheme: ptr::null_mut(),
      scheme_len: 0,
      scheme_kind: 0,
      host_name: host_buf.as_mut_ptr(),
      host_name_len: u32::try_from(host_buf.len()).unwrap_or(0),
      port: 0,
      user_name: ptr::null_mut(),
      user_name_len: 0,
      password: ptr::null_mut(),
      password_len: 0,
      url_path: path_buf.as_mut_ptr(),
      url_path_len: u32::try_from(path_buf.len()).unwrap_or(0),
      extra_info: extra_buf.as_mut_ptr(),
      extra_info_len: u32::try_from(extra_buf.len()).unwrap_or(0),
   };

   // SAFETY: `wide_url` is NUL-terminated and all output buffers are valid.
   if unsafe { WinHttpCrackUrl(wide_url.as_ptr(), 0, 0, &raw mut parts) } == 0 {
      return Err(FetchError::NotHttp {
         url: url.to_owned(),
      });
   }
   let secure = match parts.scheme_kind {
      INTERNET_SCHEME_HTTPS => true,
      INTERNET_SCHEME_HTTP => false,
      _ => {
         return Err(FetchError::NotHttp {
            url: url.to_owned(),
         });
      },
   };
   host_buf.truncate(parts.host_name_len as usize);
   host_buf.push(0);

   let mut full_path =
      Vec::with_capacity(parts.url_path_len as usize + parts.extra_info_len as usize + 2);
   if parts.url_path_len == 0 {
      full_path.push(u16::from(b'/'));
   } else {
      full_path.extend_from_slice(&path_buf[..parts.url_path_len as usize]);
   }
   full_path.extend_from_slice(&extra_buf[..parts.extra_info_len as usize]);
   full_path.push(0);

   let agent = wide("RubyCord");
   // SAFETY: WinHTTP session lifecycle managed by RAII `Handle`.
   let session = Handle(unsafe {
      WinHttpOpen(
         agent.as_ptr(),
         WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
         ptr::null(),
         ptr::null(),
         0,
      )
   });
   if session.0.is_null() {
      return Err(os_err(url, "WinHttpOpen"));
   }
   // SAFETY: `session.0` is a live session handle.
   unsafe {
      WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS);
   }

   // SAFETY: `host_buf` is NUL-terminated UTF-16.
   let conn = Handle(unsafe { WinHttpConnect(session.0, host_buf.as_ptr(), parts.port, 0) });
   if conn.0.is_null() {
      return Err(os_err(url, "WinHttpConnect"));
   }

   let verb = wide(&request.method);
   let flags = if secure { WINHTTP_FLAG_SECURE } else { 0 };
   // SAFETY: `verb` and `full_path` are NUL-terminated UTF-16.
   let req = Handle(unsafe {
      WinHttpOpenRequest(
         conn.0,
         verb.as_ptr(),
         full_path.as_ptr(),
         ptr::null(),
         ptr::null(),
         ptr::null(),
         flags,
      )
   });
   if req.0.is_null() {
      return Err(os_err(url, "WinHttpOpenRequest"));
   }

   for (name, value) in &request.headers {
      let line = wide(&format!("{name}: {value}\r\n"));
      let len = u32::try_from(line.len().saturating_sub(1)).unwrap_or(0);
      // SAFETY: `req.0` is valid and `line` has `len` UTF-16 code units.
      if unsafe {
         WinHttpAddRequestHeaders(
            req.0,
            line.as_ptr(),
            len,
            WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
         )
      } == 0
      {
         return Err(os_err(url, "WinHttpAddRequestHeaders"));
      }
   }

   let (body_ptr, body_len) = match &request.body {
      Some(bytes) => {
         let len = u32::try_from(bytes.len()).map_err(|_| FetchError::TooLarge {
            url: url.to_owned(),
         })?;
         (bytes.as_ptr().cast::<c_void>(), len)
      },
      None => (ptr::null(), 0),
   };
   // SAFETY: `body_ptr` is valid for `body_len` bytes when non-null.
   if unsafe { WinHttpSendRequest(req.0, ptr::null(), 0, body_ptr, body_len, body_len, 0) } == 0 {
      return Err(os_err(url, "WinHttpSendRequest"));
   }
   // SAFETY: `req.0` has a sent request.
   if unsafe { WinHttpReceiveResponse(req.0, ptr::null_mut()) } == 0 {
      return Err(os_err(url, "WinHttpReceiveResponse"));
   }

   let mut status = 0u32;
   let mut status_size = u32::try_from(size_of::<u32>()).unwrap_or(4);
   // SAFETY: `status` is 4 bytes matching `status_size`.
   if unsafe {
      WinHttpQueryHeaders(
         req.0,
         WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
         ptr::null(),
         (&raw mut status).cast(),
         &raw mut status_size,
         ptr::null_mut(),
      )
   } == 0
   {
      return Err(os_err(url, "WinHttpQueryHeaders"));
   }

   let mut body = Vec::new();
   loop {
      let mut available = 0u32;
      // SAFETY: `req.0` has a received response.
      if unsafe { WinHttpQueryDataAvailable(req.0, &raw mut available) } == 0 {
         return Err(os_err(url, "WinHttpQueryDataAvailable"));
      }
      if available == 0 {
         break;
      }
      let chunk_len = (available as usize).min(64 << 10);
      let start = body.len();
      if start.saturating_add(chunk_len) > LIMIT {
         return Err(FetchError::TooLarge {
            url: url.to_owned(),
         });
      }
      body.resize(start + chunk_len, 0);
      let mut read = 0u32;
      // SAFETY: `body[start..]` has `chunk_len` writable bytes.
      if unsafe {
         WinHttpReadData(
            req.0,
            body[start..].as_mut_ptr().cast(),
            u32::try_from(chunk_len).unwrap_or(64 << 10),
            &raw mut read,
         )
      } == 0
      {
         return Err(os_err(url, "WinHttpReadData"));
      }
      body.truncate(start + read as usize);
      if read == 0 {
         break;
      }
   }

   Ok(Response {
      status: i32::try_from(status).unwrap_or(0),
      body,
   })
}
