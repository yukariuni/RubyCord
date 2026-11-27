//! `phoenix.ui` contributions, sheets, banners and the clipboard, and the
//! `ctx` object plugin callbacks receive.

use std::collections::HashMap;

use phoenix_runtime::{
   js::{
      Engine,
      background::{
         Settlement,
         in_background,
      },
   },
   local,
};
use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   Object,
   Value,
};
use serde_json::value::RawValue;

use crate::{
   js::{
      Mobile,
      mount::add_mount,
      page::{
         add_page,
         page_of,
      },
      view::{
         add_view_hooks,
         add_view_queries,
      },
   },
   patches::{
      self,
      Contribution,
      ContributionId,
      Outcome,
      Place,
      Rendered,
      TintChange,
      UiContext,
      targets::Target,
   },
};

/// A contribution's plugin callbacks. `evaluate` is `render`, or `visible`
/// for a message action.
pub struct Callbacks<'js> {
   pub plugin: String,
   target:     Target,
   press:      Option<Function<'js>>,
   evaluate:   Option<Function<'js>>,
   /// A profile tab's page callbacks, by profile, as `Block` handlers index
   /// them.
   pub pages:  HashMap<Place, Vec<Function<'js>>>,
}

/// Asks contribution `id` what it shows at `place`. A promise answers
/// later through [`patches::evaluated`], and this returns `None`.
pub fn ui_evaluate(
   engine: &Engine,
   id: ContributionId,
   place: &Place,
   context: &UiContext,
) -> Option<Outcome> {
   engine.with(|ctx| {
      let (plugin, target, evaluate) = {
         let registry = ctx.userdata::<Mobile<'_>>()?;
         let callbacks = registry.ui.borrow();
         let callbacks = callbacks.get(&id)?;
         (
            callbacks.plugin.clone(),
            callbacks.target,
            callbacks.evaluate.clone()?,
         )
      };
      let answer = |value| {
         if target == Target::Tab {
            page_of(&ctx, id, place, value)
         } else {
            outcome_of(target, value)
         }
      };
      let failed = || answer(None);
      let argument = match context_object(&ctx, context) {
         Ok(argument) => argument,
         Err(err) => {
            log::error!("[{plugin}] ui context: {err}");
            return Some(failed());
         },
      };
      let result = match evaluate.call::<_, Value<'_>>((argument,)).catch(&ctx) {
         Ok(result) => result,
         Err(err) => {
            log::error!("[{plugin}] render: {err}");
            return Some(failed());
         },
      };
      let Some(promise) = result.as_promise() else {
         return Some(answer(Some(&result)));
      };
      let settle = |place: Place, plugin: String| {
         let settled = place.clone();
         let ok = Function::new(ctx.clone(), move |value: Value<'_>| {
            let outcome = if target == Target::Tab {
               page_of(value.ctx(), id, &settled, Some(&value))
            } else {
               outcome_of(target, Some(&value))
            };
            patches::evaluated(id, settled.clone(), outcome);
         });
         let err = Function::new(ctx.clone(), move |reason: Value<'_>| {
            log::error!("[{plugin}] render: {reason:?}");
            let outcome = if target == Target::Tab {
               page_of(reason.ctx(), id, &place, None)
            } else {
               outcome_of(target, None)
            };
            patches::evaluated(id, place.clone(), outcome);
         });
         let then = promise.then()?;
         then.call::<_, ()>((rquickjs::function::This(promise.clone()), ok?, err?))
      };
      if let Err(err) = settle(place.clone(), plugin.clone()).catch(&ctx) {
         log::error!("[{plugin}] render: {err}");
         return Some(failed());
      }
      None
   })
}

pub fn ui_press(engine: &Engine, id: ContributionId, context: &UiContext) {
   engine.with(|ctx| {
      let Some((plugin, press)) = ctx.userdata::<Mobile<'_>>().and_then(|registry| {
         let callbacks = registry.ui.borrow();
         let callbacks = callbacks.get(&id)?;
         Some((callbacks.plugin.clone(), callbacks.press.clone()?))
      }) else {
         return;
      };
      let run = || -> rquickjs::Result<()> {
         press.call::<_, Value<'_>>((context_object(&ctx, context)?,))?;
         Ok(())
      };
      if let Err(err) = run().catch(&ctx) {
         log::error!("[{plugin}] onPress: {err}");
      }
   });
}

/// Runs the tap callback for in-app notification banner `id` and cleans it
/// up.
pub fn banner_press(engine: &Engine, id: u32) {
   engine.with(|ctx| {
      let Some((plugin, handler)) = ctx
         .userdata::<Mobile<'_>>()
         .and_then(|registry| registry.banners.borrow_mut().remove(&id))
      else {
         return;
      };
      if let Err(err) = handler.call::<_, Value<'_>>(()).catch(&ctx) {
         log::error!("[{plugin}] notify onPress: {err}");
      }
   });
}

/// Drops the tap callback for a dismissed in-app notification banner.
pub fn banner_closed(engine: &Engine, id: u32) {
   engine.with(|ctx| {
      if let Some(registry) = ctx.userdata::<Mobile<'_>>() {
         registry.banners.borrow_mut().remove(&id);
      }
   });
}

/// The `ctx` a plugin callback receives.
pub fn context_object<'js>(ctx: &Ctx<'js>, context: &UiContext) -> rquickjs::Result<Object<'js>> {
   let object = Object::new(ctx.clone())?;
   object.set("channelId", context.channel_id.clone())?;
   object.set("currentUserId", context.current_user_id.clone())?;
   if let Some(id) = &context.message_id {
      object.set("messageId", id.as_str())?;
   }
   if let Some(id) = &context.user_id {
      object.set("userId", id.as_str())?;
   }
   if let Some(message) = &context.message {
      object.set("message", ctx.json_parse(message.as_str())?)?;
   }
   let channel = context.channel_id.clone();
   object.set(
      "reply",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, text: String| {
         let channel = channel
            .as_deref()
            .ok_or_else(|| Exception::throw_message(&ctx, "no open channel to reply in"))?;
         let embeds = RawValue::from_string("[]".to_owned())
            .map_err(|err| Exception::throw_internal(&ctx, &err.to_string()))?;
         let options = local::Options {
            author:   "Phoenix",
            guild_id: None,
            embeds:   &embeds,
         };
         local::add(channel, &text, &options)
            .ok_or_else(|| Exception::throw_message(&ctx, "this build has no MESSAGE_CREATE"))
      })?,
   )?;
   Ok(object)
}

/// A `render` or `visible` result as the patches draw it.
fn outcome_of(target: Target, value: Option<&Value<'_>>) -> Outcome {
   if target == Target::Action {
      return Outcome::Shown(value.is_some_and(|value| {
         value
            .as_bool()
            .unwrap_or_else(|| !value.is_null() && !value.is_undefined())
      }));
   }
   let rendered = value.and_then(|value| {
      if value.is_null() || value.is_undefined() || value.as_bool() == Some(false) {
         return None;
      }
      if let Some(text) = value.as_string() {
         return Some(Rendered {
            text:  text.to_string().ok()?,
            color: None,
            tint:  None,
            icons: Vec::new(),
         });
      }
      if let Some(object) = value.as_object() {
         let argb = |key: &str| {
            object
               .get::<_, Option<f64>>(key)
               .ok()
               .flatten()
               .map(|color| {
                  #[expect(clippy::cast_possible_truncation, reason = "colors are 32-bit ARGB")]
                  let color = color as i64;
                  color
               })
         };
         return Some(Rendered {
            text:  object
               .get::<_, Option<String>>("text")
               .ok()
               .flatten()
               .unwrap_or_default(),
            color: argb("color"),
            tint:  argb("tint"),
            // Phoenix's images take no SVG, so only icons with a `uri` draw.
            icons: object
               .get::<_, Option<Vec<Object<'_>>>>("icons")
               .ok()
               .flatten()
               .unwrap_or_default()
               .iter()
               .filter_map(|icon| icon.get::<_, Option<String>>("uri").ok().flatten())
               .collect(),
         });
      }
      Some(Rendered {
         text:  value.as_number()?.to_string(),
         color: None,
         tint:  None,
         icons: Vec::new(),
      })
   });
   Outcome::Text(rendered)
}

/// A function that undoes a registration, with `update` to redraw it. For a
/// message accessory or profile tab, `update(id)` redraws one message or
/// profile.
pub fn remover<'js>(
   ctx: &Ctx<'js>,
   remove: impl Fn(&Ctx<'js>) + 'js,
   update: Option<(ContributionId, Target)>,
) -> rquickjs::Result<Function<'js>> {
   let function = Function::new(ctx.clone(), move |ctx: Ctx<'js>| remove(&ctx))?;
   if let Some((id, target)) = update {
      function.set(
         "update",
         Function::new(
            ctx.clone(),
            move |arg: rquickjs::function::Opt<Value<'_>>| {
               if matches!(target, Target::Header | Target::Composer)
                  && let Some(object) = arg.0.as_ref().and_then(Value::as_object)
               {
                  let icon = object.get::<_, Option<String>>("icon").ok().flatten();
                  let label = object.get::<_, Option<String>>("label").ok().flatten();
                  let tint = match object.get::<_, Value<'_>>("tint") {
                     Ok(value) if value.is_null() => TintChange::Clear,
                     Ok(value) => {
                        value
                           .as_number()
                           .map_or(TintChange::Keep, |color| TintChange::Set(to_argb(color)))
                     },
                     Err(_) => TintChange::Keep,
                  };
                  patches::update_button(id, icon, label, tint);
                  return;
               }
               let at = arg
                  .0
                  .and_then(|value| value.as_string().and_then(|text| text.to_string().ok()));
               if target.decorates()
                  && let Some(user) = &at
               {
                  patches::update_user(id, user);
                  return;
               }
               let place = at.map(|at| {
                  if target == Target::Tab {
                     Place::User(Some(at))
                  } else {
                     Place::Message(at)
                  }
               });
               patches::update(id, place);
            },
         )?,
      )?;
   }
   Ok(function)
}

/// `phoenix.ui`, the UI patch targets.
pub fn ui_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let ui = Object::new(ctx.clone())?;
   for (method, target) in [
      ("addHeaderButton", Target::Header),
      ("addComposerButton", Target::Composer),
      ("addMessageAccessory", Target::Content),
      ("addMessageAction", Target::Action),
      ("addProfileSection", Target::Profile),
      ("addProfileTab", Target::Tab),
      ("addMemberDecoration", Target::Member),
      ("addMessageHeaderDecoration", Target::Author),
   ] {
      let name = plugin.to_owned();
      ui.set(
         method,
         Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
            add_contribution(&ctx, &name, target, &options)
         })?,
      )?;
   }
   add_sheets(ctx, &ui)?;
   add_page(ctx, plugin, &ui)?;
   add_view_hooks(ctx, plugin, &ui)?;
   add_view_queries(ctx, &ui)?;
   add_mount(ctx, plugin, &ui)?;
   add_notify(ctx, plugin, &ui)?;
   let owner = plugin.to_owned();
   ui.set(
      "disableHaptics",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         let handle = crate::bridge::disable_haptics(owner.clone());
         remover(&ctx, move |_| crate::bridge::enable_haptics(handle), None)
      })?,
   )?;
   ui.set(
      "copy",
      Function::new(ctx.clone(), |text: String| {
         if let Err(err) = crate::picker::copy(&text) {
            log::warn!("copy: {err}");
         }
      })?,
   )?;
   ui.set(
      "toast",
      Function::new(ctx.clone(), |text: String| {
         if let Err(err) = crate::picker::toast(&text) {
            log::warn!("toast: {err}");
         }
      })?,
   )?;
   Ok(ui)
}

fn add_sheets<'js>(ctx: &Ctx<'js>, ui: &Object<'js>) -> rquickjs::Result<()> {
   ui.set(
      "choose",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, options: Object<'js>| {
         let title = options.get::<_, Option<String>>("title")?;
         let message = options.get::<_, Option<String>>("message")?;
         let items = parse_menu_items(&options)?;
         if message.is_none() && items.is_empty() {
            return Err(Exception::throw_type(
               &ctx,
               "choose needs options or a message",
            ));
         }
         in_background(&ctx, move || {
            let body = message.map(|text| {
               patches::menu::SheetBody {
                  text,
                  monospace: false,
                  highlight: false,
               }
            });
            Settlement::Int(patches::menu::show(title, body, items).ok_or_else(not_drawn))
         })
      })?,
   )?;
   ui.set(
      "showSheet",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, options: Object<'js>| {
         let title = options.get::<_, Option<String>>("title")?;
         let text = options.get::<_, String>("text")?;
         let monospace = options
            .get::<_, Option<bool>>("monospace")?
            .unwrap_or_default();
         let highlight = options
            .get::<_, Option<bool>>("highlight")?
            .unwrap_or_default();
         let items = parse_menu_items(&options)?;
         in_background(&ctx, move || {
            let body = patches::menu::SheetBody {
               text,
               monospace,
               highlight,
            };
            Settlement::Int(patches::menu::show(title, Some(body), items).ok_or_else(not_drawn))
         })
      })?,
   )?;
   Ok(())
}

fn not_drawn() -> String {
   "Phoenix's view tree is not up yet".to_owned()
}

fn add_notify<'js>(ctx: &Ctx<'js>, plugin: &str, ui: &Object<'js>) -> rquickjs::Result<()> {
   let owner = plugin.to_owned();
   ui.set(
      "notify",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         let title = options
            .get::<_, Option<String>>("title")?
            .filter(|text| !text.is_empty())
            .ok_or_else(|| Exception::throw_type(&ctx, "notify({ title }) requires a title"))?;
         let body = options.get::<_, Option<String>>("body")?;
         let icon = options.get::<_, Option<String>>("icon")?;
         let accent = options.get::<_, Option<f64>>("accent")?.map(to_argb);
         let duration_ms = options
            .get::<_, Option<f64>>("durationMs")?
            .map_or(5000, |ms| if ms <= 0.0 { 0 } else { millis(ms) });
         let on_press = options.get::<_, Option<Function<'js>>>("onPress")?;
         let action = if on_press.is_some() {
            patches::banners::BannerAction::JsCallback
         } else {
            patches::banners::BannerAction::None
         };
         let id = patches::banners::show_banner(title, body, icon, accent, duration_ms, action);
         if let Some(callback) = on_press {
            ctx.userdata::<Mobile<'js>>()
               .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
               .banners
               .borrow_mut()
               .insert(id, (owner.clone(), callback));
         }
         remover(&ctx, move |_| patches::banners::dismiss_banner(id), None)
      })?,
   )?;
   Ok(())
}

fn parse_menu_items<'js>(options: &Object<'js>) -> rquickjs::Result<Vec<patches::menu::MenuItem>> {
   Ok(options
      .get::<_, Option<Vec<Value<'js>>>>("options")?
      .unwrap_or_default()
      .into_iter()
      .filter_map(|option| {
         if let Some(label) = option.as_string() {
            return Some(patches::menu::MenuItem {
               label:  label.to_string().ok()?,
               icon:   None,
               danger: false,
            });
         }
         let option = option.as_object()?;
         Some(patches::menu::MenuItem {
            label:  option.get::<_, Option<String>>("label").ok().flatten()?,
            icon:   option.get::<_, Option<String>>("icon").ok().flatten(),
            danger: option
               .get::<_, Option<bool>>("danger")
               .ok()
               .flatten()
               .unwrap_or_default(),
         })
      })
      .collect())
}

fn add_contribution<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   target: Target,
   options: &Object<'js>,
) -> rquickjs::Result<Function<'js>> {
   let placement = options.get::<_, Option<String>>("placement")?;
   let target = match (target, placement.as_deref()) {
      (Target::Content, Some("header")) => Target::Timestamp,
      _ => target,
   };
   let press = options.get::<_, Option<Function<'js>>>("onPress")?;
   let evaluate = if target == Target::Action {
      options.get::<_, Option<Function<'js>>>("visible")?
   } else {
      options.get::<_, Option<Function<'js>>>("render")?
   };
   let label = match target {
      Target::Profile | Target::Tab => options.get::<_, Option<String>>("title")?,
      _ => options.get::<_, Option<String>>("label")?,
   };
   let needs = match target {
      Target::Header | Target::Composer | Target::Action => (label.is_some(), "label and onPress"),
      Target::Content | Target::Timestamp | Target::Member | Target::Author => {
         (evaluate.is_some(), "render")
      },
      Target::Profile | Target::Tab | Target::TabPage => {
         (label.is_some() && evaluate.is_some(), "title and render")
      },
   };
   let missing_press =
      matches!(target, Target::Header | Target::Composer | Target::Action) && press.is_none();
   if !needs.0 || missing_press {
      return Err(Exception::throw_type(
         ctx,
         &format!("expected an object with {}", needs.1),
      ));
   }

   let id = ContributionId::next();
   let conditional = target == Target::Action && evaluate.is_some();
   ctx.userdata::<Mobile<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .ui
      .borrow_mut()
      .insert(id, Callbacks {
         plugin: plugin.to_owned(),
         target,
         press,
         evaluate,
         pages: HashMap::new(),
      });
   patches::add(Contribution {
      id,
      plugin: plugin.to_owned(),
      target,
      label: label.unwrap_or_default(),
      icon: options.get("icon")?,
      tint: options.get::<_, Option<f64>>("tint")?.map(to_argb),
      danger: options
         .get::<_, Option<bool>>("danger")?
         .unwrap_or_default(),
      priority: options
         .get::<_, Option<i32>>("priority")?
         .unwrap_or_default(),
      conditional,
   });
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Mobile<'js>>() {
            registry.ui.borrow_mut().remove(&id);
         }
         patches::remove(id);
      },
      Some((id, target)),
   )
}

#[expect(clippy::cast_possible_truncation, reason = "colors are 32-bit ARGB")]
const fn to_argb(color: f64) -> i64 {
   color as i64
}

#[expect(
   clippy::cast_possible_truncation,
   clippy::cast_sign_loss,
   reason = "callers pass positive milliseconds, and `as` saturates the rest"
)]
pub const fn millis(ms: f64) -> u64 {
   ms as u64
}
