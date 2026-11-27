//! Opens Android's installer on an APK in the app's `files` directory, shared
//! through the `FileProvider` Discord declares at `<package>.file-provider`,
//! a name the patcher's package rename keeps.

use std::{
   ffi::CString,
   path::{
      Path,
      PathBuf,
   },
   ptr,
};

use jni_sys::{
   JNIEnv,
   jclass,
   jint,
   jmethodID,
   jobject,
   jvalue,
};

use crate::{
   bridge,
   jvm::{
      self,
      CLASS,
      Class,
      STRING,
      class,
      jni,
      method,
      signature,
   },
};

const ACTIVITY_THREAD: Class = class!(android::app::ActivityThread);
const APPLICATION: Class = class!(android::app::Application);
const CONTEXT: Class = class!(android::content::Context);
const INTENT: Class = class!(android::content::Intent);
const URI: Class = class!(android::net::Uri);
const FILE: Class = class!(java::io::File);
const FILE_PROVIDER: Class = class!(androidx::core::content::FileProvider);
const METHOD: Class = class!(java::lang::reflect::Method);

const FLAG_GRANT_READ_URI_PERMISSION: i32 = 0x0000_0001;
const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;

/// The app's real `files` directory from `Application.getFilesDir()`, which
/// matches Android's `FileProvider` `<files-path>` root on both internal
/// storage and adopted SD cards (`/mnt/expand/<uuid>/user/<uid>/<pkg>/files`).
pub fn files_dir() -> Option<PathBuf> {
   let env = jvm::env()?;
   // SAFETY: `env` is this thread's, and every local ref is popped below.
   unsafe {
      if jni!(env, PushLocalFrame, 16) != 0 {
         return None;
      }
      let result = application(env).and_then(|app| {
         let app_class = jni!(env, GetObjectClass, app);
         let get_files = method(env, app_class, c"getFilesDir", signature!(() -> FILE))?;
         let dir = checked(
            env,
            jni!(env, CallObjectMethodA, app, get_files, ptr::null()),
            "no files dir",
         )?;
         let file_class = FILE.find(env)?;
         let get_path = method(
            env,
            file_class,
            c"getAbsolutePath",
            signature!(() -> STRING),
         )?;
         let path = checked(
            env,
            jni!(env, CallObjectMethodA, dir, get_path, ptr::null()),
            "no files dir path",
         )?;
         jvm::read_string(env, path)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "empty files dir path".to_owned())
      });
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
         .inspect_err(|err| log::warn!("install: getFilesDir failed: {err}"))
         .ok()
   }
}

pub fn open(apk: &Path) -> Result<(), String> {
   let env = jvm::env().ok_or("no JNI env")?;
   let apk = CString::new(apk.to_str().ok_or("the update path is not UTF-8")?)
      .map_err(|err| err.to_string())?;
   // SAFETY: `env` is this thread's, and every reference made here is local
   // to the frame popped below.
   unsafe {
      if jni!(env, PushLocalFrame, 32) != 0 {
         return Err(failure(env, "out of local references"));
      }
      let result = application(env).and_then(|app| {
         let uri = content_uri(env, app, &apk)?;
         start_view(env, app, uri)
      });
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// The app's `Application`.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`, inside a local frame.
unsafe fn application(env: *mut JNIEnv) -> Result<jobject, String> {
   // SAFETY: forwarded from `open`.
   unsafe {
      let thread = ACTIVITY_THREAD.find(env)?;
      let current = jvm::static_method(
         env,
         thread,
         c"currentApplication",
         signature!(() -> APPLICATION),
      )?;
      checked(
         env,
         jni!(env, CallStaticObjectMethodA, thread, current, ptr::null()),
         "no Application",
      )
   }
}

/// A `content://` URI for `apk` that the installer may read.
///
/// # Safety
///
/// As [`application`], with `app` the `Application`.
unsafe fn content_uri(env: *mut JNIEnv, app: jobject, apk: &CString) -> Result<jobject, String> {
   // SAFETY: forwarded from `open`. Each call's exception is checked before
   // its result is used.
   unsafe {
      let app_class = jni!(env, GetObjectClass, app);
      let package_name = method(env, app_class, c"getPackageName", signature!(() -> STRING))?;
      let package = checked(
         env,
         jni!(env, CallObjectMethodA, app, package_name, ptr::null()),
         "no package name",
      )?;
      let package = jvm::read_string(env, package).ok_or("no package name")?;
      let authority =
         CString::new(format!("{package}.file-provider")).map_err(|err| err.to_string())?;

      let file_class = FILE.find(env)?;
      let file_init = method(env, file_class, c"<init>", signature!((STRING) -> void))?;
      let file = checked(
         env,
         jni!(
            env,
            NewObjectA,
            file_class,
            file_init,
            [jvalue {
               l: jni!(env, NewStringUTF, apk.as_ptr()),
            }]
            .as_ptr()
         ),
         "could not make a File",
      )?;

      let provider = bridge::jni::app_class(env, FILE_PROVIDER)
         .ok_or("Discord has no androidx FileProvider")?;
      let uri_for_file = method_by_types(env, provider, &[CONTEXT, STRING, FILE], URI)?;
      checked(
         env,
         jni!(
            env,
            CallStaticObjectMethodA,
            provider,
            uri_for_file,
            [
               jvalue { l: app },
               jvalue {
                  l: jni!(env, NewStringUTF, authority.as_ptr()),
               },
               jvalue { l: file },
            ]
            .as_ptr()
         ),
         "the FileProvider does not share the update",
      )
   }
}

/// The static method of `class` that takes `params` and returns `returns`.
/// Discord's build renames `FileProvider.getUriForFile`, so it is found by
/// its types.
///
/// # Safety
///
/// As [`content_uri`], with `class` a live class reference.
unsafe fn method_by_types(
   env: *mut JNIEnv,
   class: jclass,
   params: &[Class],
   returns: Class,
) -> Result<jmethodID, String> {
   const STATIC: jint = 0x0008;

   // SAFETY: forwarded from `open`. Every reflective call is on a `Method` the
   // class just listed, with no arguments or one it accepts.
   unsafe {
      let declared = method(
         env,
         CLASS.find(env)?,
         c"getDeclaredMethods",
         signature!(() -> [METHOD]),
      )?;
      let methods = checked(
         env,
         jni!(env, CallObjectMethodA, class, declared, ptr::null()),
         "could not list the FileProvider's methods",
      )?;
      let method_class = METHOD.find(env)?;
      let modifiers = method(env, method_class, c"getModifiers", signature!(() -> int))?;
      let return_type = method(env, method_class, c"getReturnType", signature!(() -> CLASS))?;
      let param_types = method(
         env,
         method_class,
         c"getParameterTypes",
         signature!(() -> [CLASS]),
      )?;
      let wanted = params
         .iter()
         .map(|param| param.find(env))
         .collect::<Result<Vec<_>, _>>()?;
      let returns = returns.find(env)?;

      for index in 0..jni!(env, GetArrayLength, methods) {
         let candidate = jni!(env, GetObjectArrayElement, methods, index);
         let is_static = jni!(env, CallIntMethodA, candidate, modifiers, ptr::null()) & STATIC != 0;
         let returned = jni!(env, CallObjectMethodA, candidate, return_type, ptr::null());
         let types = jni!(env, CallObjectMethodA, candidate, param_types, ptr::null());
         if jni!(env, ExceptionCheck) {
            return Err(failure(env, "could not read a FileProvider method"));
         }
         let matches = is_static
            && jni!(env, IsSameObject, returned, returns)
            && usize::try_from(jni!(env, GetArrayLength, types)) == Ok(wanted.len())
            && wanted.iter().zip(0..).all(|(&param, at)| {
               jni!(
                  env,
                  IsSameObject,
                  jni!(env, GetObjectArrayElement, types, at),
                  param
               )
            });
         if matches {
            return Ok(jni!(env, FromReflectedMethod, candidate));
         }
         for local in [candidate, returned, types] {
            jni!(env, DeleteLocalRef, local);
         }
      }
      Err("Discord's FileProvider has no getUriForFile".to_owned())
   }
}

/// Starts the installer's activity on `uri`.
///
/// # Safety
///
/// As [`content_uri`], with `uri` a `content://` URI.
unsafe fn start_view(env: *mut JNIEnv, app: jobject, uri: jobject) -> Result<(), String> {
   // SAFETY: forwarded from `open`.
   unsafe {
      let intent_class = INTENT.find(env)?;
      let intent_init = method(env, intent_class, c"<init>", signature!((STRING) -> void))?;
      let intent = checked(
         env,
         jni!(
            env,
            NewObjectA,
            intent_class,
            intent_init,
            [jvalue {
               l: jni!(env, NewStringUTF, c"android.intent.action.VIEW".as_ptr()),
            }]
            .as_ptr()
         ),
         "could not make an Intent",
      )?;
      let set_data = method(
         env,
         intent_class,
         c"setDataAndType",
         signature!((URI, STRING) -> INTENT),
      )?;
      let kind = jni!(
         env,
         NewStringUTF,
         c"application/vnd.android.package-archive".as_ptr()
      );
      jni!(
         env,
         CallObjectMethodA,
         intent,
         set_data,
         [jvalue { l: uri }, jvalue { l: kind }].as_ptr()
      );
      let add_flags = method(env, intent_class, c"addFlags", signature!((int) -> INTENT))?;
      jni!(
         env,
         CallObjectMethodA,
         intent,
         add_flags,
         [jvalue {
            i: FLAG_GRANT_READ_URI_PERMISSION | FLAG_ACTIVITY_NEW_TASK,
         }]
         .as_ptr()
      );
      let start = method(
         env,
         jni!(env, GetObjectClass, app),
         c"startActivity",
         signature!((INTENT) -> void),
      )?;
      jni!(
         env,
         CallVoidMethodA,
         app,
         start,
         [jvalue { l: intent }].as_ptr()
      );
      if jni!(env, ExceptionCheck) {
         return Err(failure(env, "the installer did not open"));
      }
      Ok(())
   }
}

/// `object` unless the call that made it threw or returned null.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
unsafe fn checked(env: *mut JNIEnv, object: jobject, what: &str) -> Result<jobject, String> {
   // SAFETY: forwarded from the caller.
   unsafe {
      if jni!(env, ExceptionCheck) || object.is_null() {
         return Err(failure(env, what));
      }
   }
   Ok(object)
}

/// The pending exception's text, or `what` when there is none.
unsafe fn failure(env: *mut JNIEnv, what: &str) -> String {
   // SAFETY: forwarded from the caller.
   unsafe { jvm::take_exception(env) }
      .map_or_else(|| what.to_owned(), |thrown| format!("{what}: {thrown}"))
}
