//! The worker thread that runs plugin callbacks, and the jobs it takes.

use std::sync::{
   OnceLock,
   mpsc::Sender,
};

use phoenix_runtime::{
   js::Engine,
   store,
};
use phoenix_ui_mobile::{
   Op,
   Props,
};
use phoenix_wire::{
   Key,
   Value,
};
use serde::Deserialize;

use crate::{
   bridge,
   js::{
      mount::mount_callback,
      page::page_press,
      ui::{
         ui_evaluate,
         ui_press,
      },
      view,
   },
   patches::{
      ContributionId,
      Outcome,
      Patches,
      Place,
      RESULTS_CAP,
      UiContext,
      aids::message_id,
      banners,
      banners::BannerSpec,
      commands,
      emoji,
      menu::{
         MenuItem,
         SheetBody,
      },
      mount,
      mount::{
         MountBefore,
         MountNode,
      },
      nodes::NodeInfo,
      pages,
      pages::PageSection,
      patches,
      profile,
      targets::Target,
   },
};

pub enum Job {
   Refresh(Target),
   Evaluate(ContributionId, Place),
   Evaluated(ContributionId, Place, Outcome),
   Press(ContributionId, Place),
   Suggest,
   Pick {
      input: i32,
      name:  String,
   },
   Submit {
      input: i32,
      text:  String,
      frame: Vec<u8>,
   },
   /// Open a plugin's profile tab on `pager`.
   Select {
      pager:        i32,
      contribution: ContributionId,
   },
   /// Open Phoenix's tab `index` on `pager`.
   Leave {
      pager: i32,
      index: i32,
   },
   Block {
      contribution: ContributionId,
      place:        Place,
      index:        u32,
      text:         Option<String>,
      /// The text box to empty.
      input:        Option<i32>,
   },
   PageHeight {
      pager:        i32,
      contribution: ContributionId,
      height:       f64,
   },
   /// Submit the text box `index` on `contribution`'s page.
   Send {
      contribution: ContributionId,
      place:        Place,
      index:        u32,
   },
   /// Show a menu sheet. The chosen row's index, or -1, goes to `answer`,
   /// which is dropped unanswered when no sheet can be drawn.
   Menu {
      title:  Option<String>,
      body:   Option<SheetBody>,
      items:  Vec<MenuItem>,
      answer: Sender<i32>,
   },
   /// Close menu `menu`, answering `index`.
   CloseMenu {
      menu:  u32,
      index: i32,
   },
   /// Push a full-screen page onto the active navigator.
   OpenPage {
      id:       u32,
      title:    String,
      sections: Vec<PageSection>,
   },
   /// Pop plugin full-screen page `id`.
   ClosePage(u32),
   /// Run button `index` on plugin full-screen page `page`.
   PagePress {
      page:  u32,
      index: u32,
   },
   /// Reconcile `hidden: true` across live nodes matching `self.hidden`.
   RefreshHidden,
   /// A press on custom emoji `id` in the picker, held for the `onEmojiPick`
   /// handlers.
   Emoji {
      input: i32,
      id:    String,
      text:  String,
      frame: Vec<u8>,
   },
   /// Re-apply declarative `PatchTarget::List` prop patches to live lists.
   RefreshPropPatches,
   /// Send a raw `Op::Update` requested by `phoenix.ui.update`.
   UpdateNode {
      node:  i32,
      props: Props,
   },
   /// A view event matching one or more `phoenix.ui.onEvent` handlers.
   ViewEvent {
      node:       i32,
      event:      Key,
      payload:    Value,
      aid:        Option<String>,
      message_id: Option<String>,
      frame:      Option<Vec<u8>>,
   },
   /// A node matching one or more `phoenix.ui.onNode` watchers was created.
   NodeCreated {
      watchers: Vec<u64>,
      info:     NodeInfo,
   },
   /// Show a native top in-app notification banner.
   ShowBanner(BannerSpec),
   /// Show any banners queued while waiting for the root UI to settle.
   FlushBanners,
   /// Dismiss in-app notification banner `id` if still visible.
   DismissBanner(u32),
   /// User tapped in-app notification banner `id`.
   BannerPressed(u32),
   /// Mount a raw JS-built subtree onto `parent` (or `mirror.root()` when
   /// None).
   Mount {
      id:     u32,
      parent: Option<i32>,
      before: MountBefore,
      node:   MountNode,
   },
   /// Unmount a raw JS-built subtree.
   Unmount(u32),
   /// Run callback `callback` for raw JS mount `mount`.
   MountCallback {
      mount:    u32,
      callback: u32,
   },
}

pub static WORKER: OnceLock<Sender<Job>> = OnceLock::new();

pub fn queue(job: Job) {
   if let Some(worker) = WORKER.get()
      && worker.send(job).is_err()
   {
      log::warn!("patches: the worker is gone");
   }
}

/// Runs `job`'s plugin calls, then returns the ops it leaves for the screen.
/// The engine and the patch state are never held together.
pub fn run(job: Job) -> Vec<Op> {
   match job {
      Job::Refresh(target) => patches().redraw(target),
      Job::Select {
         pager,
         contribution,
      } => patches().select_tab(pager, contribution),
      Job::Leave { pager, index } => patches().leave_tab(pager, Some(index)),
      Job::PageHeight {
         pager,
         contribution,
         height,
      } => patches().page_height(pager, contribution, height),
      Job::Send {
         contribution,
         place,
         index,
      } => profile::send(contribution, &place, index),
      Job::Menu {
         title,
         body,
         items,
         answer,
      } => patches().open_menu(title.as_deref(), body.as_ref(), items, answer),
      Job::CloseMenu { menu, index } => patches().close_menu(menu, index),
      Job::OpenPage {
         id,
         title,
         sections,
      } => patches().open_page(id, &title, sections),
      Job::ClosePage(id) => pages::close(id),
      Job::PagePress { page, index } => callback(|engine| page_press(engine, page, index)),
      Job::RefreshHidden => patches().reconcile_hidden(),
      Job::Block {
         contribution,
         place,
         index,
         text,
         input,
      } => profile::block(contribution, &place, index, text, input),
      Job::Evaluate(contribution, place) => evaluate(contribution, place),
      Job::Evaluated(contribution, place, outcome) => {
         patches().evaluated(contribution, place, outcome)
      },
      Job::Press(contribution, place) => {
         let context = press_context(&place);
         callback(|engine| {
            ui_press(engine, contribution, &context);
         })
      },
      Job::Suggest => patches().suggested(),
      Job::Pick { input, name } => patches().pick(input, &name),
      Job::Submit { input, text, frame } => commands::submit(input, &text, &frame),
      Job::Emoji {
         input,
         id,
         text,
         frame,
      } => emoji::pick(input, &id, text, &frame),
      Job::RefreshPropPatches => patches().refresh_list_patches(),
      Job::UpdateNode { node, props } => vec![Op::Update { id: node, props }],
      Job::ViewEvent {
         node,
         event,
         payload,
         aid,
         message_id,
         frame,
      } => {
         view_event(
            node,
            event,
            &payload,
            aid.as_deref(),
            message_id.as_deref(),
            frame,
         )
      },
      Job::NodeCreated { watchers, info } => {
         callback(|engine| {
            view::node_created(engine, &watchers, &info);
         })
      },
      Job::ShowBanner(spec) => patches().show_banner(spec),
      Job::FlushBanners => patches().flush_banners(),
      Job::DismissBanner(id) => banners::dismiss(id),
      Job::BannerPressed(id) => banners::pressed(id),
      Job::Mount {
         id,
         parent,
         before,
         node,
      } => patches().open_mount(id, parent, before, node),
      Job::Unmount(id) => mount::close(id),
      Job::MountCallback {
         mount,
         callback: id,
      } => callback(|engine| mount_callback(engine, mount, id)),
   }
}

fn callback(call: impl FnOnce(&Engine)) -> Vec<Op> {
   if let Some(engine) = phoenix_runtime::engine() {
      call(&engine);
   }
   Vec::new()
}

fn evaluate(contribution: ContributionId, place: Place) -> Vec<Op> {
   let target = patches()
      .contributions
      .iter()
      .find(|known| known.id == contribution)
      .map(|known| known.target);
   if target.is_some_and(Target::decorates) {
      return decorate(contribution, place);
   }
   // A message action's `visible` decides on the message, so it gets the
   // same context as the press.
   let context = if target == Some(Target::Action) {
      press_context(&place)
   } else {
      UiContext {
         channel_id:      patches().channel.clone(),
         message_id:      match &place {
            Place::Message(id) => Some(id.clone()),
            _ => None,
         },
         user_id:         match &place {
            Place::User(id) => id.clone(),
            _ => None,
         },
         current_user_id: None,
         message:         None,
      }
   };
   let outcome = phoenix_runtime::engine()
      .and_then(|engine| ui_evaluate(&engine, contribution, &place, &context));
   outcome.map_or_else(Vec::new, |outcome| {
      patches().evaluated(contribution, place, outcome)
   })
}

/// Evaluates member or header decoration `contribution` for the user `place`
/// is about, or draws nothing when there is none, like in a group DM.
fn decorate(contribution: ContributionId, place: Place) -> Vec<Op> {
   let known = patches().users.get(&place).cloned();
   let user = known.unwrap_or_else(|| {
      let user = match &place {
         Place::User(user) => user.clone(),
         Place::Message(message) => message_author(message),
         Place::Channel(channel) => dm_recipient(channel),
         Place::Screen => None,
      };
      let mut state = patches();
      if state.users.len() >= RESULTS_CAP {
         state.users.clear();
      }
      state.users.insert(place.clone(), user.clone());
      user
   });
   let Some(user) = user else {
      return patches().evaluated(contribution, place, Outcome::Text(None));
   };
   let context = UiContext {
      user_id: Some(user),
      ..UiContext::default()
   };
   let outcome = phoenix_runtime::engine()
      .and_then(|engine| ui_evaluate(&engine, contribution, &place, &context));
   outcome.map_or_else(Vec::new, |outcome| {
      patches().evaluated(contribution, place, outcome)
   })
}

/// A row of Phoenix's cache by its key, as JSON.
fn cached<T: for<'de> Deserialize<'de>>(table: &str, key: String) -> Option<T> {
   let database = store::database(&phoenix_runtime::state()?.files_dir()).ok()?;
   // `LIKE` for a message, whose key starts with a channel we do not know.
   let sql = format!("SELECT CAST(v AS TEXT) AS v FROM {table} WHERE k LIKE ? LIMIT 1");
   let rows = store::query(&database, &sql, &[key])
      .inspect_err(|err| log::warn!("patches: {table}: {err}"))
      .ok()?;
   serde_json::from_str(rows.first("v")?.text()?).ok()
}

/// The author of `message` in Phoenix's cache.
fn message_author(message: &str) -> Option<String> {
   #[derive(Deserialize)]
   struct Author {
      id: Snowflake,
   }
   #[derive(Deserialize)]
   struct Message {
      author: Author,
   }
   cached::<Message>("messages", format!("%:{message:0>20}"))
      .map(|message| message.author.id.into())
}

/// The one recipient of DM `channel` in Phoenix's cache, kept under the empty
/// guild.
fn dm_recipient(channel: &str) -> Option<String> {
   #[derive(Deserialize)]
   struct Channel {
      r#type:        u8,
      recipient_ids: Vec<Snowflake>,
   }
   let channel = cached::<Channel>("channels", format!("{:0>20}:{channel:0>20}", 0))?;
   let [user] = <[Snowflake; 1]>::try_from(channel.recipient_ids).ok()?;
   (channel.r#type == 1).then(|| user.into())
}

fn view_event(
   node: i32,
   event: Key,
   payload: &Value,
   aid: Option<&str>,
   message_id: Option<&str>,
   frame: Option<Vec<u8>>,
) -> Vec<Op> {
   let context = message_id.map(|id| press_context(&Place::Message(id.to_owned())));
   let consumed = phoenix_runtime::engine().is_some_and(|engine| {
      view::view_event(
         &engine,
         node,
         event,
         payload,
         aid,
         context.as_ref(),
         frame.is_some(),
      )
   });
   if let Some(frame) = frame
      && !consumed
   {
      patches().resent.push(frame.clone());
      if !bridge::inject(&frame) {
         patches().resent.retain(|sent| *sent != frame);
         log::warn!("patches: view event re-inject failed");
      }
   }
   Vec::new()
}

/// A snowflake, which Phoenix's cache keeps as a string or a number.
#[derive(Deserialize)]
#[serde(untagged)]
enum Snowflake {
   Text(String),
   Number(u64),
}

impl From<Snowflake> for String {
   fn from(id: Snowflake) -> Self {
      match id {
         Snowflake::Text(text) => text,
         Snowflake::Number(number) => number.to_string(),
      }
   }
}

/// The `me` row of Phoenix's `meta` table.
#[derive(Deserialize)]
struct Me {
   id: Snowflake,
}

/// The `selected` row of Phoenix's `meta` table.
#[derive(Deserialize)]
struct Selected {
   channel_id: Snowflake,
}

/// `ctx` for a press: the open channel, and the message when there is one.
/// It reads Phoenix's cache, so it runs on the worker.
pub fn press_context(place: &Place) -> UiContext {
   let database =
      phoenix_runtime::state().and_then(|state| store::database(&state.files_dir()).ok());
   let rows = |sql: &str, args: &[String]| {
      let database = database.as_ref()?;
      store::query(database, sql, args)
         .inspect_err(|err| log::warn!("patches: {sql}: {err}"))
         .ok()
   };
   let message = |id: &str| {
      let found = rows("SELECT k, v FROM messages WHERE k LIKE ? LIMIT 1", &[
         format!("%:{id:0>20}"),
      ]);
      let text = |column| found.as_ref()?.first(column)?.text();
      let channel =
         text("k").and_then(|key| Some(key.split(':').next()?.trim_start_matches('0').to_owned()));
      (channel, text("v").map(str::to_owned))
   };

   let mut context = UiContext::default();
   if let Place::Message(id) = place {
      (context.channel_id, context.message) = message(id);
      context.message_id = Some(id.clone());
   }
   if let Place::User(id) = place {
      context.user_id.clone_from(id);
   }
   let meta = |key: &str| {
      let database = database.as_ref()?;
      store::meta(database, key)
         .inspect_err(|err| log::warn!("patches: meta {key}: {err}"))
         .ok()
         .flatten()
   };
   context.current_user_id = meta("me")
      .and_then(|me| serde_json::from_str::<Me>(&me).ok())
      .map(|me| me.id.into());
   if context.channel_id.is_none() {
      context.channel_id = meta("selected")
         .and_then(|selected| serde_json::from_str::<Selected>(&selected).ok())
         .map(|selected| selected.channel_id.into());
   }
   if context.channel_id.is_none() {
      let newest = patches().newest_message();
      context.channel_id = newest.and_then(|id| message(&id).0);
   }
   if context.channel_id.is_some() {
      patches().channel.clone_from(&context.channel_id);
   }
   context
}

impl Patches {
   /// The newest message on screen.
   fn newest_message(&self) -> Option<String> {
      self
         .anchors
         .get(&Target::Content)?
         .iter()
         .filter_map(|id| message_id(self.mirror.accessibility_id(*id)?))
         .max_by_key(|id| (id.len(), *id))
         .map(str::to_owned)
   }
}
