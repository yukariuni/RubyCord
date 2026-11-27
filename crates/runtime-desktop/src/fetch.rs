//! `send` for the platform's HTTP stack, with the OS's TLS certificates
//! instead of a bundled TLS library. `WinHTTP` follows the system proxy,
//! libcurl (the system's own on macOS) the `*_proxy` environment variables.

#[cfg(unix)]
mod curl;
#[cfg(windows)]
mod winhttp;

use phoenix_runtime::fetch::{FetchError, Request, Response};

pub fn send(request: &Request) -> Result<Response, FetchError> {
   // A line break would smuggle extra headers in. The value is never echoed,
   // it may be a token.
   if let Some((name, _)) = request.headers.iter().find(|(name, value)| {
      [name, value]
         .iter()
         .any(|part| part.contains(['\r', '\n', '\0']))
   }) {
      return Err(FetchError::Platform {
         url: request.url.clone(),
         message: format!("header {name} has a line break or NUL"),
      });
   }
   #[cfg(unix)]
   return curl::send(request);
   #[cfg(windows)]
   return winhttp::send(request);
}
