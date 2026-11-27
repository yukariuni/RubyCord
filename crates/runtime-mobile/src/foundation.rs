//! HTTP through Foundation's URL loading system, so requests follow the
//! system's certificates, proxy and VPN. `NSURLConnection`'s synchronous call
//! needs no completion block, which keeps this free of the block ABI.

use std::{
   ffi::{
      CStr,
      c_char,
      c_void,
   },
   mem::transmute,
   ptr,
   slice,
};

use phoenix_runtime::fetch::{
   FetchError,
   LIMIT,
   Request,
   Response,
   TIMEOUT_MS,
};

type Id = *mut c_void;
type Sel = *const c_void;

const UTF8: usize = 4;

#[link(name = "objc")]
unsafe extern "C" {
   fn objc_getClass(name: *const c_char) -> Id;
   fn sel_registerName(name: *const c_char) -> Sel;
   fn objc_msgSend();
   fn objc_autoreleasePoolPush() -> *mut c_void;
   fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

/// `objc_msgSend` cast to one method's signature, which arm64 requires since
/// it passes no argument count.
macro_rules! send {
   ($receiver:expr, $selector:literal $(, $arg:expr => $kind:ty)* ; $ret:ty) => {{
      // SAFETY: the signature matches the selector's Foundation declaration.
      let send = unsafe {
         transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(Id, Sel $(, $kind)*) -> $ret>(
            objc_msgSend,
         )
      };
      // SAFETY: a NUL-terminated selector name.
      let selector = unsafe { sel_registerName(concat!($selector, "\0").as_ptr().cast()) };
      // SAFETY: the receiver is a live object of a class that has the selector.
      unsafe { send($receiver, selector $(, $arg)*) }
   }};
}

fn class(name: &CStr) -> Id {
   // SAFETY: a NUL-terminated class name.
   unsafe { objc_getClass(name.as_ptr()) }
}

/// An autoreleased `NSString` of `text`.
fn string(text: &str) -> Id {
   let allocated = send!(class(c"NSString"), "alloc"; Id);
   let string = send!(
      allocated,
      "initWithBytes:length:encoding:",
      text.as_ptr() => *const u8,
      text.len() => usize,
      UTF8 => usize;
      Id
   );
   send!(string, "autorelease"; Id)
}

fn text(string: Id) -> String {
   if string.is_null() {
      return String::new();
   }
   let utf8 = send!(string, "UTF8String"; *const c_char);
   if utf8.is_null() {
      return String::new();
   }
   // SAFETY: `UTF8String` is NUL-terminated and lives as long as the pool.
   unsafe { CStr::from_ptr(utf8) }
      .to_string_lossy()
      .into_owned()
}

pub fn send(request: &Request) -> Result<Response, FetchError> {
   if !request.url.starts_with("https://") && !request.url.starts_with("http://") {
      return Err(FetchError::NotHttp {
         url: request.url.clone(),
      });
   }
   // SAFETY: popped below on the same thread.
   let pool = unsafe { objc_autoreleasePoolPush() };
   let result = exchange(request);
   // SAFETY: the pool pushed above.
   unsafe { objc_autoreleasePoolPop(pool) };
   result
}

fn exchange(request: &Request) -> Result<Response, FetchError> {
   let fail = |message: String| {
      FetchError::Platform {
         url: request.url.clone(),
         message,
      }
   };

   let url = send!(class(c"NSURL"), "URLWithString:", string(&request.url) => Id; Id);
   if url.is_null() {
      return Err(fail("not a URL Foundation accepts".to_owned()));
   }
   let call = send!(class(c"NSMutableURLRequest"), "requestWithURL:", url => Id; Id);
   send!(call, "setHTTPMethod:", string(&request.method) => Id; ());
   send!(call, "setTimeoutInterval:", f64::from(TIMEOUT_MS) / 1000.0 => f64; ());
   for (name, value) in &request.headers {
      send!(
         call,
         "setValue:forHTTPHeaderField:",
         string(value) => Id,
         string(name) => Id;
         ()
      );
   }
   if let Some(body) = &request.body {
      let data = send!(
         class(c"NSData"),
         "dataWithBytes:length:",
         body.as_ptr() => *const u8,
         body.len() => usize;
         Id
      );
      send!(call, "setHTTPBody:", data => Id; ());
   }

   let mut response: Id = ptr::null_mut();
   let mut error: Id = ptr::null_mut();
   let data = send!(
      class(c"NSURLConnection"),
      "sendSynchronousRequest:returningResponse:error:",
      call => Id,
      &raw mut response => *mut Id,
      &raw mut error => *mut Id;
      Id
   );
   if data.is_null() || response.is_null() {
      let message = if error.is_null() {
         "no response".to_owned()
      } else {
         text(send!(error, "localizedDescription"; Id))
      };
      return Err(fail(message));
   }

   let status = send!(response, "statusCode"; isize);
   let status = i32::try_from(status).map_err(|_| fail(format!("HTTP status {status}")))?;
   let length = send!(data, "length"; usize);
   if length > LIMIT {
      return Err(FetchError::TooLarge {
         url: request.url.clone(),
      });
   }
   let bytes = send!(data, "bytes"; *const u8);
   let body = if length == 0 || bytes.is_null() {
      Vec::new()
   } else {
      // SAFETY: `NSData` holds `length` bytes at `bytes` while the pool lives.
      unsafe { slice::from_raw_parts(bytes, length) }.to_vec()
   };
   Ok(Response { status, body })
}
