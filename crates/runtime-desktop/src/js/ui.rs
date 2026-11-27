//! Desktop `phoenix.ui` bindings backed by [`phoenix_ui_desktop`] and native
//! GPUI message accessories ([`crate::accessories`]).
//!
//! Native clipboard, toasts, notifications, and `addMessageAccessory` run live
//! on Desktop Phoenix. Remaining mobile-only UI-op slot hooks return harmless
//! no-op removers so cross-platform plugins load cleanly without throwing.

use std::{
   cell::RefCell,
   rc::Rc,
   sync::{
      Arc,
      atomic::{AtomicU64, Ordering},
   },
   time::Duration,
};

use phoenix_runtime::{
   discord::RequestAction,
   js::{
      Engine, Registry,
      background::{Settlement, in_background},
   },
   store::{self, Cell},
};
use rquickjs::{
   Array, CatchResultExt, Ctx, Exception, Function, Object, Value,
   function::{Opt, This},
};

#[cfg(target_arch = "x86_64")]
use crate::counts::{self, Count};
use crate::{
   accessories::{self, RenderedAccessory},
   composer,
   context_menu::{self, ActionMeta},
   double_click, emoji, icons,
   js::Desktop,
   members::Place,
   modal::{self, ModalButton, ModalSection, PageModalSpec, Prompt},
};

#[derive(Clone)]
pub struct DesktopAccessory<'js> {
   pub id: u64,
   pub plugin: String,
   pub priority: i32,
   pub render: Function<'js>,
}

/// A `phoenix.ui.addMemberDecoration` or `addMessageHeaderDecoration`
/// callback.
#[derive(Clone)]
pub struct DesktopMember<'js> {
   pub id: u64,
   pub plugin: String,
   pub priority: i32,
   pub render: Function<'js>,
}

#[derive(Clone)]
pub struct DesktopAction<'js> {
   pub id: u64,
   pub plugin: String,
   pub label: String,
   pub icon: Option<String>,
   pub danger: bool,
   pub priority: i32,
   pub visible: Option<Function<'js>>,
   pub on_press: Function<'js>,
}

impl DesktopAction<'_> {
   pub fn meta(&self) -> ActionMeta {
      ActionMeta {
         id: self.id,
         label: self.label.clone(),
         icon: self.icon.clone(),
         danger: self.danger,
         priority: self.priority,
         has_visible: self.visible.is_some(),
      }
   }
}

fn noop_handle<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
   let remove = Function::new(ctx.clone(), || {})?;
   remove.set("update", Function::new(ctx.clone(), || {})?)?;
   remove.set("close", Function::new(ctx.clone(), || {})?)?;
   Ok(remove)
}

fn parse_rendered(value: &Value<'_>, priority: i32) -> Option<RenderedAccessory> {
   if value.is_null() || value.is_undefined() || value.as_bool() == Some(false) {
      return None;
   }
   if let Some(s) = value.as_string() {
      let text = s.to_string().ok()?;
      if text.is_empty() {
         return None;
      }
      return Some(RenderedAccessory {
         priority,
         text,
         color: None,
         tint: None,
         icons: Vec::new(),
      });
   }
   if let Some(obj) = value.as_object() {
      let text = obj
         .get::<_, Option<String>>("text")
         .ok()
         .flatten()
         .unwrap_or_default();
      let color = obj
         .get::<_, Option<f64>>("color")
         .ok()
         .flatten()
         .map(to_argb);
      let tint = obj
         .get::<_, Option<f64>>("tint")
         .ok()
         .flatten()
         .map(to_argb);
      let icons = parse_icons(obj);
      if text.is_empty() && tint.is_none() && icons.is_empty() {
         return None;
      }
      return Some(RenderedAccessory {
         priority,
         text,
         color,
         tint,
         icons,
      });
   }
   None
}

/// `icons: [{ svg, color }]`, registered for drawing. Empty where icons cannot
/// be drawn, which leaves the text.
fn parse_icons(obj: &Object<'_>) -> Vec<(&'static str, Option<u32>)> {
   let Ok(Some(list)) = obj.get::<_, Option<Array<'_>>>("icons") else {
      return Vec::new();
   };
   list
      .iter::<Object<'_>>()
      .filter_map(Result::ok)
      .take(8)
      .filter_map(|icon| {
         let svg = icon.get::<_, String>("svg").ok()?;
         let color = icon
            .get::<_, Option<f64>>("color")
            .ok()
            .flatten()
            .map(to_argb);
         Some((icons::register(&svg)?, color))
      })
      .collect()
}

fn evaluate_accessories_in_ctx<'js>(ctx: &Ctx<'js>, message_id: u64) -> Vec<RenderedAccessory> {
   let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
      return Vec::new();
   };
   let snapshot: Vec<DesktopAccessory<'js>> = registry.accessories.borrow().clone();
   drop(registry);
   if snapshot.is_empty() {
      return Vec::new();
   }

   let id_str = message_id.to_string();
   let Ok(arg) = Object::new(ctx.clone()) else {
      return Vec::new();
   };
   let _ = arg.set("messageId", id_str.as_str());
   let _ = arg.set("channelId", Value::new_null(ctx.clone()));
   let _ = arg.set("currentUserId", Value::new_null(ctx.clone()));
   if let Ok(reply) = Function::new(ctx.clone(), String::new) {
      let _ = arg.set("reply", reply);
   }

   let mut out = Vec::new();
   for acc in snapshot {
      match acc.render.call::<_, Value<'_>>((arg.clone(),)).catch(ctx) {
         Ok(val) if val.is_promise() => {
            if let Some(promise) = val.as_object()
               && let Ok(then) = promise.get::<_, Function<'_>>("then")
            {
               let mid = message_id;
               let prio = acc.priority;
               if let Ok(on_resolve) =
                  Function::new(ctx.clone(), move |ctx: Ctx<'js>, resolved: Value<'js>| {
                     if let Some(item) = parse_rendered(&resolved, prio) {
                        let mut all = evaluate_sync_only(&ctx, mid, Some((prio, item)));
                        all.sort_by_key(|a| a.priority);
                        accessories::set_rendered(mid, all);
                     }
                  })
               {
                  let _ = then.call::<_, Value<'_>>((on_resolve,));
               }
            }
         },
         Ok(val) => {
            if let Some(rendered) = parse_rendered(&val, acc.priority) {
               out.push(rendered);
            }
         },
         Err(err) => {
            log::error!("[{}] addMessageAccessory render: {err}", acc.plugin);
         },
      }
   }
   out
}

fn evaluate_sync_only<'js>(
   ctx: &Ctx<'js>,
   message_id: u64,
   extra: Option<(i32, RenderedAccessory)>,
) -> Vec<RenderedAccessory> {
   let mut out = Vec::new();
   if let Some((_, item)) = extra {
      out.push(item);
   }
   let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
      return out;
   };
   let snapshot: Vec<DesktopAccessory<'js>> = registry.accessories.borrow().clone();
   drop(registry);

   let id_str = message_id.to_string();
   let Ok(arg) = Object::new(ctx.clone()) else {
      return out;
   };
   let _ = arg.set("messageId", id_str.as_str());
   let _ = arg.set("channelId", Value::new_null(ctx.clone()));
   let _ = arg.set("currentUserId", Value::new_null(ctx.clone()));

   for acc in snapshot {
      if let Ok(val) = acc.render.call::<_, Value<'_>>((arg.clone(),)).catch(ctx)
         && !val.is_promise()
         && let Some(rendered) = parse_rendered(&val, acc.priority)
         && !out.contains(&rendered)
      {
         out.push(rendered);
      }
   }
   out
}

/// Evaluates every registered desktop `addMessageAccessory` for `message_id`.
pub fn render_accessories(engine: &Engine, message_id: u64) -> Vec<RenderedAccessory> {
   engine.with(|ctx| evaluate_accessories_in_ctx(&ctx, message_id))
}

/// The plugin API name registering `place`'s decorations.
const fn api_name(place: Place) -> &'static str {
   match place {
      Place::Members => "addMemberDecoration",
      Place::Headers => "addMessageHeaderDecoration",
   }
}

/// Calls every decoration registered for `place` with `user_id`.
fn evaluate_decorations_in_ctx<'js>(
   ctx: &Ctx<'js>,
   place: Place,
   user_id: u64,
) -> Vec<RenderedAccessory> {
   let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
      return Vec::new();
   };
   let snapshot: Vec<DesktopMember<'js>> = registry.decorations(place).borrow().clone();
   drop(registry);
   let Ok(arg) = Object::new(ctx.clone()) else {
      return Vec::new();
   };
   let _ = arg.set("userId", user_id.to_string());

   let mut out = Vec::new();
   for decoration in &snapshot {
      match decoration
         .render
         .call::<_, Value<'_>>((arg.clone(),))
         .catch(ctx)
      {
         Ok(val) => out.extend(parse_rendered(&val, decoration.priority)),
         Err(err) => log::error!("[{}] {} render: {err}", decoration.plugin, api_name(place)),
      }
   }
   out
}

/// Evaluates every decoration registered for `place` with `user_id`.
pub fn render_decorations(engine: &Engine, place: Place, user_id: u64) -> Vec<RenderedAccessory> {
   engine.with(|ctx| evaluate_decorations_in_ctx(&ctx, place, user_id))
}

/// `addMemberDecoration` and `addMessageHeaderDecoration`, which differ only
/// in where they draw.
fn add_decoration<'js>(
   ctx: &Ctx<'js>,
   place: Place,
   plugin: &str,
   options: &Object<'js>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT_ID: AtomicU64 = AtomicU64::new(1);

   let render = options.get::<_, Function<'js>>("render")?;
   let priority = options.get::<_, Option<i32>>("priority")?.unwrap_or(0);
   let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
   let decorations = place.decorations();
   let count = {
      let registry = ctx
         .userdata::<Desktop<'js>>()
         .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
      let mut list = registry.decorations(place).borrow_mut();
      list.push(DesktopMember {
         id,
         plugin: plugin.to_owned(),
         priority,
         render,
      });
      list.len()
   };
   decorations.set_active_count(count);
   decorations.clear_cache();
   accessories::invalidate_discord_windows();

   let remove = Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      if let Some(registry) = ctx.userdata::<Desktop<'js>>() {
         let mut list = registry.decorations(place).borrow_mut();
         list.retain(|entry| entry.id != id);
         decorations.set_active_count(list.len());
         decorations.clear_cache();
         accessories::invalidate_discord_windows();
      }
   })?;
   remove.set(
      "update",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, user_id: Opt<String>| {
         if let Some(uid) = user_id.0.and_then(|s| s.parse::<u64>().ok()) {
            let items = evaluate_decorations_in_ctx(&ctx, place, uid);
            decorations.set_rendered(uid, items);
         } else {
            decorations.clear_cache();
            accessories::invalidate_discord_windows();
         }
      })?,
   )?;
   Ok(remove)
}

#[expect(
   clippy::cast_possible_truncation,
   clippy::cast_sign_loss,
   reason = "ARGB colors are 0..=0xFFFFFFFF, and `as` saturates the rest"
)]
const fn to_argb(color: f64) -> u32 {
   color as u32
}

fn add_message_accessory<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   options: &Object<'js>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT_ID: AtomicU64 = AtomicU64::new(1);

   let render = options.get::<_, Function<'js>>("render")?;
   let priority = options.get::<_, Option<i32>>("priority")?.unwrap_or(0);
   let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

   let count = {
      let registry = ctx
         .userdata::<Desktop<'js>>()
         .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
      let mut list = registry.accessories.borrow_mut();
      list.push(DesktopAccessory {
         id,
         plugin: plugin.to_owned(),
         priority,
         render,
      });
      list.len()
   };
   accessories::set_active_count(count);
   accessories::clear_cache();

   let remove = Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      if let Some(registry) = ctx.userdata::<Desktop<'js>>() {
         let mut list = registry.accessories.borrow_mut();
         list.retain(|entry| entry.id != id);
         accessories::set_active_count(list.len());
         accessories::clear_cache();
         accessories::invalidate_discord_windows();
      }
   })?;
   remove.set(
      "update",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, message_id: Opt<String>| {
         if let Some(mid) = message_id.0.and_then(|s| s.parse::<u64>().ok()) {
            let items = evaluate_accessories_in_ctx(&ctx, mid);
            accessories::set_rendered(mid, items);
         } else {
            accessories::clear_cache();
            accessories::invalidate_discord_windows();
         }
      })?,
   )?;
   Ok(remove)
}

/// The author of `message_id` in Phoenix's message store.
pub fn cached_author(message_id: u64) -> Option<u64> {
   let json = cached_message_and_me(0, message_id).0?;
   let message = serde_json::from_str::<serde_json::Value>(&json).ok()?;
   message.get("author")?.get("id")?.as_str()?.parse().ok()
}

fn cached_message_and_me(channel_id: u64, message_id: u64) -> (Option<String>, Option<String>) {
   let Some(state) = phoenix_runtime::state() else {
      return (None, None);
   };
   let Ok(db) = store::database(&state.files_dir()) else {
      return (None, None);
   };
   // Phoenix keys messages `<channel>:<message>`, each zero-padded to 20
   // digits.
   let rows = if channel_id != 0 {
      store::query(
         &db,
         "SELECT v FROM messages WHERE k = ?",
         &[format!("{channel_id:020}:{message_id:020}")],
      )
   } else {
      store::query(
         &db,
         "SELECT v FROM messages WHERE k LIKE ? LIMIT 1",
         &[format!("%:{message_id:020}")],
      )
   };
   let msg_json = rows
      .ok()
      .and_then(|rows| rows.first("v").and_then(Cell::text).map(str::to_owned));
   let me_id = store::meta(&db, "me")
      .ok()
      .flatten()
      .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
      .and_then(|val| {
         val.get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
      });
   (msg_json, me_id)
}

fn build_message_context<'js>(
   ctx: &Ctx<'js>,
   message_id: u64,
   channel_id: u64,
   include_message: bool,
) -> rquickjs::Result<Object<'js>> {
   let arg = Object::new(ctx.clone())?;
   let id_str = message_id.to_string();
   arg.set("messageId", id_str.as_str())?;
   if channel_id != 0 {
      let ch_str = channel_id.to_string();
      arg.set("channelId", ch_str.as_str())?;
   } else {
      arg.set("channelId", Value::new_null(ctx.clone()))?;
   }
   let (msg_json, me_id) = if include_message {
      let memory = context_menu::menu_message(message_id);
      let (cached, me) = cached_message_and_me(channel_id, message_id);
      (memory.or(cached), me)
   } else {
      (None, None)
   };
   if let Some(json) = msg_json
      && let Ok(parsed) = ctx.json_parse(json)
   {
      arg.set("message", parsed)?;
   } else {
      arg.set("message", Value::new_null(ctx.clone()))?;
   }
   if let Some(me) = me_id {
      arg.set("currentUserId", me.as_str())?;
   } else {
      arg.set("currentUserId", Value::new_null(ctx.clone()))?;
   }
   if let Ok(reply) = Function::new(ctx.clone(), String::new) {
      arg.set("reply", reply)?;
   }
   Ok(arg)
}

/// Evaluates an `addMessageAction`'s synchronous `visible(ctx)` predicate.
pub fn check_action_visible(
   engine: &Engine,
   action_id: u64,
   message_id: u64,
   channel_id: u64,
) -> bool {
   engine.with(|ctx| {
      let Some((plugin, visible)) = ctx.userdata::<Desktop<'_>>().and_then(|registry| {
         let list = registry.actions.borrow();
         let entry = list.iter().find(|e| e.id == action_id)?;
         Some((entry.plugin.clone(), entry.visible.clone()?))
      }) else {
         return true;
      };
      let Ok(arg) = build_message_context(&ctx, message_id, channel_id, true) else {
         return true;
      };
      match visible.call::<_, Value<'_>>((arg,)).catch(&ctx) {
         Ok(val) if val.is_promise() => true,
         Ok(val) => val.as_bool().unwrap_or(true),
         Err(err) => {
            log::error!("[{plugin}] addMessageAction visible: {err}");
            false
         },
      }
   })
}

/// Invokes a registered `addMessageAction`'s `onPress(ctx)` callback.
pub fn invoke_message_action(engine: &Engine, action_id: u64, message_id: u64, channel_id: u64) {
   engine.with(|ctx| {
      let Some((plugin, on_press)) = ctx.userdata::<Desktop<'_>>().and_then(|registry| {
         let list = registry.actions.borrow();
         let entry = list.iter().find(|e| e.id == action_id)?;
         Some((entry.plugin.clone(), entry.on_press.clone()))
      }) else {
         return;
      };
      let Ok(arg) = build_message_context(&ctx, message_id, channel_id, true) else {
         return;
      };
      if let Err(err) = on_press.call::<_, Value<'_>>((arg,)).catch(&ctx) {
         log::error!("[{plugin}] addMessageAction onPress: {err}");
      }
   });
}

static NEXT_ACTION: AtomicU64 = AtomicU64::new(1);

fn register_action<'js>(ctx: &Ctx<'js>, action: DesktopAction<'js>) -> rquickjs::Result<()> {
   let registry = ctx
      .userdata::<Desktop<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   let mut list = registry.actions.borrow_mut();
   list.push(action);
   context_menu::sync_actions(list.iter().map(DesktopAction::meta).collect());
   Ok(())
}

fn remove_action(ctx: &Ctx<'_>, id: u64) {
   if let Some(registry) = ctx.userdata::<Desktop<'_>>() {
      let mut list = registry.actions.borrow_mut();
      list.retain(|entry| entry.id != id);
      context_menu::sync_actions(list.iter().map(DesktopAction::meta).collect());
   }
}

fn add_message_action<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   options: &Object<'js>,
) -> rquickjs::Result<Function<'js>> {
   let label = options.get::<_, String>("label")?;
   let icon = options.get::<_, Option<String>>("icon")?;
   let danger = options.get::<_, Option<bool>>("danger")?.unwrap_or(false);
   let priority = options.get::<_, Option<i32>>("priority")?.unwrap_or(0);
   let visible = options.get::<_, Option<Function<'js>>>("visible")?;
   let on_press = options.get::<_, Function<'js>>("onPress")?;
   let id = NEXT_ACTION.fetch_add(1, Ordering::Relaxed);
   register_action(
      ctx,
      DesktopAction {
         id,
         plugin: plugin.to_owned(),
         label,
         icon,
         danger,
         priority,
         visible,
         on_press,
      },
   )?;
   let remove = Function::new(ctx.clone(), move |ctx: Ctx<'js>| remove_action(&ctx, id))?;
   remove.set("update", Function::new(ctx.clone(), || {})?)?;
   Ok(remove)
}

static NEXT_PAGE: AtomicU64 = AtomicU64::new(1);
static NEXT_BTN: AtomicU64 = AtomicU64::new(1);

/// Runs a page button's callback, with the submitted text for an input.
fn invoke_page_button(button_id: u64, text: Option<String>) {
   let Some(engine) = phoenix_runtime::engine() else {
      return;
   };
   engine.with(|ctx| {
      let Some((plugin, on_press)) = ctx.userdata::<Desktop<'_>>().and_then(|registry| {
         let map = registry.page_buttons.borrow();
         let (_, plugin, func) = map.get(&button_id)?;
         Some((plugin.clone(), func.clone()))
      }) else {
         return;
      };
      if let Err(err) = on_press.call::<_, Value<'_>>((text,)).catch(&ctx) {
         log::error!("[{plugin}] openPage button onPress: {err}");
      }
   });
}

fn open_page<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   options: &Object<'js>,
) -> rquickjs::Result<Object<'js>> {
   let page_id = NEXT_PAGE.fetch_add(1, Ordering::Relaxed);
   let title = options
      .get::<_, Option<String>>("title")?
      .unwrap_or_else(|| "Ruby".to_owned());
   let sections_js = options.get::<_, Option<Array<'js>>>("sections")?;

   let mut sections = Vec::new();
   let mut new_buttons = Vec::new();
   if let Some(arr) = sections_js {
      for entry in arr.iter::<Object<'js>>() {
         let Ok(sec) = entry else {
            continue;
         };
         let kind = sec.get::<_, Option<String>>("type")?.unwrap_or_default();
         let sec_title = sec.get::<_, Option<String>>("title")?;
         match kind.as_str() {
            "buttons" => {
               let mut buttons = Vec::new();
               if let Some(btns) = sec.get::<_, Option<Array<'js>>>("buttons")? {
                  for item in btns.iter::<Object<'js>>() {
                     let Ok(btn) = item else {
                        continue;
                     };
                     let Ok(label) = btn.get::<_, String>("label") else {
                        continue;
                     };
                     let danger = btn.get::<_, Option<bool>>("danger")?.unwrap_or(false);
                     let on_press = btn.get::<_, Option<Function<'js>>>("onPress")?;
                     let btn_id = NEXT_BTN.fetch_add(1, Ordering::Relaxed);
                     if let Some(func) = on_press {
                        new_buttons.push((btn_id, func));
                     }
                     buttons.push(ModalButton {
                        label,
                        danger,
                        on_press: Arc::new(move || invoke_page_button(btn_id, None)),
                     });
                  }
               }
               sections.push(ModalSection::Buttons {
                  title: sec_title,
                  buttons,
               });
            },
            "code" => {
               let text = sec.get::<_, Option<String>>("text")?.unwrap_or_default();
               let monospace = sec.get::<_, Option<bool>>("monospace")?.unwrap_or(true);
               let highlight = sec.get::<_, Option<bool>>("highlight")?.unwrap_or(false);
               sections.push(ModalSection::Code {
                  title: sec_title,
                  text,
                  monospace,
                  highlight,
               });
            },
            "text" => {
               let text = sec.get::<_, Option<String>>("text")?.unwrap_or_default();
               sections.push(ModalSection::Text {
                  title: sec_title,
                  text,
               });
            },
            _ => {},
         }
      }
   }

   if let Some(registry) = ctx.userdata::<Desktop<'js>>() {
      let mut map = registry.page_buttons.borrow_mut();
      for (btn_id, func) in new_buttons {
         map.insert(btn_id, (page_id, plugin.to_owned(), func));
      }
   }

   let modal_id = modal::open_page(PageModalSpec { title, sections });
   let obj = Object::new(ctx.clone())?;
   obj.set(
      "close",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         modal::close_page(modal_id);
         if let Some(registry) = ctx.userdata::<Desktop<'js>>() {
            registry
               .page_buttons
               .borrow_mut()
               .retain(|_, (pid, ..)| *pid != page_id);
         }
      })?,
   )?;
   Ok(obj)
}

/// A `phoenix.ui.addProfileTab`. Desktop has no profile tabs, so it is a
/// message action that shows the tab for the message's author in a page.
struct ProfileTab<'js> {
   plugin: String,
   title: String,
   render: Function<'js>,
   shown: RefCell<Option<ShownTab>>,
}

/// The page a [`ProfileTab`] is on, the key its page buttons are under, and
/// whose profile it shows.
#[derive(Clone)]
struct ShownTab {
   page: u64,
   owner: u64,
   user: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TabShow {
   Open,
   /// Redraws the page on screen, and leaves a closed one closed.
   Refresh,
}

fn add_profile_tab<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   options: &Object<'js>,
) -> rquickjs::Result<Function<'js>> {
   let tab = Rc::new(ProfileTab {
      plugin: plugin.to_owned(),
      title: options.get("title")?,
      render: options.get("render")?,
      shown: RefCell::new(None),
   });
   let priority = options.get::<_, Option<i32>>("priority")?.unwrap_or(0);
   let id = NEXT_ACTION.fetch_add(1, Ordering::Relaxed);

   let opener = Rc::clone(&tab);
   let on_press = Function::new(ctx.clone(), move |ctx: Ctx<'js>, arg: Object<'js>| {
      let author = arg
         .get::<_, Option<Object<'js>>>("message")
         .ok()
         .flatten()
         .and_then(|message| message.get::<_, Object<'js>>("author").ok())
         .and_then(|author| author.get::<_, String>("id").ok());
      match author {
         Some(user) => show_tab(&ctx, &opener, user, TabShow::Open),
         None => phoenix_ui_desktop::toast("This message has no author to show"),
      }
   })?;
   register_action(
      ctx,
      DesktopAction {
         id,
         plugin: plugin.to_owned(),
         label: tab.title.clone(),
         icon: None,
         danger: false,
         priority,
         visible: None,
         on_press,
      },
   )?;

   let closer = Rc::clone(&tab);
   let remove = Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      remove_action(&ctx, id);
      if let Some(shown) = closer.shown.borrow_mut().take() {
         modal::close_page(shown.page);
         drop_page_buttons(&ctx, shown.owner);
      }
   })?;
   remove.set(
      "update",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, user: Opt<String>| {
         let Some(shown) = tab.shown.borrow().clone() else {
            return;
         };
         if user.0.is_none_or(|user| user == shown.user) {
            show_tab(&ctx, &tab, shown.user, TabShow::Refresh);
         }
      })?,
   )?;
   Ok(remove)
}

fn show_tab<'js>(ctx: &Ctx<'js>, tab: &Rc<ProfileTab<'js>>, user: String, how: TabShow) {
   let rendered = Object::new(ctx.clone())
      .and_then(|arg| {
         arg.set("userId", user.as_str())?;
         tab.render.call::<_, Value<'js>>((arg,))
      })
      .catch(ctx);
   let blocks = match rendered {
      Ok(blocks) => blocks,
      Err(err) => {
         log::error!("[{}] addProfileTab render: {err}", tab.plugin);
         return;
      },
   };
   let Some(promise) = blocks.as_promise() else {
      present_tab(ctx, tab, user, how, &blocks);
      return;
   };
   let owner = Rc::clone(tab);
   let plugin = tab.plugin.clone();
   let settled = Function::new(ctx.clone(), move |ctx: Ctx<'js>, blocks: Value<'js>| {
      present_tab(&ctx, &owner, user.clone(), how, &blocks);
   })
   .and_then(|on_resolve| {
      let on_reject = Function::new(ctx.clone(), move |err: Value<'js>| {
         log::error!("[{plugin}] addProfileTab render: {err:?}");
      })?;
      promise
         .then()?
         .call::<_, Value<'js>>((This(promise.clone()), on_resolve, on_reject))
   });
   if let Err(err) = settled.catch(ctx) {
      log::error!("[{}] addProfileTab render: {err}", tab.plugin);
   }
}

fn present_tab<'js>(
   ctx: &Ctx<'js>,
   tab: &Rc<ProfileTab<'js>>,
   user: String,
   how: TabShow,
   blocks: &Value<'js>,
) {
   let owner = NEXT_PAGE.fetch_add(1, Ordering::Relaxed);
   let mut buttons = Vec::new();
   let sections = match profile_sections(ctx, tab, &user, blocks, &mut buttons) {
      Ok(sections) => sections,
      Err(err) => {
         log::error!("[{}] addProfileTab blocks: {}", tab.plugin, err);
         return;
      },
   };
   let spec = PageModalSpec {
      title: tab.title.clone(),
      sections,
   };

   let previous = tab.shown.borrow().clone();
   let page = match (how, &previous) {
      (TabShow::Refresh, Some(shown)) if shown.user == user => {
         if !modal::replace_page(shown.page, spec) {
            drop_page_buttons(ctx, shown.owner);
            *tab.shown.borrow_mut() = None;
            return;
         }
         shown.page
      },
      (TabShow::Refresh, _) => return,
      (TabShow::Open, _) => modal::open_page(spec),
   };
   if let Some(shown) = previous {
      drop_page_buttons(ctx, shown.owner);
   }
   if let Some(registry) = ctx.userdata::<Desktop<'js>>() {
      let mut map = registry.page_buttons.borrow_mut();
      for (btn_id, func) in buttons {
         map.insert(btn_id, (owner, tab.plugin.clone(), func));
      }
   }
   *tab.shown.borrow_mut() = Some(ShownTab { page, owner, user });
}

/// A page button that runs `func`.
fn page_button<'js>(
   label: String,
   danger: bool,
   func: Function<'js>,
   buttons: &mut Vec<(u64, Function<'js>)>,
) -> ModalButton {
   let btn_id = NEXT_BTN.fetch_add(1, Ordering::Relaxed);
   buttons.push((btn_id, func));
   ModalButton {
      label,
      danger,
      on_press: Arc::new(move || invoke_page_button(btn_id, None)),
   }
}

/// A `ProfileAction` list as page buttons. Actions without `onPress` are
/// labels, returned joined.
fn profile_actions<'js>(
   list: Option<&Array<'js>>,
   out: &mut Vec<ModalButton>,
   buttons: &mut Vec<(u64, Function<'js>)>,
) -> rquickjs::Result<Vec<String>> {
   let mut labels = Vec::new();
   for action in list.into_iter().flat_map(Array::iter::<Object<'js>>) {
      let action = action?;
      let label = action.get::<_, String>("label")?;
      match action.get::<_, Option<Function<'js>>>("onPress")? {
         Some(func) => {
            let danger = action.get::<_, Option<bool>>("danger")?.unwrap_or(false);
            out.push(page_button(label, danger, func, buttons));
         },
         None => labels.push(label),
      }
   }
   Ok(labels)
}

/// Lays out a profile tab's blocks as page sections. An entry is a text
/// section and a row of its actions, and an input is a button that asks for
/// the text, then opens the tab again.
#[expect(clippy::too_many_lines, reason = "one arm per block type")]
fn profile_sections<'js>(
   ctx: &Ctx<'js>,
   tab: &Rc<ProfileTab<'js>>,
   user: &str,
   blocks: &Value<'js>,
   buttons: &mut Vec<(u64, Function<'js>)>,
) -> rquickjs::Result<Vec<ModalSection>> {
   let Some(blocks) = blocks.as_array() else {
      return Err(Exception::throw_type(
         ctx,
         "render must return an array of blocks",
      ));
   };
   let mut sections = Vec::new();
   for block in blocks.iter::<Object<'js>>() {
      let block = block?;
      let kind = block.get::<_, String>("type")?;
      match kind.as_str() {
         "text" => {
            sections.push(ModalSection::Text {
               title: block.get("title")?,
               text: block.get("text")?,
            });
         },
         "entry" => {
            let mut row = Vec::new();
            let side = profile_actions(
               block.get::<_, Option<Array<'js>>>("side")?.as_ref(),
               &mut row,
               buttons,
            )?;
            let actions = profile_actions(
               block.get::<_, Option<Array<'js>>>("actions")?.as_ref(),
               &mut row,
               buttons,
            )?;
            if let Some(func) = block.get::<_, Option<Function<'js>>>("onLongPress")? {
               row.push(page_button("More".to_owned(), false, func, buttons));
            }
            let mut title = block.get::<_, String>("title")?;
            for part in block
               .get::<_, Option<String>>("subtitle")?
               .into_iter()
               .chain(side)
               .chain(actions)
            {
               title.push_str(" · ");
               title.push_str(&part);
            }
            let mut text = block.get::<_, String>("text")?;
            for image in block
               .get::<_, Option<Vec<String>>>("images")?
               .into_iter()
               .flatten()
            {
               if !text.is_empty() {
                  text.push('\n');
               }
               text.push_str(&image);
            }
            sections.push(ModalSection::Text {
               title: Some(title),
               text,
            });
            if !row.is_empty() {
               sections.push(ModalSection::Buttons {
                  title: None,
                  buttons: row,
               });
            }
         },
         "input" => {
            let on_submit = block.get::<_, Function<'js>>("onSubmit")?;
            let label = block
               .get::<_, Option<String>>("placeholder")?
               .unwrap_or_else(|| "Write".to_owned());
            let reopen = Rc::clone(tab);
            let user = user.to_owned();
            let submit = Function::new(ctx.clone(), move |ctx: Ctx<'js>, text: String| {
               if let Err(err) = on_submit.call::<_, Value<'js>>((text,)).catch(&ctx) {
                  log::error!("[{}] addProfileTab onSubmit: {err}", reopen.plugin);
               }
               show_tab(&ctx, &reopen, user.clone(), TabShow::Open);
            })?;
            let btn_id = NEXT_BTN.fetch_add(1, Ordering::Relaxed);
            buttons.push((btn_id, submit));
            let title = label.clone();
            sections.push(ModalSection::Buttons {
               title: None,
               buttons: vec![ModalButton {
                  label,
                  danger: false,
                  on_press: Arc::new(move || {
                     modal::open_prompt(Prompt {
                        title: title.clone(),
                        on_submit: Arc::new(move |text| invoke_page_button(btn_id, Some(text))),
                     });
                  }),
               }],
            });
         },
         "button" => {
            let label = block.get::<_, String>("label")?;
            let danger = block.get::<_, Option<bool>>("danger")?.unwrap_or(false);
            let func = block.get::<_, Function<'js>>("onPress")?;
            sections.push(ModalSection::Buttons {
               title: None,
               buttons: vec![page_button(label, danger, func, buttons)],
            });
         },
         "buttons" => {
            let mut row = Vec::new();
            profile_actions(
               block.get::<_, Option<Array<'js>>>("buttons")?.as_ref(),
               &mut row,
               buttons,
            )?;
            sections.push(ModalSection::Buttons {
               title: None,
               buttons: row,
            });
         },
         _ => {},
      }
   }
   Ok(sections)
}

/// `phoenix.ui.hide`. Desktop knows one id, the composer's gift button.
fn hide<'js>(ctx: &Ctx<'js>, plugin: &str, id: &str) -> rquickjs::Result<Function<'js>> {
   static NEXT_HIDE: AtomicU64 = AtomicU64::new(1);

   let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
      return noop_handle(ctx);
   };
   if id != "composer.gift" || !composer::hide_gift(true) {
      return noop_handle(ctx);
   }
   let hold = NEXT_HIDE.fetch_add(1, Ordering::Relaxed);
   registry.hides.borrow_mut().push((hold, plugin.to_owned()));
   Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
         return;
      };
      let mut hides = registry.hides.borrow_mut();
      let before = hides.len();
      hides.retain(|(held, _)| *held != hold);
      if hides.len() != before {
         composer::hide_gift(false);
      }
   })
}

/// `phoenix.ui.onEvent`. Desktop sends `rowDoubleTap`, on message rows.
fn on_event<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   event: &str,
   handler: Function<'js>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT_TAP: AtomicU64 = AtomicU64::new(1);

   let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
      return noop_handle(ctx);
   };
   if event != "rowDoubleTap" {
      return noop_handle(ctx);
   }
   let id = NEXT_TAP.fetch_add(1, Ordering::Relaxed);
   let mut taps = registry.double_taps.borrow_mut();
   taps.push((id, plugin.to_owned(), handler));
   double_click::set_active(true);
   drop(taps);
   Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      if let Some(registry) = ctx.userdata::<Desktop<'js>>() {
         let mut taps = registry.double_taps.borrow_mut();
         taps.retain(|(held, ..)| *held != id);
         double_click::set_active(!taps.is_empty());
      }
   })
}

/// Runs the `rowDoubleTap` handlers for a double click on `message_id`, and
/// returns the message menu item one pressed with `ev.press`.
pub fn double_tap(engine: &Engine, message_id: u64) -> Option<String> {
   engine.with(|ctx| {
      let handlers = ctx
         .userdata::<Desktop<'_>>()?
         .double_taps
         .borrow()
         .iter()
         .map(|(_, plugin, handler)| (plugin.clone(), handler.clone()))
         .collect::<Vec<_>>();
      let pressed = Rc::new(RefCell::new(None::<String>));
      for (plugin, handler) in handlers {
         let ev = build_message_context(&ctx, message_id, 0, true).ok()?;
         let into = Rc::clone(&pressed);
         let press = Function::new(ctx.clone(), move |action: String| {
            let item = action.strip_prefix("message.actions.").unwrap_or(&action);
            *into.borrow_mut() = Some(item.to_owned());
         })
         .ok()?;
         ev.set("press", press).ok()?;
         if let Err(err) = handler.call::<_, Value<'_>>((ev,)).catch(&ctx) {
            log::error!("[{plugin}] onEvent rowDoubleTap: {err}");
         }
         if pressed.borrow().is_some() {
            break;
         }
      }
      pressed.take()
   })
}

fn drop_page_buttons(ctx: &Ctx<'_>, owner: u64) {
   if let Some(registry) = ctx.userdata::<Desktop<'_>>() {
      registry
         .page_buttons
         .borrow_mut()
         .retain(|_, (key, ..)| *key != owner);
   }
}

/// Wires desktop `phoenix.ui`, `phoenix.messages.onSend` / `onEmojiPick`, and
/// `phoenix.commands` onto the plugin's API objects.
pub fn desktop_patch_api<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   api: &Object<'js>,
   messages: &Object<'js>,
) -> rquickjs::Result<()> {
   let name = plugin.to_owned();
   messages.set(
      "onSend",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, function: Function<'js>| on_send(&ctx, &name, function),
      )?,
   )?;
   let name = plugin.to_owned();
   messages.set(
      "onEmojiPick",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, _function: Opt<Value<'js>>| on_emoji_pick(&ctx, &name),
      )?,
   )?;
   let commands = Object::new(ctx.clone())?;
   commands.set(
      "register",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, _options: Opt<Value<'js>>| {
         noop_handle(&ctx)
      })?,
   )?;
   commands.set(
      "list",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| rquickjs::Array::new(ctx))?,
   )?;
   api.set("commands", commands)?;
   api.set("ui", desktop_ui_object(ctx, plugin)?)
}

#[expect(
   clippy::too_many_lines,
   reason = "one binding per `phoenix.ui` function"
)]
pub fn desktop_ui_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let ui = Object::new(ctx.clone())?;
   ui.set(
      "toast",
      Function::new(ctx.clone(), |message: String| {
         phoenix_ui_desktop::toast(&message);
      })?,
   )?;
   ui.set(
      "copy",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| {
         phoenix_ui_desktop::copy(&text)
            .map_err(|err| Exception::throw_message(&ctx, &err.to_string()))
      })?,
   )?;
   ui.set(
      "notify",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, options: Object<'js>| {
         let title = options
            .get::<_, Option<String>>("title")?
            .unwrap_or_default();
         let body = options.get::<_, Option<String>>("body")?;
         phoenix_ui_desktop::notify(&title, body.as_deref());
         noop_handle(&ctx)
      })?,
   )?;

   let count_owner = plugin.to_owned();
   ui.set(
      "hideCount",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, name: String| {
         hide_count(&ctx, &count_owner, &name)
      })?,
   )?;

   let owner = plugin.to_owned();
   ui.set(
      "addMessageAccessory",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         add_message_accessory(&ctx, &owner, &options)
      })?,
   )?;

   for place in [Place::Members, Place::Headers] {
      let owner = plugin.to_owned();
      ui.set(
         api_name(place),
         Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
            add_decoration(&ctx, place, &owner, &options)
         })?,
      )?;
   }

   let action_owner = plugin.to_owned();
   ui.set(
      "addMessageAction",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         add_message_action(&ctx, &action_owner, &options)
      })?,
   )?;

   let page_owner = plugin.to_owned();
   ui.set(
      "openPage",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         open_page(&ctx, &page_owner, &options)
      })?,
   )?;

   let tab_owner = plugin.to_owned();
   ui.set(
      "addProfileTab",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         add_profile_tab(&ctx, &tab_owner, &options)
      })?,
   )?;

   let hide_owner = plugin.to_owned();
   ui.set(
      "hide",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: String| {
         hide(&ctx, &hide_owner, &id)
      })?,
   )?;
   let event_owner = plugin.to_owned();
   ui.set(
      "onEvent",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, event: String, handler: Function<'js>| {
            on_event(&ctx, &event_owner, &event, handler)
         },
      )?,
   )?;

   // Graceful no-op shims for remaining mobile UI-op surfaces so cross-platform
   // plugins load without throwing on desktop.
   for slot_fn in [
      "addHeaderButton",
      "addComposerButton",
      "addProfileSection",
      "patchProps",
      "onNode",
      "mount",
      "dispatch",
      "update",
      "disableHaptics",
   ] {
      ui.set(
         slot_fn,
         Function::new(ctx.clone(), |ctx: Ctx<'js>| noop_handle(&ctx))?,
      )?;
   }
   for query_fn in ["find", "findDown"] {
      ui.set(
         query_fn,
         Function::new(ctx.clone(), |ctx: Ctx<'js>| rquickjs::Array::new(ctx))?,
      )?;
   }
   for nullable_fn in ["node", "root"] {
      ui.set(
         nullable_fn,
         Function::new(ctx.clone(), |ctx: Ctx<'js>| Value::new_null(ctx))?,
      )?;
   }
   ui.set(
      "choose",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, options: Object<'js>| {
         choose_dialog(&ctx, &options)
      })?,
   )?;
   ui.set(
      "showSheet",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, options: Object<'js>| {
         sheet_dialog(&ctx, &options)
      })?,
   )?;
   Ok(ui)
}

/// `phoenix.ui.choose` as a native Mana modal; the picked index, or -1 when it
/// was dismissed.
fn choose_dialog<'js>(
   ctx: &Ctx<'js>,
   options: &Object<'js>,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
   let title = options.get::<_, Option<String>>("title")?;
   let message = options.get::<_, Option<String>>("message")?;
   let items = parse_choice_options(options)?;
   if message.is_none() && items.is_empty() {
      return Err(Exception::throw_type(
         ctx,
         "choose needs options or a message",
      ));
   }
   let body = message.map(|text| ModalSection::Text { title: None, text });
   let title = title.unwrap_or_else(|| "Ruby".to_owned());
   in_background(ctx, move || answer(title, body, items))
}

/// `phoenix.ui.showSheet` as a native Mana modal with the code viewer.
fn sheet_dialog<'js>(
   ctx: &Ctx<'js>,
   options: &Object<'js>,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
   let title = options.get::<_, Option<String>>("title")?;
   let text = options.get::<_, String>("text")?;
   let monospace = options
      .get::<_, Option<bool>>("monospace")?
      .unwrap_or_default();
   let highlight = options
      .get::<_, Option<bool>>("highlight")?
      .unwrap_or_default();
   let items = parse_choice_options(options)?;
   let body = ModalSection::Code {
      title: None,
      text,
      monospace,
      highlight,
   };
   let title = title.unwrap_or_else(|| "Ruby".to_owned());
   in_background(ctx, move || answer(title, Some(body), items))
}

/// Opens a choose dialog and waits for the picked index, -1 when it is
/// dismissed or left open for ten minutes.
fn answer(
   title: String,
   body: Option<ModalSection>,
   items: Vec<modal::ChoiceOption>,
) -> Settlement {
   Settlement::Int(Ok(modal::ask(title, body, items, Duration::from_secs(600))))
}

fn parse_choice_options<'js>(options: &Object<'js>) -> rquickjs::Result<Vec<modal::ChoiceOption>> {
   Ok(options
      .get::<_, Option<Vec<Value<'js>>>>("options")?
      .unwrap_or_default()
      .into_iter()
      .filter_map(|option| {
         if let Some(label) = option.as_string() {
            return Some(modal::ChoiceOption {
               label: label.to_string().ok()?,
               danger: false,
            });
         }
         let option = option.as_object()?;
         Some(modal::ChoiceOption {
            label: option.get::<_, Option<String>>("label").ok().flatten()?,
            danger: option
               .get::<_, Option<bool>>("danger")
               .ok()
               .flatten()
               .unwrap_or_default(),
         })
      })
      .collect())
}

/// Runs registered `phoenix.messages.onSend` hooks against the JSON payload
/// of an outgoing `POST /channels/<channel_id>/messages` request on desktop.
pub fn intercept_send_body(engine: &Engine, channel_id: &str, body: &[u8]) -> RequestAction {
   engine.with(|ctx| {
      let Some(registry) = ctx.userdata::<Registry<'_>>() else {
         return RequestAction::Pass;
      };
      let interceptors = registry
         .interceptors
         .borrow()
         .iter()
         .map(|(_, plugin, function)| (plugin.clone(), function.clone()))
         .collect::<Vec<_>>();
      drop(registry);
      if interceptors.is_empty() {
         return RequestAction::Pass;
      }
      let Ok(mut payload) = serde_json::from_slice::<serde_json::Value>(body) else {
         return RequestAction::Pass;
      };
      let Some(original) = payload
         .get("content")
         .and_then(serde_json::Value::as_str)
         .map(str::to_owned)
      else {
         return RequestAction::Pass;
      };
      let Ok(argument) = Object::new(ctx.clone()) else {
         return RequestAction::Pass;
      };
      let _ = argument.set("channelId", channel_id);

      let mut value = original.clone();
      for (plugin, function) in interceptors {
         match function
            .call::<_, Value<'_>>((value.clone(), argument.clone()))
            .catch(&ctx)
         {
            Ok(result) if result.as_bool() == Some(false) => {
               return RequestAction::Drop;
            },
            Ok(result) => {
               if let Some(text) = result.as_string().and_then(|text| text.to_string().ok()) {
                  value = text;
               }
            },
            Err(err) => log::error!("[{plugin}] onSend: {err}"),
         }
      }
      if value == original {
         return RequestAction::Pass;
      }
      if let Some(obj) = payload.as_object_mut() {
         obj.insert("content".to_owned(), serde_json::Value::String(value));
      }
      serde_json::to_vec(&payload).map_or(RequestAction::Pass, RequestAction::Replace)
   })
}

/// Counts the handler toward the emoji unlock. Desktop has no picker presses
/// to give it: the unlocked picker inserts the emoji itself.
fn on_emoji_pick<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Function<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   let registry = ctx
      .userdata::<Desktop<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   registry
      .emoji_pickers
      .borrow_mut()
      .push((id, plugin.to_owned()));
   emoji::unlock(true);
   drop(registry);
   Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      if let Some(registry) = ctx.userdata::<Desktop<'js>>() {
         let mut pickers = registry.emoji_pickers.borrow_mut();
         pickers.retain(|(existing, _)| *existing != id);
         emoji::unlock(!pickers.is_empty());
      }
   })
}

/// `phoenix.ui.hideCount`, until the returned remover runs.
#[cfg(target_arch = "x86_64")]
fn hide_count<'js>(ctx: &Ctx<'js>, plugin: &str, name: &str) -> rquickjs::Result<Function<'js>> {
   static NEXT_HIDE: AtomicU64 = AtomicU64::new(1);

   let count = name.parse::<Count>().map_err(|()| {
      Exception::throw_type(
         ctx,
         &format!("unknown count {name:?}, expected \"friendRequests\" or \"messageRequests\""),
      )
   })?;
   let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
      return noop_handle(ctx);
   };
   let hold = NEXT_HIDE.fetch_add(1, Ordering::Relaxed);
   let mut hidden = registry.hidden_counts.borrow_mut();
   hidden.push((hold, plugin.to_owned(), count));
   counts::hide(hidden.iter().map(|&(.., count)| count));
   Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
         return;
      };
      let mut hidden = registry.hidden_counts.borrow_mut();
      hidden.retain(|&(held, ..)| held != hold);
      counts::hide(hidden.iter().map(|&(.., count)| count));
   })
}

/// macOS has no pending count locators yet, so the counts stay.
#[cfg(not(target_arch = "x86_64"))]
fn hide_count<'js>(ctx: &Ctx<'js>, _plugin: &str, _name: &str) -> rquickjs::Result<Function<'js>> {
   noop_handle(ctx)
}

pub fn on_send<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   function: Function<'js>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   let registry = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   registry
      .interceptors
      .borrow_mut()
      .push((id, plugin.to_owned(), function));
   drop(registry);
   Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      if let Some(registry) = ctx.userdata::<Registry<'js>>() {
         registry
            .interceptors
            .borrow_mut()
            .retain(|(existing, ..)| *existing != id);
      }
   })
}
