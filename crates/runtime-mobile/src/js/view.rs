//! Raw view tree access: `phoenix.ui.onEvent`, `dispatch`, `onNode`,
//! `patchProps`, `hide`, `update` and the node queries.

use std::str::FromStr;

use phoenix_runtime::js::Engine;
use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   Object,
   Value,
   function::Opt,
};

use crate::{
   js::{
      Mobile,
      ui::{
         millis,
         remover,
      },
      wire::{
         js_to_wire,
         parse_props,
         wire_to_js,
      },
   },
   patches::{
      self,
      UiContext,
      nodes::{
         NodeInfo,
         NodeMatch,
      },
      props::PatchTarget,
   },
};

pub struct EventHook<'js> {
   pub id:        u64,
   pub plugin:    String,
   pub key:       phoenix_wire::Key,
   pub intercept: bool,
   pub function:  Function<'js>,
}

/// Runs `phoenix.ui.onEvent` handlers matching `event`. When `intercepted`
/// is true, returns whether any intercepting handler consumed the event
/// (true) or all returned `false` (false, re-injecting to Phoenix).
pub fn view_event(
   engine: &Engine,
   node: i32,
   event: phoenix_wire::Key,
   payload: &phoenix_wire::Value,
   aid: Option<&str>,
   context: Option<&UiContext>,
   intercepted: bool,
) -> bool {
   engine.with(|ctx| {
      let Some(registry) = ctx.userdata::<Mobile<'_>>() else {
         return false;
      };
      let handlers = registry
         .event_hooks
         .borrow()
         .iter()
         .filter(|hook| hook.key == event)
         .map(|hook| (hook.plugin.clone(), hook.intercept, hook.function.clone()))
         .collect::<Vec<_>>();
      drop(registry);
      if handlers.is_empty() {
         return false;
      }
      let Ok(ev) = Object::new(ctx.clone()) else {
         return false;
      };
      let event_name = phoenix_ui_mobile::PROPS
         .get(usize::from(event.get()))
         .copied()
         .unwrap_or_default();
      let _ = ev.set("node", node);
      let _ = ev.set("event", event_name);
      if let Ok(val) = wire_to_js(&ctx, payload) {
         let _ = ev.set("payload", val);
      }
      let _ = ev.set("accessibilityId", aid);
      if let Some(context) = context {
         let _ = ev.set("channelId", context.channel_id.clone());
         let _ = ev.set("currentUserId", context.current_user_id.clone());
         if let Some(id) = &context.message_id {
            let _ = ev.set("messageId", id.as_str());
         }
         if let Some(message) = &context.message
            && let Ok(parsed) = ctx.json_parse(message.as_str())
         {
            let _ = ev.set("message", parsed);
         }
      }
      let mut taken = false;
      for (plugin, is_interceptor, function) in handlers {
         match function.call::<_, Value<'_>>((ev.clone(),)).catch(&ctx) {
            Ok(result) if intercepted && is_interceptor && result.as_bool() != Some(false) => {
               taken = true;
            },
            Ok(_) => {},
            Err(err) => log::error!("[{plugin}] onEvent({event_name}): {err}"),
         }
      }
      taken
   })
}

/// Runs `phoenix.ui.onNode` watchers for a newly created node.
pub fn node_created(engine: &Engine, watcher_ids: &[u64], info: &NodeInfo) {
   engine.with(|ctx| {
      let Some(registry) = ctx.userdata::<Mobile<'_>>() else {
         return;
      };
      let handlers = {
         let watchers = registry.node_watchers.borrow();
         watcher_ids
            .iter()
            .filter_map(|id| {
               let (plugin, function) = watchers.get(id)?;
               Some((plugin.clone(), function.clone()))
            })
            .collect::<Vec<_>>()
      };
      drop(registry);
      if handlers.is_empty() {
         return;
      }
      let Ok(obj) = node_info_object(&ctx, info) else {
         return;
      };
      for (plugin, function) in handlers {
         if let Err(err) = function.call::<_, Value<'_>>((obj.clone(),)).catch(&ctx) {
            log::error!("[{plugin}] onNode: {err}");
         }
      }
   });
}

pub fn add_view_hooks<'js>(ctx: &Ctx<'js>, plugin: &str, ui: &Object<'js>) -> rquickjs::Result<()> {
   let owner = plugin.to_owned();
   ui.set(
      "hide",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, aid: String| {
         let handle = patches::hidden::hide(owner.clone(), aid);
         remover(&ctx, move |_| patches::hidden::unhide(handle), None)
      })?,
   )?;
   let owner = plugin.to_owned();
   ui.set(
      "onEvent",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, event: String, handler: Function<'js>, options: Opt<Object<'js>>| {
            on_view_event(&ctx, &owner, &event, handler, options.0)
         },
      )?,
   )?;
   ui.set(
      "dispatch",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, node: i32, event: String, payload: Opt<Value<'js>>| {
            let prop = phoenix_ui_mobile::Prop::from_str(&event).map_err(|_| {
               Exception::throw_type(&ctx, &format!("unknown Phoenix event {event:?}"))
            })?;
            let wire = match payload.0 {
               Some(value) if !value.is_undefined() && !value.is_null() => {
                  js_to_wire(&ctx, &value)?
               },
               _ => phoenix_wire::Value::Obj(Vec::new()),
            };
            Ok::<_, rquickjs::Error>(patches::events::dispatch_event(node, prop.key(), &wire))
         },
      )?,
   )?;
   let owner = plugin.to_owned();
   ui.set(
      "onNode",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, matcher: Value<'js>, handler: Function<'js>| {
            let matcher = parse_node_match(&ctx, &matcher)?;
            on_node_created(&ctx, &owner, matcher, handler)
         },
      )?,
   )?;
   let owner = plugin.to_owned();
   ui.set(
      "patchProps",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>,
               matcher: Value<'js>,
               patch: Object<'js>,
               options: Opt<Object<'js>>| {
            let matcher = parse_node_match(&ctx, &matcher)?;
            let add_on = patch
               .get::<_, Option<Vec<String>>>("addOn")?
               .unwrap_or_default();
            let props = parse_props(&ctx, &patch, Some("addOn"))?;
            let (target, once, timeout) = patch_options(&ctx, options.0)?;
            let handle = patches::props::add_prop_patch(
               owner.clone(),
               matcher,
               target,
               props,
               add_on,
               once,
               timeout,
            );
            remover(
               &ctx,
               move |_| patches::props::remove_prop_patch(handle),
               None,
            )
         },
      )?,
   )?;
   Ok(())
}

fn patch_options<'js>(
   ctx: &Ctx<'js>,
   options: Option<Object<'js>>,
) -> rquickjs::Result<(PatchTarget, bool, Option<std::time::Duration>)> {
   Ok(match options {
      Some(opts) => {
         let target = match opts.get::<_, Option<String>>("target")?.as_deref() {
            Some("sheet") => PatchTarget::Sheet,
            Some("list") => PatchTarget::List,
            Some("node") | None => PatchTarget::Node,
            Some(other) => {
               return Err(Exception::throw_type(
                  ctx,
                  &format!("unknown patchProps target {other:?}"),
               ));
            },
         };
         let once = opts.get::<_, Option<bool>>("once")?.unwrap_or(false);
         let timeout = opts
            .get::<_, Option<f64>>("timeoutMs")?
            .filter(|ms| *ms > 0.0)
            .map(|ms| std::time::Duration::from_millis(millis(ms)));
         (target, once, timeout)
      },
      None => (PatchTarget::Node, false, None),
   })
}

pub fn add_view_queries<'js>(ctx: &Ctx<'js>, ui: &Object<'js>) -> rquickjs::Result<()> {
   ui.set(
      "update",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, node: i32, patch: Object<'js>| {
            let props = parse_props(&ctx, &patch, None)?;
            patches::nodes::update_node(node, props);
            Ok::<_, rquickjs::Error>(())
         },
      )?,
   )?;
   ui.set(
      "find",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, matcher: Value<'js>| {
         let matcher = parse_node_match(&ctx, &matcher)?;
         Ok::<_, rquickjs::Error>(patches::nodes::find_nodes(&matcher))
      })?,
   )?;
   ui.set(
      "findDown",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, root: i32, matcher: Value<'js>, depth: Opt<usize>| {
            let matcher = parse_node_match(&ctx, &matcher)?;
            Ok::<_, rquickjs::Error>(patches::nodes::find_down(
               root,
               &matcher,
               depth.0.unwrap_or(8),
            ))
         },
      )?,
   )?;
   ui.set(
      "node",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, id: i32| -> rquickjs::Result<Value<'js>> {
            match patches::nodes::node_info(id) {
               Some(info) => Ok(node_info_object(&ctx, &info)?.into_value()),
               None => Ok(Value::new_null(ctx)),
            }
         },
      )?,
   )?;
   ui.set("root", Function::new(ctx.clone(), patches::root_id)?)?;
   Ok(())
}

fn on_view_event<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   event: &str,
   function: Function<'js>,
   options: Option<Object<'js>>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

   let prop = phoenix_ui_mobile::Prop::from_str(event)
      .map_err(|_| Exception::throw_type(ctx, &format!("unknown Phoenix event {event:?}")))?;
   let intercept = options
      .and_then(|opts| opts.get::<_, Option<bool>>("intercept").ok().flatten())
      .unwrap_or(false);
   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   let key = prop.key();
   let registry = ctx
      .userdata::<Mobile<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   registry.event_hooks.borrow_mut().push(EventHook {
      id,
      plugin: plugin.to_owned(),
      key,
      intercept,
      function,
   });
   drop(registry);
   patches::events::add_event_hook(id, plugin.to_owned(), key, intercept);
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Mobile<'js>>() {
            registry
               .event_hooks
               .borrow_mut()
               .retain(|hook| hook.id != id);
         }
         patches::events::remove_event_hook(id);
      },
      None,
   )
}

fn on_node_created<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   matcher: NodeMatch,
   function: Function<'js>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   let registry = ctx
      .userdata::<Mobile<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   registry
      .node_watchers
      .borrow_mut()
      .insert(id, (plugin.to_owned(), function));
   drop(registry);
   patches::nodes::add_node_watcher(id, plugin.to_owned(), matcher);
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Mobile<'js>>() {
            registry.node_watchers.borrow_mut().remove(&id);
         }
         patches::nodes::remove_node_watcher(id);
      },
      None,
   )
}

fn parse_node_match<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> rquickjs::Result<NodeMatch> {
   if let Some(text) = value.as_string() {
      return Ok(NodeMatch::Exact(text.to_string()?));
   }
   if let Some(obj) = value.as_object() {
      if let Some(exact) = obj.get::<_, Option<String>>("exact")? {
         return Ok(NodeMatch::Exact(exact));
      }
      if let Some(prefix) = obj.get::<_, Option<String>>("prefix")? {
         return Ok(NodeMatch::Prefix(prefix));
      }
   }
   Err(Exception::throw_type(
      ctx,
      "expected accessibilityId string or { exact | prefix }",
   ))
}

pub fn node_info_object<'js>(ctx: &Ctx<'js>, info: &NodeInfo) -> rquickjs::Result<Object<'js>> {
   let obj = Object::new(ctx.clone())?;
   obj.set("id", info.id)?;
   obj.set("kind", info.kind.clone())?;
   obj.set("accessibilityId", info.accessibility_id.clone())?;
   obj.set("parent", info.parent)?;
   obj.set("children", info.children.clone())?;
   obj.set("sheet", info.sheet)?;
   obj.set("list", info.list)?;
   obj.set("on", info.on.clone())?;
   Ok(obj)
}
