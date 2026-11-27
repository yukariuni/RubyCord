//! `phoenix.storage`, a JSON object per plugin on disk, and `phoenix.stores`,
//! read-only SQL over Phoenix's cache.

use std::{
   fs,
   io::ErrorKind,
   path::PathBuf,
};

use rquickjs::{
   Ctx,
   Exception,
   Function,
   Object,
   Value,
   function::Rest,
};

use crate::{
   io_error,
   js::{
      background::{
         Settlement,
         in_background,
      },
      state,
      throw,
   },
   store,
};

/// A JSON object per plugin, loaded once and rewritten on every change.
pub fn storage<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let path = state().storage_path(plugin);
   let text = match fs::read_to_string(&path) {
      Ok(text) => text,
      Err(err) if err.kind() == ErrorKind::NotFound => "{}".to_owned(),
      Err(err) => return Err(throw(ctx, &io_error(&path)(err))),
   };
   let store = ctx.json_parse(text)?.into_object().ok_or_else(|| {
      Exception::throw_type(ctx, &format!("{} is not a JSON object", path.display()))
   })?;

   let persist = move |ctx: &Ctx<'js>, store: &Object<'js>, path: &PathBuf| {
      let json = ctx
         .json_stringify(store.clone())?
         .map(|json| json.to_string())
         .transpose()?;
      let write = || {
         fs::create_dir_all(path.parent().expect("storage paths have a parent"))?;
         fs::write(path, json.unwrap_or_default())
      };
      write().map_err(|err| throw(ctx, &io_error(path)(err)))
   };

   let storage = Object::new(ctx.clone())?;
   let read = store.clone();
   storage.set(
      "get",
      Function::new(ctx.clone(), move |key: String| {
         read.get::<_, Value<'js>>(key)
      })?,
   )?;
   let (write, file) = (store.clone(), path.clone());
   storage.set(
      "set",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, key: String, value: Value<'js>| {
            write.set(key, value)?;
            persist(&ctx, &write, &file)
         },
      )?,
   )?;
   let (write, file) = (store.clone(), path);
   storage.set(
      "delete",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, key: String| {
         write.remove(key)?;
         persist(&ctx, &write, &file)
      })?,
   )?;
   storage.set(
      "keys",
      Function::new(ctx.clone(), move || {
         store.keys::<String>().collect::<rquickjs::Result<Vec<_>>>()
      })?,
   )?;
   Ok(storage)
}

/// `phoenix.stores`, read-only SQL over Phoenix's cache.
pub fn stores_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   let stores = Object::new(ctx.clone())?;
   stores.set(
      "sql",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, sql: String, args: Rest<Value<'js>>| {
            let args = args
               .0
               .into_iter()
               .map(|arg| {
                  if let Some(text) = arg.as_string() {
                     return text.to_string();
                  }
                  ctx.json_stringify(arg)?
                     .map_or_else(|| Ok("null".to_owned()), |json| json.to_string())
               })
               .collect::<rquickjs::Result<Vec<String>>>()?;
            let files = state().files_dir();
            in_background(&ctx, move || {
               Settlement::Rows(
                  store::database(&files).and_then(|db| store::query(&db, &sql, &args)),
               )
            })
         },
      )?,
   )?;
   Ok(stores)
}
