//! Read-only SQL over Phoenix's own cache,
//! `files/store/<account>/cache.sqlite`, through Android's `SQLiteDatabase` so
//! phoenix ships no SQLite.

use std::{
   ffi::CString,
   path::Path,
   ptr,
};

use jni_sys::{
   JNIEnv,
   jint,
   jobject,
   jvalue,
};
use phoenix_runtime::store::{
   Cell,
   ROW_LIMIT,
   Rows,
   StoreError,
};

use crate::jvm::{
   Class,
   JniError,
   STRING,
   class,
   clear_exception,
   env,
   jni,
   method,
   read_bytes,
   read_string,
   signature,
   static_method,
   take_exception,
};

const SQLITE_DATABASE: Class = class!(android::database::sqlite::SQLiteDatabase);
const CURSOR_FACTORY: Class = class!(android::database::sqlite::SQLiteDatabase.CursorFactory);
const CURSOR: Class = class!(android::database::Cursor);

/// `SQLiteDatabase.OPEN_READONLY | NO_LOCALIZED_COLLATORS`.
const OPEN_FLAGS: jint = 0x01 | 0x10;
/// `Cursor.FIELD_TYPE_*`.
const FIELD_INTEGER: jint = 1;
const FIELD_FLOAT: jint = 2;
const FIELD_STRING: jint = 3;
const FIELD_BLOB: jint = 4;

/// Runs `sql` with `args` bound to its `?`s.
pub fn query(database: &Path, sql: &str, args: &[String]) -> Result<Rows, StoreError> {
   let env = env().ok_or(JniError::NoVm)?;
   // SAFETY: `env` belongs to this thread, and every call below passes
   // references made on it with the argument types its signature names.
   unsafe {
      if jni!(env, PushLocalFrame, 64) != 0 {
         return Err(sql_error(env));
      }
      let result = run(env, database, sql, args);
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv` inside a local frame.
unsafe fn run(
   env: *mut JNIEnv,
   database: &Path,
   sql: &str,
   args: &[String],
) -> Result<Rows, StoreError> {
   // SAFETY: forwarded from the caller.
   unsafe {
      let string = |text: &str| -> Result<jobject, StoreError> {
         let text = CString::new(text).map_err(|_| JniError::Nul)?;
         Ok(jni!(env, NewStringUTF, text.as_ptr()))
      };
      let db_class = SQLITE_DATABASE.find(env)?;
      let string_class = STRING.find(env)?;

      let open = static_method(
         env,
         db_class,
         c"openDatabase",
         signature!((STRING, CURSOR_FACTORY, int) -> SQLITE_DATABASE),
      )?;
      let path = string(&database.to_string_lossy())?;
      let db = jni!(
         env,
         CallStaticObjectMethodA,
         db_class,
         open,
         [jvalue { l: path }, jvalue { l: ptr::null_mut() }, jvalue {
            i: OPEN_FLAGS,
         }]
         .as_ptr()
      );
      if db.is_null() || jni!(env, ExceptionCheck) {
         return Err(sql_error(env));
      }

      let count = jint::try_from(args.len()).map_err(|_| JniError::Nul)?;
      let bound = jni!(env, NewObjectArray, count, string_class, ptr::null_mut());
      for (index, arg) in (0..).zip(args) {
         jni!(env, SetObjectArrayElement, bound, index, string(arg)?);
      }
      let raw_query = method(
         env,
         db_class,
         c"rawQuery",
         signature!((STRING, [STRING]) -> CURSOR),
      )?;
      let cursor = jni!(
         env,
         CallObjectMethodA,
         db,
         raw_query,
         [jvalue { l: string(sql)? }, jvalue { l: bound }].as_ptr()
      );
      let rows = if cursor.is_null() || jni!(env, ExceptionCheck) {
         Err(sql_error(env))
      } else {
         rows(env, cursor)
      };

      let close = method(env, db_class, c"close", signature!(() -> void))?;
      jni!(env, CallVoidMethodA, db, close, ptr::null());
      clear_exception(env);
      rows
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv` inside a local frame, and `cursor` a
/// live `Cursor`, closed here.
unsafe fn rows(env: *mut JNIEnv, cursor: jobject) -> Result<Rows, StoreError> {
   // SAFETY: forwarded from the caller. Each column value is a local ref
   // deleted after it is read, so a long result does not fill the frame.
   unsafe {
      let class = jni!(env, GetObjectClass, cursor);
      let column_count = method(env, class, c"getColumnCount", signature!(() -> int))?;
      let column_name = method(env, class, c"getColumnName", signature!((int) -> STRING))?;
      let move_next = method(env, class, c"moveToNext", signature!(() -> boolean))?;
      let kind = method(env, class, c"getType", signature!((int) -> int))?;
      let get_long = method(env, class, c"getLong", signature!((int) -> long))?;
      let get_double = method(env, class, c"getDouble", signature!((int) -> double))?;
      let get_string = method(env, class, c"getString", signature!((int) -> STRING))?;
      let get_blob = method(env, class, c"getBlob", signature!((int) -> [byte]))?;
      let close = method(env, class, c"close", signature!(() -> void))?;

      let count = jni!(env, CallIntMethodA, cursor, column_count, ptr::null());
      let columns = (0..count)
         .map(|column| {
            let name = jni!(
               env,
               CallObjectMethodA,
               cursor,
               column_name,
               [jvalue { i: column }].as_ptr()
            );
            let text = read_string(env, name).unwrap_or_default();
            jni!(env, DeleteLocalRef, name);
            text
         })
         .collect::<Vec<String>>();

      let mut rows = Vec::new();
      while rows.len() < ROW_LIMIT && jni!(env, CallBooleanMethodA, cursor, move_next, ptr::null())
      {
         let mut row = Vec::with_capacity(columns.len());
         for column in 0..count {
            let arg = [jvalue { i: column }];
            let cell = match jni!(env, CallIntMethodA, cursor, kind, arg.as_ptr()) {
               FIELD_INTEGER => {
                  Cell::Integer(jni!(env, CallLongMethodA, cursor, get_long, arg.as_ptr()))
               },
               FIELD_FLOAT => {
                  Cell::Float(jni!(
                     env,
                     CallDoubleMethodA,
                     cursor,
                     get_double,
                     arg.as_ptr()
                  ))
               },
               FIELD_STRING => {
                  let text = jni!(env, CallObjectMethodA, cursor, get_string, arg.as_ptr());
                  let cell = read_string(env, text).map_or(Cell::Null, Cell::Text);
                  jni!(env, DeleteLocalRef, text);
                  cell
               },
               FIELD_BLOB => {
                  let blob = jni!(env, CallObjectMethodA, cursor, get_blob, arg.as_ptr());
                  let cell = Cell::Blob(read_bytes(env, blob, None));
                  jni!(env, DeleteLocalRef, blob);
                  cell
               },
               _ => Cell::Null,
            };
            row.push(cell);
         }
         rows.push(row);
      }
      let failed = jni!(env, ExceptionCheck);
      let error = failed.then(|| sql_error(env));
      jni!(env, CallVoidMethodA, cursor, close, ptr::null());
      clear_exception(env);
      error.map_or(Ok(Rows { columns, rows }), Err)
   }
}

/// The pending exception as `Throwable.toString()`, cleared.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
unsafe fn sql_error(env: *mut JNIEnv) -> StoreError {
   // SAFETY: forwarded from the caller.
   let message = unsafe { take_exception(env) };
   StoreError::Sql {
      message: message.unwrap_or_else(|| "a Java call failed".to_owned()),
   }
}
