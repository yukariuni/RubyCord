//! `phoenix.ui.mount`, subtrees plugins build and attach to the view tree.

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
      ui::remover,
      wire::parse_props,
   },
   patches,
};

/// Runs callback `index` on raw JS-mounted subtree `mount`.
pub fn mount_callback(engine: &Engine, mount: u32, index: u32) {
   engine.with(|ctx| {
      let Some((plugin, handler)) = ctx.userdata::<Mobile<'_>>().and_then(|registry| {
         let mounts = registry.mounts.borrow();
         let (plugin, handlers) = mounts.get(&mount)?;
         let handler = handlers.get(usize::try_from(index).ok()?)?;
         Some((plugin.clone(), handler.clone()))
      }) else {
         return;
      };
      if let Err(err) = handler.call::<_, Value<'_>>(()).catch(&ctx) {
         log::error!("[{plugin}] mount callback: {err}");
      }
   });
}

/// Drops callbacks for an unmounted raw JS subtree.
pub fn mount_closed(engine: &Engine, mount: u32) {
   engine.with(|ctx| {
      if let Some(registry) = ctx.userdata::<Mobile<'_>>() {
         registry.mounts.borrow_mut().remove(&mount);
      }
   });
}

pub fn add_mount<'js>(ctx: &Ctx<'js>, plugin: &str, ui: &Object<'js>) -> rquickjs::Result<()> {
   let owner = plugin.to_owned();
   ui.set(
      "mount",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, parent: Value<'js>, node: Object<'js>, options: Opt<Object<'js>>| {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
            let parent_id = if parent
               .as_string()
               .is_some_and(|s| s.to_string().ok().as_deref() == Some("root"))
               || parent.is_null()
               || parent.is_undefined()
            {
               None
            } else if let Some(id) = parent.as_int() {
               Some(id)
            } else if let Some(num) = parent.as_float() {
               #[expect(
                  clippy::cast_possible_truncation,
                  reason = "Phoenix node ids fit in i32"
               )]
               Some(num as i32)
            } else {
               return Err(Exception::throw_type(
                  &ctx,
                  "mount(parent, node) expects a node id or \"root\"",
               ));
            };
            let before = match options
               .0
               .and_then(|opts| opts.get::<_, Value<'js>>("before").ok())
            {
               Some(val)
                  if val
                     .as_string()
                     .is_some_and(|s| s.to_string().ok().as_deref() == Some("start")) =>
               {
                  patches::mount::MountBefore::Start
               },
               Some(val) if let Some(sib) = val.as_int() => patches::mount::MountBefore::Node(sib),
               Some(val) if let Some(num) = val.as_float() =>
               {
                  #[expect(
                     clippy::cast_possible_truncation,
                     reason = "Phoenix node ids fit in i32"
                  )]
                  patches::mount::MountBefore::Node(num as i32)
               },
               _ => patches::mount::MountBefore::End,
            };
            let mut handlers = Vec::new();
            let parsed = parse_mount_node(&ctx, &node, &mut handlers)?;
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            ctx.userdata::<Mobile<'js>>()
               .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
               .mounts
               .borrow_mut()
               .insert(id, (owner.clone(), handlers));
            patches::mount::mount(id, parent_id, before, parsed);
            remover(&ctx, move |_| patches::mount::unmount(id), None)
         },
      )?,
   )?;
   Ok(())
}

fn parse_mount_node<'js>(
   ctx: &Ctx<'js>,
   object: &Object<'js>,
   handlers: &mut Vec<Function<'js>>,
) -> rquickjs::Result<patches::mount::MountNode> {
   let kind_str = object
      .get::<_, Option<String>>("kind")?
      .unwrap_or_else(|| "view".to_owned());
   let mut props = match object.get::<_, Option<Object<'js>>>("props")? {
      Some(props_obj) => parse_props(ctx, &props_obj, None)?,
      None => Vec::new(),
   };
   let on_press = object
      .get::<_, Option<Function<'js>>>("onPress")?
      .map(|cb| {
         #[expect(
            clippy::cast_possible_truncation,
            reason = "mounted trees have few callbacks"
         )]
         let idx = handlers.len() as u32;
         handlers.push(cb);
         // Automatically wire `on: ["press"]` if the caller didn't add it
         // manually.
         let has_press = props.iter().any(|(k, v)| {
            *k == phoenix_ui_mobile::Prop::ON
               && matches!(v, phoenix_wire::Value::Arr(arr) if arr.iter().any(|x| x.as_str() == Some("press")))
         });
         if !has_press {
            let mut on = props
               .iter()
               .find(|(k, _)| *k == phoenix_ui_mobile::Prop::ON)
               .and_then(|(_, v)| {
                  match v {
                     phoenix_wire::Value::Arr(arr) => Some(arr.clone()),
                     _ => None,
                  }
               })
               .unwrap_or_default();
            on.push(phoenix_wire::Value::Str("press".to_owned()));
            phoenix_ui_mobile::set_prop(
               &mut props,
               phoenix_ui_mobile::Prop::ON,
               Some(phoenix_wire::Value::Arr(on)),
            );
         }
         idx
      });
   let on_dismiss = object
      .get::<_, Option<Function<'js>>>("onDismiss")?
      .map(|cb| {
         #[expect(
            clippy::cast_possible_truncation,
            reason = "mounted trees have few callbacks"
         )]
         let idx = handlers.len() as u32;
         handlers.push(cb);
         idx
      });
   let kids = match object.get::<_, Option<Vec<Object<'js>>>>("children")? {
      Some(raw_kids) => {
         raw_kids
            .iter()
            .map(|kid| parse_mount_node(ctx, kid, handlers))
            .collect::<rquickjs::Result<Vec<_>>>()?
      },
      None => Vec::new(),
   };
   Ok(patches::mount::MountNode {
      kind: kind_str.into(),
      props,
      kids,
      on_press,
      on_dismiss,
   })
}
