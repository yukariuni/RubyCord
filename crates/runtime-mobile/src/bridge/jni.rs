//! The Android side of the bridge.
//!
//! - Rust to Java goes through ART's process-wide JNI function table.
//!   `CallVoidMethodA` and `CallObjectMethodA` are swapped for filters that
//!   catch `GuestHost.applyOps([BI)V` and `GuestHost.invoke([B)[B`. `invoke`
//!   treats a frame starting with 4 zero bytes as a UI-op batch, which is how
//!   almost every batch travels.
//! - Java to Rust goes through `WasmRuntime.nativeDispatch(J[B)Z`, re-bound
//!   with `RegisterNatives` to a filter around the library's own export.
//!
//! Everything here calls JNI through the saved original table, so the filters
//! never see phoenix's own calls.

use std::{
   ffi::c_void,
   mem::offset_of,
   ptr,
   sync::{
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         AtomicPtr,
         Ordering,
      },
   },
};

use jni_sys::{
   _jmethodID,
   _jobject,
   JNI_FALSE,
   JNI_OK,
   JNI_TRUE,
   JNI_VERSION_1_6,
   JNIEnv,
   JNINativeInterface__1_6,
   JNINativeMethod,
   jboolean,
   jbyteArray,
   jclass,
   jlong,
   jmethodID,
   jobject,
   jvalue,
};
use phoenix_hook::image::swap_words;

use crate::{
   bridge::{
      self,
      Action,
   },
   jvm::{
      CLASS,
      CLASS_LOADER,
      Class,
      SAVED,
      STRING,
      Table,
      class,
      clear_exception,
      env,
      jni,
      new_bytes,
      read_bytes,
      read_string,
      signature,
      table,
      vm,
   },
};

const FUNCTION0: Class = class!(kotlin::jvm::functions::Function0);
const WASM_RUNTIME: Class = class!(com::discord::nativeapp::bridge::WasmRuntime);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
   Other,
   ApplyOps,
   Invoke,
}

type DispatchFn = unsafe extern "system" fn(*mut JNIEnv, jclass, jlong, jbyteArray) -> jboolean;

static PATCH_LOCK: Mutex<()> = Mutex::new(());
static METHODS: Mutex<Vec<(usize, Method)>> = Mutex::new(Vec::new());
static GUEST_HOST: AtomicPtr<_jobject> = AtomicPtr::new(ptr::null_mut());
static INVOKE_METHOD: AtomicPtr<_jmethodID> = AtomicPtr::new(ptr::null_mut());
static NATIVES_BOUND: AtomicBool = AtomicBool::new(false);
static ORIGINAL_DISPATCH: OnceLock<DispatchFn> = OnceLock::new();
/// `GuestHost.d([B, Function0)`, which queues a view frame for the
/// `wasm-guest` thread. Rust drops a `nativeDispatch` from any other thread.
static ENQUEUE_METHOD: AtomicPtr<_jmethodID> = AtomicPtr::new(ptr::null_mut());
/// Sentinel passed through `GuestHost.d` to wake `wasm-guest` and drain
/// the deferred frames. Never matches a real view frame (`00 00 00 00 01
/// 01`).
const WAKE_SENTINEL: &[u8] = b"\0\0\0\0PHXW";

/// Patches ART's shared JNI table once. Cheap after the first call, and does
/// nothing on threads the VM has not attached.
pub fn attach() {
   if SAVED.get().is_some() {
      return;
   }
   let vm = vm();
   if vm.is_null() {
      return;
   }
   let mut raw_env: *mut c_void = ptr::null_mut();
   // SAFETY: `vm` is the process's live JavaVM, and GetEnv only writes
   // `raw_env`.
   if unsafe { ((**vm).v1_2.GetEnv)(vm, &raw mut raw_env, JNI_VERSION_1_6) } != JNI_OK
      || raw_env.is_null()
   {
      return;
   }
   let env = raw_env.cast::<JNIEnv>();

   let _guard = PATCH_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
   if SAVED.get().is_some() {
      return;
   }
   // SAFETY: GetEnv returned this thread's env, whose function table ART
   // shares across threads and keeps for the life of the process.
   let (functions, saved) = unsafe { (*env, (**env).v1_6) };
   if SAVED.set(Table(saved)).is_err() {
      return;
   }

   let base = functions as usize;
   let writes = [
      (
         base + offset_of!(JNINativeInterface__1_6, CallVoidMethodA),
         call_void_method_a as *const () as usize,
      ),
      (
         base + offset_of!(JNINativeInterface__1_6, CallObjectMethodA),
         call_object_method_a as *const () as usize,
      ),
   ];
   // SAFETY: both slots are aligned function pointers in ART's table. Each is
   // swapped atomically, and SAVED already holds the originals, so a thread
   // that reads either pointer makes a valid call.
   match unsafe { swap_words(&writes) } {
      Ok(_) => log::info!("bridge: JNI table patched"),
      Err(err) => log::warn!("bridge: JNI table patch failed: {err}"),
   }
}

/// Passes `frame` to `GuestHost.invoke`.
pub fn invoke(frame: &[u8]) -> bool {
   let guest_host = GUEST_HOST.load(Ordering::Acquire);
   if guest_host.is_null() {
      return false;
   }
   let Some(env) = env() else {
      return false;
   };

   // SAFETY: `env` belongs to this thread, `guest_host` is a global ref, and
   // the method is `GuestHost.invoke([B)[B`, which takes the one byte array.
   unsafe {
      let Some((array, _)) = new_bytes(env, frame) else {
         return false;
      };
      let method = INVOKE_METHOD.load(Ordering::Acquire);
      let result = jni!(
         env,
         CallObjectMethodA,
         guest_host,
         method,
         [jvalue { l: array }].as_ptr()
      );
      if !result.is_null() {
         jni!(env, DeleteLocalRef, result);
      }
      jni!(env, DeleteLocalRef, array);
      if clear_exception(env) {
         log::warn!("bridge: GuestHost.invoke threw on a phoenix frame");
         return false;
      }
   }
   true
}

/// Wakes `wasm-guest` to deliver the deferred frames.
pub fn wake() {
   let _ = inject(WAKE_SENTINEL);
}

/// Queues view `frame` through `GuestHost.d`.
pub fn inject(frame: &[u8]) -> bool {
   let guest_host = GUEST_HOST.load(Ordering::Acquire);
   let (Some(body), false) = (frame.strip_prefix(&[0; 4]), guest_host.is_null()) else {
      return false;
   };
   let Some(env) = env() else {
      return false;
   };
   // SAFETY: `env` belongs to this thread and `guest_host` is a global ref.
   // `d` takes the frame without its 4-byte header and a nullable callback.
   unsafe {
      let mut method = ENQUEUE_METHOD.load(Ordering::Acquire);
      if method.is_null() {
         let class = jni!(env, GetObjectClass, guest_host);
         method = jni!(
            env,
            GetMethodID,
            class,
            c"d".as_ptr(),
            signature!(([byte], FUNCTION0) -> void).as_ptr()
         );
         jni!(env, DeleteLocalRef, class);
         if clear_exception(env) || method.is_null() {
            log::warn!("bridge: GuestHost has no d([B, Function0)");
            return false;
         }
         ENQUEUE_METHOD.store(method, Ordering::Release);
      }
      let Some((array, _)) = new_bytes(env, body) else {
         return false;
      };
      jni!(
         env,
         CallVoidMethodA,
         guest_host,
         method,
         [jvalue { l: array }, jvalue { l: ptr::null_mut() }].as_ptr()
      );
      jni!(env, DeleteLocalRef, array);
      !clear_exception(env)
   }
}

/// Which `GuestHost` method `method` is, cached per method id.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`, and `obj` and `method` come from a
/// JNI call made on it.
unsafe fn method_kind(env: *mut JNIEnv, obj: jobject, method: jmethodID) -> Method {
   let key = method as usize;
   let cached = METHODS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .find(|(id, _)| *id == key)
      .map(|&(_, kind)| kind);
   if let Some(kind) = cached {
      return kind;
   }

   // SAFETY: forwarded from the caller. Each call checks the result it depends
   // on, and the local frame frees every reference made here.
   let kind = unsafe {
      if jni!(env, PushLocalFrame, 8) != JNI_OK {
         clear_exception(env);
         return Method::Other;
      }
      let resolve = || -> Option<Method> {
         let class = jni!(env, GetObjectClass, obj);
         let reflected = jni!(env, ToReflectedMethod, class, method, JNI_FALSE);
         if reflected.is_null() {
            return None;
         }
         let reflected_class = jni!(env, GetObjectClass, reflected);
         let get_name = c"getName".as_ptr();
         let signature = signature!(() -> STRING).as_ptr();
         let method_name = jni!(env, GetMethodID, reflected_class, get_name, signature);
         if method_name.is_null() {
            return None;
         }
         let name = read_string(
            env,
            jni!(env, CallObjectMethodA, reflected, method_name, ptr::null()),
         )?;
         let class_class = CLASS.find(env).ok()?;
         let class_name = jni!(env, GetMethodID, class_class, get_name, signature);
         if class_name.is_null() {
            return None;
         }
         let owner = read_string(
            env,
            jni!(env, CallObjectMethodA, class, class_name, ptr::null()),
         )?;
         if !owner.contains("nativeapp.bridge.GuestHost") {
            return Some(Method::Other);
         }
         Some(match name.as_str() {
            "applyOps" => Method::ApplyOps,
            "invoke" => Method::Invoke,
            _ => Method::Other,
         })
      };
      let kind = resolve().unwrap_or(Method::Other);
      clear_exception(env);
      jni!(env, PopLocalFrame, ptr::null_mut());
      kind
   };

   let mut methods = METHODS.lock().unwrap_or_else(PoisonError::into_inner);
   if methods.len() < 64 {
      methods.push((key, kind));
   }
   kind
}

unsafe extern "system" fn call_void_method_a(
   env: *mut JNIEnv,
   obj: jobject,
   method: jmethodID,
   args: *const jvalue,
) {
   let watch = bridge::watching();
   // SAFETY: ART calls this with the current thread's env and the caller's
   // arguments. `applyOps([BI)V` gets a byte array and an int, forwarded
   // unchanged or swapped for a replacement of the same shape.
   unsafe {
      let original = table(env).CallVoidMethodA;
      let kind = if watch || !NATIVES_BOUND.load(Ordering::Acquire) {
         method_kind(env, obj, method)
      } else {
         Method::Other
      };
      if kind != Method::Other {
         bind_natives(env, obj);
      }
      if !watch || kind != Method::ApplyOps {
         return original(env, obj, method, args);
      }

      let batch = read_bytes(env, (*args).l, Some((*args.add(1)).i));
      let (action, _guard) = bridge::ops(&batch);
      match action {
         Action::Drop => {},
         Action::Pass => original(env, obj, method, args),
         Action::Replace(replaced) => {
            let Some((array, len)) = new_bytes(env, &replaced) else {
               return original(env, obj, method, args);
            };
            let replaced_args = [jvalue { l: array }, jvalue { i: len }];
            original(env, obj, method, replaced_args.as_ptr());
            jni!(env, DeleteLocalRef, array);
         },
      }
   }
}

/// `invoke` returns null for UI-op batches.
unsafe extern "system" fn call_object_method_a(
   env: *mut JNIEnv,
   obj: jobject,
   method: jmethodID,
   args: *const jvalue,
) -> jobject {
   let watch = bridge::watching();
   // SAFETY: ART calls this with the current thread's env and the caller's
   // arguments. `invoke([B)[B` gets one byte array, forwarded unchanged or
   // swapped for a replacement.
   unsafe {
      let original = table(env).CallObjectMethodA;
      let kind = if watch || !NATIVES_BOUND.load(Ordering::Acquire) {
         method_kind(env, obj, method)
      } else {
         Method::Other
      };
      if kind != Method::Other {
         bind_natives(env, obj);
      }
      if !watch || kind != Method::Invoke {
         return original(env, obj, method, args);
      }

      remember_guest_host(env, obj, method);
      let request = (*args).l;
      let head = read_bytes(env, request, Some(6));
      if !head.starts_with(&[0; 4]) {
         match bridge::host_call(&head, || read_bytes(env, request, None)) {
            Action::Pass => {},
            Action::Drop => return ptr::null_mut(),
            Action::Replace(replaced) => {
               if let Some((array, _)) = new_bytes(env, &replaced) {
                  let result = original(env, obj, method, [jvalue { l: array }].as_ptr());
                  jni!(env, DeleteLocalRef, array);
                  return result;
               }
            },
         }
         return original(env, obj, method, args);
      }
      let frame = read_bytes(env, request, None);
      let (action, _guard) = bridge::ops(&frame[4..]);
      match action {
         Action::Drop => ptr::null_mut(),
         Action::Pass => original(env, obj, method, args),
         Action::Replace(replaced) => {
            let mut replaced_frame = vec![0; 4];
            replaced_frame.extend_from_slice(&replaced);
            let Some((array, _)) = new_bytes(env, &replaced_frame) else {
               return original(env, obj, method, args);
            };
            let result = original(env, obj, method, [jvalue { l: array }].as_ptr());
            jni!(env, DeleteLocalRef, array);
            result
         },
      }
   }
}

/// Phoenix's `GuestHost` as a global reference, null until Rust's first UI
/// batch.
pub fn guest_host() -> jobject {
   GUEST_HOST.load(Ordering::Acquire)
}

pub fn ready() -> bool {
   !guest_host().is_null()
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv`, and `obj` is the `GuestHost` that
/// `method` was invoked on.
unsafe fn remember_guest_host(env: *mut JNIEnv, obj: jobject, method: jmethodID) {
   let current = GUEST_HOST.load(Ordering::Acquire);
   // Backing out of the app destroys its activity but keeps the process, and
   // reopening it builds a new GuestHost. Frames sent to the old one go
   // nowhere, so the newest one wins.
   // SAFETY: forwarded from the caller, and `current` is a live global ref.
   if !current.is_null() && unsafe { jni!(env, IsSameObject, current, obj) } == JNI_TRUE {
      return;
   }
   INVOKE_METHOD.store(method, Ordering::Release);
   // SAFETY: forwarded from the caller. The global ref keeps the object alive
   // for apply_ops on other threads.
   let global = unsafe { jni!(env, NewGlobalRef, obj) };
   // ponytail: the old global ref leaks (one per reopen), since another
   // thread may still be calling through it.
   GUEST_HOST.store(global, Ordering::Release);
   if !current.is_null() {
      log::info!("bridge: GuestHost replaced after the activity was recreated");
   }
}

unsafe extern "system" fn native_dispatch(
   env: *mut JNIEnv,
   class: jclass,
   handle: jlong,
   frame: jbyteArray,
) -> jboolean {
   attach();
   let Some(&original) = ORIGINAL_DISPATCH.get() else {
      log::warn!("bridge: nativeDispatch bound without the original");
      return JNI_FALSE;
   };
   // SAFETY: ART calls this with the current thread's env and the arguments of
   // `nativeDispatch(J[B)Z`, forwarded unchanged or with a replacement array.
   unsafe {
      // Deliver any deferred frames (e.g. async HTTP response hooks) on the
      // `wasm-guest` thread before processing the current frame.
      for pending in bridge::take_deferred() {
         if let Some((array, _)) = new_bytes(env, &pending) {
            let _ = original(env, class, handle, array);
            jni!(env, DeleteLocalRef, array);
         }
      }

      let mut payload = read_bytes(env, frame, None);
      let mut stripped_wake = false;
      if payload == WAKE_SENTINEL {
         return JNI_TRUE;
      }
      if payload.first() == Some(&4)
         && let Ok(items) = phoenix_wire::decode_dispatch(&payload)
      {
         let wakes = items
            .iter()
            .filter(|item| &payload[item.offset..item.offset + item.len] == WAKE_SENTINEL)
            .map(|item| item.offset)
            .collect::<Vec<_>>();
         if !wakes.is_empty() {
            if wakes.len() == items.len() {
               return JNI_TRUE;
            }
            payload = phoenix_wire::without_frames(&payload, &wakes);
            stripped_wake = true;
         }
      }
      let replaced = match bridge::dispatched(&payload) {
         Action::Drop => return JNI_TRUE,
         Action::Pass => stripped_wake.then_some(payload),
         Action::Replace(rest) => Some(rest),
      };
      let Some(replaced) = replaced else {
         return original(env, class, handle, frame);
      };
      let Some((array, _)) = new_bytes(env, &replaced) else {
         return original(env, class, handle, frame);
      };
      let handled = original(env, class, handle, array);
      jni!(env, DeleteLocalRef, array);
      handled
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv`, and `guest_host` is a live
/// `GuestHost`.
unsafe fn bind_natives(env: *mut JNIEnv, guest_host: jobject) {
   if NATIVES_BOUND.swap(true, Ordering::AcqRel) {
      return;
   }

   // SAFETY: RTLD_NOLOAD only returns a handle to the already loaded library,
   // which the handle then pins, and the export has `nativeDispatch`'s JNI
   // signature.
   let original = unsafe {
      let handle = libc::dlopen(
         c"liblibdiscord_wasm_host.so".as_ptr(),
         libc::RTLD_NOW | libc::RTLD_NOLOAD,
      );
      let symbol = if handle.is_null() {
         ptr::null_mut()
      } else {
         libc::dlsym(
            handle,
            c"Java_com_discord_nativeapp_bridge_WasmRuntime_nativeDispatch".as_ptr(),
         )
      };
      (!symbol.is_null()).then(|| std::mem::transmute::<*mut c_void, DispatchFn>(symbol))
   };
   let Some(original) = original else {
      log::warn!("bridge: nativeDispatch symbol not found");
      return;
   };
   if ORIGINAL_DISPATCH.set(original).is_err() {
      return;
   }

   // WasmRuntime is an app class, so it resolves through GuestHost's loader.
   // SAFETY: forwarded from the caller. The method entry points at a function
   // with the JNI signature `(J[B)Z` declares.
   unsafe {
      let Some(runtime) = load_class(env, guest_host, WASM_RUNTIME) else {
         log::warn!("bridge: WasmRuntime class not found");
         return;
      };
      let entry = JNINativeMethod {
         name:      c"nativeDispatch".as_ptr().cast_mut(),
         signature: signature!((long, [byte]) -> boolean).as_ptr().cast_mut(),
         fnPtr:     (native_dispatch as *const ()).cast_mut().cast(),
      };
      if jni!(env, RegisterNatives, runtime, &raw const entry, 1) != JNI_OK {
         clear_exception(env);
         log::warn!("bridge: RegisterNatives failed for nativeDispatch");
         return;
      }
   }
   log::info!("bridge: nativeDispatch re-bound");
}

/// Loads `class` through Phoenix's class loader, as a local reference.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
pub unsafe fn app_class(env: *mut JNIEnv, class: Class) -> Option<jclass> {
   let anchor = guest_host();
   // SAFETY: `anchor` is the live `GuestHost` global reference, when set.
   (!anchor.is_null())
      .then(|| unsafe { load_class(env, anchor, class) })
      .flatten()
}

/// Loads `class` through the class loader of `anchor`'s class.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`, and `anchor` is a live object.
unsafe fn load_class(env: *mut JNIEnv, anchor: jobject, class: Class) -> Option<jclass> {
   // SAFETY: forwarded from the caller. Each method id is checked before the
   // call that uses it.
   unsafe {
      let anchor_class = jni!(env, GetObjectClass, anchor);
      let class_class = CLASS.find(env).ok()?;
      let get_loader = jni!(
         env,
         GetMethodID,
         class_class,
         c"getClassLoader".as_ptr(),
         signature!(() -> CLASS_LOADER).as_ptr(),
      );
      if get_loader.is_null() {
         clear_exception(env);
         return None;
      }
      let loader = jni!(
         env,
         CallObjectMethodA,
         anchor_class,
         get_loader,
         ptr::null()
      );
      if loader.is_null() {
         clear_exception(env);
         return None;
      }
      let loader_class = CLASS_LOADER.find(env).ok()?;
      let load = jni!(
         env,
         GetMethodID,
         loader_class,
         c"loadClass".as_ptr(),
         signature!((STRING) -> CLASS).as_ptr(),
      );
      let class_name = jni!(env, NewStringUTF, class.dotted().as_ptr());
      if load.is_null() || class_name.is_null() {
         clear_exception(env);
         return None;
      }
      let found = jni!(
         env,
         CallObjectMethodA,
         loader,
         load,
         [jvalue { l: class_name }].as_ptr()
      );
      (!clear_exception(env) && !found.is_null()).then_some(found)
   }
}
