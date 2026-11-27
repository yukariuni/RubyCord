//! `phoenix.discord.onRequest` and `onResponse`, hooks on Phoenix's own API
//! traffic.

use std::sync::{
   Arc,
   Mutex,
   PoisonError,
   mpsc::Sender,
};

use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   Object,
   Value,
};

use crate::{
   discord,
   js::{
      Engine,
      Registry,
      drain,
   },
};

impl Engine {
   /// Runs matching `phoenix.discord.onRequest` hooks on an outgoing Discord
   /// API request.
   #[must_use]
   pub fn intercept_request(
      &self,
      matched: &[u64],
      method: &str,
      path: &str,
      body: &[u8],
   ) -> discord::RequestAction {
      let action = self.context.with(|ctx| {
         let Some(registry) = ctx.userdata::<Registry<'_>>() else {
            return discord::RequestAction::Pass;
         };
         let hooks = {
            let map = registry.request_hooks.borrow();
            matched
               .iter()
               .filter_map(|id| {
                  let (plugin, function) = map.get(id)?;
                  Some((plugin.clone(), function.clone()))
               })
               .collect::<Vec<_>>()
         };
         drop(registry);
         if hooks.is_empty() {
            return discord::RequestAction::Pass;
         }
         let mut current_body = String::from_utf8_lossy(body).into_owned();
         let mut changed = false;
         for (plugin, function) in hooks {
            let Ok(req) = Object::new(ctx.clone()) else {
               break;
            };
            let _ = req.set("method", method);
            let _ = req.set("path", path);
            let _ = req.set("body", current_body.as_str());
            let json_source = current_body.clone();
            if let Ok(json_fn) = json_parser_fn(&ctx, json_source) {
               let _ = req.set("json", json_fn);
            }
            match function.call::<_, Value<'_>>((req,)).catch(&ctx) {
               Ok(result) if result.as_bool() == Some(false) => {
                  return discord::RequestAction::Drop;
               },
               Ok(ref result) if let Some(obj) = result.as_object() => {
                  if let Ok(new_body) = obj.get::<_, Value<'_>>("body")
                     && !new_body.is_undefined()
                     && !new_body.is_null()
                  {
                     if let Some(text) = new_body.as_string() {
                        if let Ok(text) = text.to_string() {
                           current_body = text;
                           changed = true;
                        }
                     } else if let Ok(Some(json)) = ctx.json_stringify(new_body)
                        && let Ok(text) = json.to_string()
                     {
                        current_body = text;
                        changed = true;
                     }
                  }
               },
               Ok(_) => {},
               Err(err) => log::error!("[{plugin}] onRequest: {err}"),
            }
         }
         if changed {
            discord::RequestAction::Replace(current_body.into_bytes())
         } else {
            discord::RequestAction::Pass
         }
      });
      drain(&self.runtime);
      action
   }

   /// Runs matching `phoenix.discord.onResponse` hooks (sync or async) on an
   /// incoming Discord API reply, sending the final body through `done`.
   pub fn intercept_response(
      &self,
      matched: Vec<u64>,
      method: String,
      path: String,
      status: i32,
      body: Vec<u8>,
      done: Sender<(i32, Vec<u8>)>,
   ) {
      let pending = Pending {
         remaining: matched,
         method,
         path,
         status,
         body,
         done,
      };
      self.context.with(|ctx| run_response_chain(&ctx, pending));
      drain(&self.runtime);
   }
}

fn json_parser_fn<'js>(ctx: &Ctx<'js>, source: String) -> rquickjs::Result<Function<'js>> {
   Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      ctx.json_parse(source.as_str())
         .unwrap_or_else(|_| Value::new_null(ctx))
   })
}

/// Extracts a replacement body from `{ body: string | object }` returned by
/// `onRequest` / `onResponse`.
fn extract_hook_body<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Option<Vec<u8>> {
   let obj = value.as_object()?;
   let new_body = obj.get::<_, Value<'js>>("body").ok()?;
   if new_body.is_undefined() || new_body.is_null() {
      return None;
   }
   if let Some(text) = new_body.as_string() {
      return text.to_string().ok().map(String::into_bytes);
   }
   ctx.json_stringify(new_body)
      .ok()
      .flatten()
      .and_then(|json| json.to_string().ok())
      .map(String::into_bytes)
}

/// Extracts a replacement status from `{ status: number }` returned by
/// `onResponse`: lets a hook turn a Discord 404 into a 200 with a body of its
/// own. Out-of-range values leave the reply's status alone.
fn extract_hook_status(value: &Value<'_>) -> Option<i32> {
   let status = value.as_object()?.get::<_, Value<'_>>("status").ok()?.as_int()?;
   (100..=599).contains(&status).then_some(status)
}

/// What the rest of a response chain needs. A promise's fulfil and reject
/// handlers share one, and whichever runs first takes it.
struct Pending {
   remaining: Vec<u64>,
   method:    String,
   path:      String,
   status:    i32,
   body:      Vec<u8>,
   done:      Sender<(i32, Vec<u8>)>,
}

fn take_pending(shared: &Mutex<Option<Pending>>) -> Option<Pending> {
   shared.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Chains matching `onResponse` hooks sequentially, supporting both synchronous
/// and Promise-returning handlers without blocking Phoenix's `wasm-guest`
/// thread.
fn run_response_chain<'js>(ctx: &Ctx<'js>, mut pending: Pending) {
   while !pending.remaining.is_empty() {
      let hook_id = pending.remaining.remove(0);
      let hook = ctx.userdata::<Registry<'js>>().and_then(|registry| {
         let map = registry.response_hooks.borrow();
         let (plugin, function) = map.get(&hook_id)?;
         Some((plugin.clone(), function.clone()))
      });
      let Some((plugin, function)) = hook else {
         continue;
      };
      let body_str = String::from_utf8_lossy(&pending.body).into_owned();
      let Ok(res_obj) = Object::new(ctx.clone()) else {
         break;
      };
      let _ = res_obj.set("method", pending.method.as_str());
      let _ = res_obj.set("path", pending.path.as_str());
      let _ = res_obj.set("status", pending.status);
      let _ = res_obj.set("body", body_str.as_str());
      if let Ok(json_fn) = json_parser_fn(ctx, body_str) {
         let _ = res_obj.set("json", json_fn);
      }
      let result = match function.call::<_, Value<'js>>((res_obj,)).catch(ctx) {
         Ok(result) => result,
         Err(err) => {
            log::error!("[{plugin}] onResponse: {err}");
            continue;
         },
      };
      let Some(promise) = result.as_promise() else {
         if let Some(next_body) = extract_hook_body(ctx, &result) {
            pending.body = next_body;
         }
         if let Some(next_status) = extract_hook_status(&result) {
            pending.status = next_status;
         }
         continue;
      };

      let shared = Arc::new(Mutex::new(Some(pending)));
      let ok_shared = Arc::clone(&shared);
      let ok = Function::new(ctx.clone(), move |value: Value<'_>| {
         let Some(mut rest) = take_pending(&ok_shared) else {
            return;
         };
         if let Some(next_body) = extract_hook_body(value.ctx(), &value) {
            rest.body = next_body;
         }
         if let Some(next_status) = extract_hook_status(&value) {
            rest.status = next_status;
         }
         run_response_chain(value.ctx(), rest);
      });
      let err_shared = Arc::clone(&shared);
      let err_plugin = plugin.clone();
      let err_fn = Function::new(ctx.clone(), move |reason: Value<'_>| {
         log::error!("[{err_plugin}] onResponse: {reason:?}");
         if let Some(rest) = take_pending(&err_shared) {
            run_response_chain(reason.ctx(), rest);
         }
      });
      let attach = (|| -> rquickjs::Result<()> {
         let then = promise.then()?;
         then.call::<_, ()>((rquickjs::function::This(promise.clone()), ok?, err_fn?))
      })();
      if let Err(err) = attach.catch(ctx) {
         log::error!("[{plugin}] onResponse promise: {err}");
         // Dropping the sender causes `run_response_hooks` to deliver the
         // original frame.
         drop(take_pending(&shared));
      }
      return;
   }
   let _ = pending.done.send((pending.status, pending.body));
}

pub fn add_discord_hooks<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   object: &Object<'js>,
) -> rquickjs::Result<()> {
   let owner = plugin.to_owned();
   object.set(
      "onRequest",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, filter: Object<'js>, handler: Function<'js>| {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let parsed = discord::RequestFilter {
               method:        filter.get::<_, Option<String>>("method")?,
               path_prefix:   filter.get::<_, Option<String>>("pathPrefix")?,
               path_suffix:   filter.get::<_, Option<String>>("pathSuffix")?,
               path_includes: filter.get::<_, Option<String>>("pathIncludes")?,
            };
            ctx.userdata::<Registry<'js>>()
               .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
               .request_hooks
               .borrow_mut()
               .insert(id, (owner.clone(), handler));
            discord::add_request_hook(id, owner.clone(), parsed);
            Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
               if let Some(registry) = ctx.userdata::<Registry<'js>>() {
                  registry.request_hooks.borrow_mut().remove(&id);
               }
               discord::remove_request_hook(id);
            })
         },
      )?,
   )?;
   let owner_res = plugin.to_owned();
   object.set(
      "onResponse",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, filter: Object<'js>, handler: Function<'js>| {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let parsed = discord::RequestFilter {
               method:        filter.get::<_, Option<String>>("method")?,
               path_prefix:   filter.get::<_, Option<String>>("pathPrefix")?,
               path_suffix:   filter.get::<_, Option<String>>("pathSuffix")?,
               path_includes: filter.get::<_, Option<String>>("pathIncludes")?,
            };
            ctx.userdata::<Registry<'js>>()
               .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
               .response_hooks
               .borrow_mut()
               .insert(id, (owner_res.clone(), handler));
            discord::add_response_hook(id, owner_res.clone(), parsed);
            Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
               if let Some(registry) = ctx.userdata::<Registry<'js>>() {
                  registry.response_hooks.borrow_mut().remove(&id);
               }
               discord::remove_response_hook(id);
            })
         },
      )?,
   )?;
   Ok(())
}
