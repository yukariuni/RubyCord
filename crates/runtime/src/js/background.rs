//! Promises that worker threads settle.

use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Value,
};

use crate::{
   discord,
   fetch,
   js::{
      Engine,
      Registry,
      drain,
      http::response_object,
   },
   store,
};

impl Engine {
   /// Settles promise `id` with what its worker thread produced, and runs what
   /// awaited it.
   pub fn settle(&self, id: u64, settlement: Settlement) {
      self.context.with(|ctx| {
         let Some((resolve, reject)) = ctx
            .userdata::<Registry<'_>>()
            .and_then(|registry| registry.pending.borrow_mut().remove(&id))
         else {
            return;
         };
         let value: rquickjs::Result<Result<Value<'_>, String>> = match settlement {
            Settlement::Fetch(Ok(response)) | Settlement::Discord(Ok(response)) => {
               response_object(&ctx, &response).map(|object| Ok(object.into_value()))
            },
            Settlement::Int(Ok(number)) => Ok(Ok(Value::new_int(ctx.clone(), number))),
            Settlement::Int(Err(err)) => Ok(Err(err)),
            Settlement::Text(Ok(text)) => {
               rquickjs::String::from_str(ctx.clone(), &text).map(|text| Ok(text.into_value()))
            },
            Settlement::Rows(Ok(rows)) => {
               serde_json::to_string(&rows)
                  .map_err(|err| Exception::throw_internal(&ctx, &err.to_string()))
                  .and_then(|json| ctx.json_parse(json))
                  .map(Ok)
            },
            Settlement::Json(Ok(Some(json))) => ctx.json_parse(json).map(Ok),
            Settlement::Json(Ok(None)) => Ok(Ok(Value::new_null(ctx.clone()))),
            Settlement::Fetch(Err(err)) => Ok(Err(err.to_string())),
            Settlement::Discord(Err(err)) | Settlement::Text(Err(err)) => Ok(Err(err.to_string())),
            Settlement::Rows(Err(err)) | Settlement::Json(Err(err)) => Ok(Err(err.to_string())),
         };
         let outcome = value.and_then(|value| {
            match value {
               Ok(value) => resolve.call::<_, ()>((value,)),
               Err(message) => {
                  reject.call::<_, ()>((Exception::from_message(ctx.clone(), &message)?,))
               },
            }
         });
         if let Err(err) = outcome.catch(&ctx) {
            log::error!("settling promise {id}: {err}");
         }
      });
      drain(&self.runtime);
   }
}

/// What a worker thread hands back to settle a promise with.
pub enum Settlement {
   Fetch(Result<fetch::Response, fetch::FetchError>),
   Discord(Result<fetch::Response, discord::DiscordError>),
   Text(Result<String, discord::DiscordError>),
   Int(Result<i32, String>),
   Rows(Result<store::Rows, store::StoreError>),
   Json(Result<Option<String>, store::StoreError>),
}

/// Runs `work` on its own thread and returns a promise it settles. The
/// resolvers stay in the registry, since JS values cannot leave the thread
/// that holds the engine.
pub fn in_background<'js>(
   ctx: &Ctx<'js>,
   work: impl FnOnce() -> Settlement + Send + 'static,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

   let (promise, resolve, reject) = ctx.promise()?;
   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .pending
      .borrow_mut()
      .insert(id, (resolve, reject));
   std::thread::spawn(move || {
      let settlement = work();
      if let Some(engine) = crate::engine() {
         engine.settle(id, settlement);
      }
   });
   Ok(promise)
}
