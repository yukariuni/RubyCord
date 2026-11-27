//! HTTP through the platform's own stack, so requests follow the system's
//! certificates, proxy and VPN, and phoenix ships no TLS code.

use std::{
   ffi::CString,
   ptr,
};

use jni_sys::{
   JNIEnv,
   jclass,
   jobject,
   jvalue,
};
use phoenix_runtime::fetch::{
   FetchError,
   LIMIT,
   Request,
   Response,
   TIMEOUT_MS,
};

use crate::jvm::{
   Class,
   JniError,
   STRING,
   class,
   env,
   jni,
   method,
   new_bytes,
   read_bytes,
   signature,
   take_exception,
};

const URL: Class = class!(java::net::URL);
const HTTP_URL_CONNECTION: Class = class!(java::net::HttpURLConnection);
const URL_CONNECTION: Class = class!(java::net::URLConnection);
const INPUT_STREAM: Class = class!(java::io::InputStream);
const OUTPUT_STREAM: Class = class!(java::io::OutputStream);

const CHUNK: i32 = 64 << 10;

/// Runs on a worker thread. The local frame frees every reference this
/// creates, since the thread never returns to Java to free them.
pub fn send(request: &Request) -> Result<Response, FetchError> {
   let env = env().ok_or(JniError::NoVm)?;
   // SAFETY: `env` belongs to this thread, and every call below passes
   // references made on it with the argument types its signature names.
   unsafe {
      if jni!(env, PushLocalFrame, 32) != 0 {
         return Err(java_error(env, &request.url));
      }
      let result = exchange(env, request);
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv` inside a local frame.
unsafe fn exchange(env: *mut JNIEnv, request: &Request) -> Result<Response, FetchError> {
   let url = request.url.as_str();
   // SAFETY: forwarded from the caller.
   unsafe {
      let (connection, http_class) = connect(env, request)?;
      let code = method(env, http_class, c"getResponseCode", signature!(() -> int))?;
      let status = jni!(env, CallIntMethodA, connection, code, ptr::null());
      if jni!(env, ExceptionCheck) {
         return Err(java_error(env, url));
      }

      // Error answers carry their body on a separate stream, which is null
      // when there is none.
      let name = if status >= 400 {
         c"getErrorStream"
      } else {
         c"getInputStream"
      };
      let input = method(env, http_class, name, signature!(() -> INPUT_STREAM))?;
      let stream = jni!(env, CallObjectMethodA, connection, input, ptr::null());
      if jni!(env, ExceptionCheck) {
         return Err(java_error(env, url));
      }
      let body = if stream.is_null() {
         Ok(Vec::new())
      } else {
         read_all(env, stream, url)
      };

      let disconnect = method(env, http_class, c"disconnect", signature!(() -> void))?;
      jni!(env, CallVoidMethodA, connection, disconnect, ptr::null());
      jni!(env, ExceptionClear);
      Ok(Response {
         status,
         body: body?,
      })
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv` inside a local frame, and `stream`
/// a live `InputStream`.
unsafe fn read_all(env: *mut JNIEnv, stream: jobject, url: &str) -> Result<Vec<u8>, FetchError> {
   // SAFETY: forwarded from the caller.
   unsafe {
      let stream_class = jni!(env, GetObjectClass, stream);
      let read = method(env, stream_class, c"read", signature!(([byte]) -> int))?;
      let buffer = jni!(env, NewByteArray, CHUNK);
      let mut body = Vec::new();
      let outcome = loop {
         let count = jni!(
            env,
            CallIntMethodA,
            stream,
            read,
            [jvalue { l: buffer }].as_ptr()
         );
         if jni!(env, ExceptionCheck) {
            break Err(java_error(env, url));
         }
         if count < 0 {
            break Ok(());
         }
         body.extend(read_bytes(env, buffer, Some(count)));
         if body.len() > LIMIT {
            break Err(FetchError::TooLarge {
               url: url.to_owned(),
            });
         }
      };
      let close = method(env, stream_class, c"close", signature!(() -> void))?;
      jni!(env, CallVoidMethodA, stream, close, ptr::null());
      jni!(env, ExceptionClear);
      outcome.map(|()| body)
   }
}

/// An `HttpURLConnection` with the request's method, headers and body sent,
/// and its class.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv` inside a local frame.
unsafe fn connect(env: *mut JNIEnv, request: &Request) -> Result<(jobject, jclass), FetchError> {
   let url = request.url.as_str();
   let not_http = || {
      FetchError::NotHttp {
         url: url.to_owned(),
      }
   };
   // SAFETY: forwarded from the caller.
   unsafe {
      let string = |text: &str| -> Result<jobject, FetchError> {
         let text = CString::new(text).map_err(|_| not_http())?;
         Ok(jni!(env, NewStringUTF, text.as_ptr()))
      };
      let url_class = URL.find(env)?;
      let http_class = HTTP_URL_CONNECTION.find(env)?;

      let init = method(env, url_class, c"<init>", signature!((STRING) -> void))?;
      let url_object = jni!(
         env,
         NewObjectA,
         url_class,
         init,
         [jvalue { l: string(url)? }].as_ptr()
      );
      if url_object.is_null() {
         return Err(java_error(env, url));
      }
      let open = method(
         env,
         url_class,
         c"openConnection",
         signature!(() -> URL_CONNECTION),
      )?;
      let connection = jni!(env, CallObjectMethodA, url_object, open, ptr::null());
      if connection.is_null() {
         return Err(java_error(env, url));
      }
      if !jni!(env, IsInstanceOf, connection, http_class) {
         return Err(not_http());
      }

      for name in [c"setConnectTimeout", c"setReadTimeout"] {
         let set = method(env, http_class, name, signature!((int) -> void))?;
         jni!(
            env,
            CallVoidMethodA,
            connection,
            set,
            [jvalue { i: TIMEOUT_MS }].as_ptr()
         );
      }
      let set_method = method(
         env,
         http_class,
         c"setRequestMethod",
         signature!((STRING) -> void),
      )?;
      let verb = string(&request.method)?;
      jni!(
         env,
         CallVoidMethodA,
         connection,
         set_method,
         [jvalue { l: verb }].as_ptr()
      );
      let set_header = method(
         env,
         http_class,
         c"setRequestProperty",
         signature!((STRING, STRING) -> void),
      )?;
      for (name, value) in &request.headers {
         let args = [jvalue { l: string(name)? }, jvalue { l: string(value)? }];
         jni!(env, CallVoidMethodA, connection, set_header, args.as_ptr());
      }
      if jni!(env, ExceptionCheck) {
         return Err(java_error(env, url));
      }

      if let Some(body) = &request.body {
         send_body(env, connection, http_class, body, url)?;
      }
      Ok((connection, http_class))
   }
}

/// Writes `body` to the connection's output stream.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv` inside a local frame, and
/// `connection` a live `HttpURLConnection` of `http_class`.
unsafe fn send_body(
   env: *mut JNIEnv,
   connection: jobject,
   http_class: jclass,
   body: &[u8],
   url: &str,
) -> Result<(), FetchError> {
   // SAFETY: forwarded from the caller.
   unsafe {
      let set_output = method(
         env,
         http_class,
         c"setDoOutput",
         signature!((boolean) -> void),
      )?;
      jni!(
         env,
         CallVoidMethodA,
         connection,
         set_output,
         [jvalue { z: true }].as_ptr()
      );
      let output = method(
         env,
         http_class,
         c"getOutputStream",
         signature!(() -> OUTPUT_STREAM),
      )?;
      let stream = jni!(env, CallObjectMethodA, connection, output, ptr::null());
      if stream.is_null() {
         return Err(java_error(env, url));
      }
      let Some((array, _)) = new_bytes(env, body) else {
         return Err(FetchError::TooLarge {
            url: url.to_owned(),
         });
      };
      let stream_class = jni!(env, GetObjectClass, stream);
      let write = method(env, stream_class, c"write", signature!(([byte]) -> void))?;
      let close = method(env, stream_class, c"close", signature!(() -> void))?;
      jni!(
         env,
         CallVoidMethodA,
         stream,
         write,
         [jvalue { l: array }].as_ptr()
      );
      jni!(env, CallVoidMethodA, stream, close, ptr::null());
      if jni!(env, ExceptionCheck) {
         return Err(java_error(env, url));
      }
   }
   Ok(())
}

/// The pending exception as `Throwable.toString()`, cleared.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
unsafe fn java_error(env: *mut JNIEnv, url: &str) -> FetchError {
   // SAFETY: forwarded from the caller.
   let message = unsafe { take_exception(env) };
   FetchError::Platform {
      url:     url.to_owned(),
      message: message.unwrap_or_else(|| "a Java call failed".to_owned()),
   }
}
