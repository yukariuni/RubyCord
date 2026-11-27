//! Gateway hooks: `phoenix.on`, `hook`, `dispatch` and `enqueue`, and the
//! `before` and `after` phases that run them.

use phoenix_sigs::registry::EventId;
use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   Object,
   Value,
};

use crate::{
   Error,
   gateway,
   js::{
      Before,
      Engine,
      Registry,
      drain,
      js_error,
   },
};

#[derive(Clone)]
pub struct Hook<'js> {
   pub plugin: String,
   pub before: Option<Function<'js>>,
   pub after:  Option<Function<'js>>,
}

impl Engine {
   /// Runs the `before` hooks for `event`. Each sees the previous hook's
   /// payload.
   pub fn before(&self, event: EventId, name: &str, json: &str) -> Result<Before, Error> {
      let result = self.context.with(|ctx| {
         let hooks = registered(&ctx, event, |hook| hook.before.as_ref());
         if hooks.is_empty() {
            return Ok(Before::Keep);
         }
         let mut payload = ctx.json_parse(json).catch(&ctx).map_err(js_error)?;
         for (plugin, before) in hooks {
            match before.call::<_, Value<'_>>((payload.clone(),)).catch(&ctx) {
               Ok(result) if result.as_bool() == Some(false) => return Ok(Before::Drop),
               Ok(result) if result.is_undefined() => {},
               Ok(result) => payload = result,
               Err(err) => log::error!("[{plugin}] before {name}: {err}"),
            }
         }
         let json = ctx
            .json_stringify(payload)
            .catch(&ctx)
            .map_err(js_error)?
            .ok_or_else(|| Error::Js(format!("a before {name} hook returned no JSON value")))?;
         Ok(Before::Replace(
            json.to_string().map_err(|err| Error::Js(err.to_string()))?,
         ))
      });
      drain(&self.runtime);
      result
   }

   pub fn after(&self, event: EventId, name: &str, json: &str, ok: bool) -> Result<(), Error> {
      let result = self.context.with(|ctx| {
         let hooks = registered(&ctx, event, |hook| hook.after.as_ref());
         if hooks.is_empty() {
            return Ok(());
         }
         let payload = ctx.json_parse(json).catch(&ctx).map_err(js_error)?;
         for (plugin, after) in hooks {
            if let Err(err) = after.call::<_, ()>((payload.clone(), ok)).catch(&ctx) {
               log::error!("[{plugin}] after {name}: {err}");
            }
         }
         Ok(())
      });
      drain(&self.runtime);
      result
   }

   #[must_use]
   pub fn intercept_gateway_send(
      &self,
      matched: &[u64],
      op: u32,
      frame_json: &str,
   ) -> gateway::SendAction {
      let action = self.context.with(|ctx| {
         let Some(registry) = ctx.userdata::<Registry<'_>>() else {
            return gateway::SendAction::Pass;
         };
         let hooks = {
            let map = registry.gateway_send_hooks.borrow();
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
            return gateway::SendAction::Pass;
         }
         let Ok(frame_val) = ctx.json_parse(frame_json).catch(&ctx) else {
            return gateway::SendAction::Pass;
         };
         let Some(frame) = frame_val.as_object() else {
            return gateway::SendAction::Pass;
         };
         let mut payload: Value<'_> = frame
            .get("d")
            .unwrap_or_else(|_| Value::new_null(ctx.clone()));
         let before_d = ctx
            .json_stringify(payload.clone())
            .ok()
            .flatten()
            .and_then(|s| s.to_string().ok());
         let mut replaced = false;
         for (plugin, function) in hooks {
            match function.call::<_, Value<'_>>((payload.clone(),)).catch(&ctx) {
               Ok(result) if result.as_bool() == Some(false) => {
                  return gateway::SendAction::Drop;
               },
               Ok(result) if result.is_undefined() => {},
               Ok(result) => {
                  payload = result;
                  replaced = true;
               },
               Err(err) => log::error!("[{plugin}] gateway.onSend({op}): {err}"),
            }
         }
         let after_d = ctx
            .json_stringify(payload.clone())
            .ok()
            .flatten()
            .and_then(|s| s.to_string().ok());
         if replaced && after_d.is_none() {
            log::error!("gateway.onSend({op}): hook returned no JSON value");
            return gateway::SendAction::Pass;
         }
         if !replaced && before_d == after_d {
            return gateway::SendAction::Pass;
         }
         if frame.set("d", payload).is_err() {
            return gateway::SendAction::Pass;
         }
         ctx.json_stringify(frame_val)
            .ok()
            .flatten()
            .and_then(|s| s.to_string().ok())
            .map_or_else(
               || {
                  log::error!("gateway.onSend({op}): hook returned no JSON value");
                  gateway::SendAction::Pass
               },
               gateway::SendAction::Replace,
            )
      });
      drain(&self.runtime);
      action
   }
}

/// Snapshot of one phase's callbacks, so a callback that registers a hook does
/// not run into the borrow held while iterating.
fn registered<'js>(
   ctx: &Ctx<'js>,
   event: EventId,
   phase: impl for<'hook> Fn(&'hook Hook<'js>) -> Option<&'hook Function<'js>>,
) -> Vec<(String, Function<'js>)> {
   let Some(registry) = ctx.userdata::<Registry<'js>>() else {
      return Vec::new();
   };
   let hooks = registry.hooks.borrow();
   hooks
      .get(&event)
      .into_iter()
      .flatten()
      .filter_map(|hook| Some((hook.plugin.clone(), phase(hook)?.clone())))
      .collect()
}

fn event_of<'js>(ctx: &Ctx<'js>, sig: &Value<'js>) -> rquickjs::Result<EventId> {
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .handles
      .iter()
      .find(|(_, handle)| handle.as_value() == sig)
      .map(|(event, _)| *event)
      .ok_or_else(|| Exception::throw_type(ctx, "expected a hook point from phoenix.gateway"))
}

fn register<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   sig: &Value<'js>,
   before: Option<Function<'js>>,
   after: Option<Function<'js>>,
) -> rquickjs::Result<()> {
   let event = event_of(ctx, sig)?;
   gateway::install(event);
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .hooks
      .borrow_mut()
      .entry(event)
      .or_default()
      .push(Hook {
         plugin: plugin.to_owned(),
         before,
         after,
      });
   Ok(())
}

pub fn add_gateway_api<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   api: &Object<'js>,
) -> rquickjs::Result<()> {
   let name = plugin.to_owned();
   api.set(
      "on",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, sig: Value<'js>, listener: Function<'js>| {
            register(&ctx, &name, &sig, None, Some(listener))
         },
      )?,
   )?;
   let name = plugin.to_owned();
   api.set(
      "hook",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, sig: Value<'js>, hooks: Object<'js>| {
            register(&ctx, &name, &sig, hooks.get("before")?, hooks.get("after")?)
         },
      )?,
   )?;
   api.set(
      "dispatch",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, sig: Value<'js>, payload: Value<'js>| {
            let event = event_of(&ctx, &sig)?;
            let json = ctx
               .json_stringify(payload)?
               .ok_or_else(|| Exception::throw_type(&ctx, "dispatch needs a JSON payload"))?
               .to_string()?;
            gateway::dispatch(event, &json).ok_or_else(|| {
               Exception::throw_message(&ctx, "dispatch runs only inside a gateway hook")
            })
         },
      )?,
   )?;

   api.set(
      "enqueue",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, sig: Value<'js>, payload: Value<'js>| {
            let event = event_of(&ctx, &sig)?;
            let json = ctx
               .json_stringify(payload)?
               .ok_or_else(|| Exception::throw_type(&ctx, "enqueue needs a JSON payload"))?
               .to_string()?;
            gateway::enqueue(event, json);
            Ok::<_, rquickjs::Error>(())
         },
      )?,
   )?;
   Ok(())
}

pub fn gateway_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let base_gateway = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .gateway
      .clone();
   let object_ctor = ctx.globals().get::<_, Object>("Object")?;
   let gateway: Object<'js> = object_ctor
      .get::<_, Function>("assign")?
      .call((Object::new(ctx.clone())?, base_gateway))?;
   add_gateway_send_api(ctx, plugin, &gateway)?;
   object_ctor
      .get::<_, Function>("freeze")?
      .call((gateway,))
}

pub fn add_gateway_send_api<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   gateway_obj: &Object<'js>,
) -> rquickjs::Result<()> {
   let name = plugin.to_owned();
   let on_send = Function::new(
      ctx.clone(),
      move |ctx: Ctx<'js>, op: f64, handler: Function<'js>| {
         if !op.is_finite()
            || op.fract() != 0.0
            || op < 0.0
            || op > f64::from(u32::MAX)
         {
            return Err(Exception::throw_type(
               &ctx,
               "gateway.onSend expects a non-negative integer opcode",
            ));
         }
         #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked finite, integral, and within 0..=u32::MAX above"
         )]
         let op = op as u32;
         if gateway::BLOCKED_SEND_OPS.contains(&op) {
            return Err(Exception::throw_type(
               &ctx,
               "Identify (2) and Resume (6) carry credentials and cannot be hooked",
            ));
         }
         let id = gateway::register_send_hook(&name, op);
         ctx.userdata::<Registry<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
            .gateway_send_hooks
            .borrow_mut()
            .insert(id, (name.clone(), handler));
         Function::new(ctx, move |ctx: Ctx<'js>| {
            gateway::unregister_send_hook(id);
            if let Some(registry) = ctx.userdata::<Registry<'js>>() {
               registry.gateway_send_hooks.borrow_mut().remove(&id);
            }
            Ok::<_, rquickjs::Error>(())
         })
      },
   )?;
   // keep onSend non-enumerable so Object.keys(phoenix.gateway) stays just event handles
   let desc = Object::new(ctx.clone())?;
   desc.set("value", on_send)?;
   desc.set("enumerable", false)?;
   desc.set("writable", false)?;
   desc.set("configurable", false)?;
   ctx.globals()
      .get::<_, Object>("Object")?
      .get::<_, Function>("defineProperty")?
      .call::<_, ()>((gateway_obj.clone(), "onSend", desc))?;
   Ok(())
}

#[cfg(test)]
mod tests {
   use std::sync::Mutex;

   use phoenix_sigs::registry::GatewayRegistry;

   use super::*;
   use crate::gateway::SendAction;

   // SEND_HOOKS is process-global, so serialize these tests
   static TEST_LOCK: Mutex<()> = Mutex::new(());

   fn run_plugin(engine: &Engine, plugin: &str, code: &str) -> Result<bool, String> {
      engine.with(|ctx| {
         let run = || -> rquickjs::Result<bool> {
            let gw = gateway_object(&ctx, plugin)?;
            let wrapped = format!("(function (gateway) {{\n{code}\n}})");
            let val: Value<'_> = ctx.eval::<Function<'_>, _>(wrapped)?.call((gw,))?;
            Ok(val.as_bool().unwrap_or(true))
         };
         run().catch(&ctx).map_err(|e| e.to_string())
      })
   }

   fn dispatch_send(engine: &Engine, op: u32, frame: &str) -> SendAction {
      let matched = gateway::matching_send_hooks(op);
      if matched.is_empty() {
         return SendAction::Pass;
      }
      engine.intercept_gateway_send(&matched, op, frame)
   }

   #[test]
   fn rewrites_mutates_chains_drops_and_cleans_up() {
      let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      let engine = Engine::new(&GatewayRegistry::default()).expect("engine initializes");

      // blocked opcodes and bad args throw TypeError
      assert!(run_plugin(&engine, "t", "gateway.onSend(2, () => {});").is_err());
      assert!(run_plugin(&engine, "t", "gateway.onSend(6, () => {});").is_err());
      assert!(run_plugin(&engine, "t", "gateway.onSend(-1, () => {});").is_err());
      assert!(run_plugin(&engine, "t", "gateway.onSend(4.5, () => {});").is_err());

      // onSend is non-enumerable and gateway stays frozen
      assert_eq!(run_plugin(&engine, "t", "return Object.isFrozen(gateway);"), Ok(true));
      assert_eq!(
         run_plugin(&engine, "t", "return Object.keys(gateway).includes('onSend');"),
         Ok(false)
      );

      // rewrite + chain + in-place mutation
      run_plugin(&engine, "a", "gateway.onSend(4, d => ({ ...d, self_deaf: true }));").unwrap();
      run_plugin(&engine, "b", "gateway.onSend(4, d => { d.self_mute = true; });").unwrap();

      let frame = r#"{"op":4,"d":{"guild_id":"1","channel_id":"2","self_mute":false,"self_deaf":false}}"#;
      match dispatch_send(&engine, 4, frame) {
         SendAction::Replace(out) => {
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["op"], 4);
            assert_eq!(v["d"]["self_deaf"], true);
            assert_eq!(v["d"]["self_mute"], true);
            assert_eq!(v["d"]["guild_id"], "1");
         },
         other => panic!("expected Replace, got {other:?}"),
      }

      // unmatched op never enters JS
      assert_eq!(dispatch_send(&engine, 3, r#"{"op":3,"d":{}}"#), SendAction::Pass);

      // pure observer returns Pass
      run_plugin(&engine, "obs", "gateway.onSend(3, () => {});").unwrap();
      assert_eq!(dispatch_send(&engine, 3, r#"{"op":3,"d":{"status":"online"}}"#), SendAction::Pass);

      // non-JSON return falls back to Pass instead of stripping d
      run_plugin(&engine, "bad", "gateway.onSend(3, () => () => {});").unwrap();
      assert_eq!(dispatch_send(&engine, 3, r#"{"op":3,"d":{"status":"online"}}"#), SendAction::Pass);

      // returning false drops the frame
      run_plugin(&engine, "dropper", "gateway.onSend(4, () => false);").unwrap();
      assert_eq!(dispatch_send(&engine, 4, frame), SendAction::Drop);

      // unload cleans up plugin hooks
      engine.unload("dropper");
      assert!(matches!(dispatch_send(&engine, 4, frame), SendAction::Replace(_)));

      // remover unregisters one hook
      run_plugin(&engine, "rem", "const off = gateway.onSend(4, () => false); off();").unwrap();
      assert!(matches!(dispatch_send(&engine, 4, frame), SendAction::Replace(_)));

      for plugin in ["a", "b", "obs", "bad", "dropper", "rem", "t"] {
         gateway::forget_send_hooks(plugin);
      }
   }
}
