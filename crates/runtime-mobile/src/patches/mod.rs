//! Plugin UI spliced into Phoenix's screens: buttons in the chat header and
//! the message box, lines under messages, rows in the message long-press
//! menu, profile sections and tabs, and slash commands in the `/` suggestions.
//!
//! - Rust's batches are mirrored (children, accessibility ids), and plugin
//!   nodes go next to stable accessibility ids, cloned from Phoenix's own
//!   neighbours.
//! - No plugin code runs on the threads that carry UI batches or view events.
//!   Rust's store thread can hold the engine while it emits a batch, and the
//!   main thread can be waited on with the UI lock held. Plugin callbacks run
//!   on a worker, and their results reach the screen as follow-up batches.

mod aids;
pub mod banners;
pub mod commands;
mod emoji;
pub mod events;
pub mod hidden;
pub mod menu;
pub mod mount;
pub mod nodes;
pub mod pages;
mod profile;
pub mod props;
mod sink;
mod slots;
mod tabs;
pub mod targets;
mod tints;
mod worker;

use std::{
   collections::{
      HashMap,
      HashSet,
      VecDeque,
   },
   fmt,
   sync::{
      LazyLock,
      Mutex,
      MutexGuard,
      PoisonError,
      atomic::{
         AtomicBool,
         AtomicI32,
         AtomicU64,
         Ordering,
      },
      mpsc::{
         self,
         Sender,
      },
   },
   thread,
   time::Instant,
};

use phoenix_ui_mobile::{
   Node,
   Op,
   Prop,
   Props,
   ViewKind,
   encode,
   inspect::TreeJson,
   mirror::Mirror,
   set_prop,
};
use phoenix_wire::{
   Key,
   Value,
};

use crate::{
   bridge,
   patches::{
      banners::{
         BannerAction,
         BannerSpec,
      },
      commands::Command,
      nodes::NodeMatch,
      props::PatchTarget,
      sink::PatchSink,
      targets::{
         Seen,
         Target,
         Templates,
      },
      worker::{
         Job,
         WORKER,
         queue,
         run,
      },
   },
};

/// Plugin node ids count up from here, clear of Rust's and the settings page's.
const FIRST_ID: i32 = 0x5800_0000;
const FORCE_VALUE: Prop = Prop::named("forceValue");
const RESULTS_CAP: usize = 4096;
const PAGE: Prop = Prop::named("page");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContributionId(u64);

impl ContributionId {
   pub fn next() -> Self {
      static NEXT: AtomicU64 = AtomicU64::new(1);
      Self(NEXT.fetch_add(1, Ordering::Relaxed))
   }
}

impl fmt::Display for ContributionId {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      self.0.fmt(formatter)
   }
}

/// Something a plugin added to a [`Target`]. Its callbacks stay in the
/// engine.
pub struct Contribution {
   pub id:          ContributionId,
   pub plugin:      String,
   pub target:      Target,
   pub label:       String,
   pub icon:        Option<String>,
   pub tint:        Option<i64>,
   pub danger:      bool,
   pub priority:    i32,
   /// Shown only where the plugin's `visible` says so.
   pub conditional: bool,
}

/// What a contribution's nodes are about.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Place {
   Screen,
   Message(String),
   User(Option<String>),
   /// A DM, whose one recipient member decorations are about.
   Channel(String),
}

#[derive(Clone)]
pub struct Rendered {
   pub text:  String,
   pub color: Option<i64>,
   /// Under a message's content: the color of its author and text (32-bit
   /// ARGB).
   pub tint:  Option<i64>,
   /// Images drawn after names by member and header decorations.
   pub icons: Vec<String>,
}

/// A plugin's answer for one place: the text it renders, or whether its row
/// shows.
#[derive(Clone)]
pub enum Outcome {
   Text(Option<Rendered>),
   Shown(bool),
   /// A profile tab's page.
   Page(Vec<Block>),
}

/// A piece of a profile tab's page. `handler` indexes the callbacks the
/// engine kept for the page.
#[derive(Clone)]
pub enum Block {
   Text {
      title: Option<String>,
      text:  String,
      color: Option<i64>,
   },
   Entry {
      avatar:   Option<String>,
      title:    String,
      subtitle: Option<String>,
      text:     String,
      /// Small images after the title, like badges.
      badges:   Vec<String>,
      /// Images under the text.
      images:   Vec<String>,
      /// A row under the text. Actions without a handler are plain text.
      actions:  Vec<BlockAction>,
      /// A column at the entry's end, like votes.
      side:     Vec<BlockAction>,
      /// Runs on a long press of the entry.
      hold:     Option<u32>,
   },
   /// Buttons side by side.
   Buttons(Vec<BlockAction>),
   Input {
      placeholder: String,
      handler:     u32,
      /// A send button, which submits like the keyboard's send key.
      send:        bool,
   },
   Button {
      label:   String,
      danger:  bool,
      handler: u32,
   },
}

#[derive(Clone)]
pub struct BlockAction {
   pub label:   String,
   pub danger:  bool,
   /// 32-bit ARGB, over `danger`.
   pub color:   Option<i64>,
   pub handler: Option<u32>,
}

/// What plugins made of a message about to be sent.
pub enum Sent {
   Cancel,
   Text(String),
   Unchanged,
}

/// A synchronous Rust-side prop patch applied inside `rewrite` with zero JS
/// overhead per batch.
struct PropPatch {
   id:       u64,
   plugin:   String,
   matcher:  NodeMatch,
   target:   PatchTarget,
   props:    Props,
   add_on:   Vec<String>,
   once:     bool,
   deadline: Option<Instant>,
}

struct EventHookMeta {
   id:        u64,
   plugin:    String,
   event:     Key,
   intercept: bool,
}

struct NodeWatcher {
   id:      u64,
   plugin:  String,
   matcher: NodeMatch,
}

/// What plugin callbacks receive as `ctx`.
#[derive(Default)]
pub struct UiContext {
   pub channel_id:      Option<String>,
   pub message_id:      Option<String>,
   pub user_id:         Option<String>,
   pub current_user_id: Option<String>,
   /// The message as Phoenix cached it, JSON.
   pub message:         Option<String>,
}

#[derive(Clone)]
enum Handler {
   Press {
      contribution: ContributionId,
      place:        Place,
      dismiss:      bool,
   },
   /// A command row in the `/` suggestions.
   Pick(String),
   /// A plugin's profile tab.
   Tab { contribution: ContributionId },
   /// A press in a profile tab's page.
   Block {
      contribution: ContributionId,
      index:        u32,
   },
   /// A text box in a profile tab's page.
   Input {
      contribution: ContributionId,
      index:        u32,
   },
   /// A profile tab page's content, which reports its height.
   Page { contribution: ContributionId },
   /// The send button of a text box in a profile tab's page.
   Send {
      contribution: ContributionId,
      index:        u32,
   },
   /// A row of a plugin menu sheet, or with `index` -1 the sheet itself,
   /// which says when it was swiped away.
   Choice { menu: u32, index: i32 },
   /// An entry in a profile tab's page that takes long presses.
   Hold {
      contribution: ContributionId,
      index:        u32,
   },
   /// Back button of a plugin full-screen page.
   PageBack(u32),
   /// Action button on a plugin full-screen page.
   PageButton { page: u32, index: u32 },
   /// Tap on an in-app notification banner card.
   BannerPress(u32),
   /// Tap on an in-app notification banner's dismiss button.
   BannerDismiss(u32),
   /// Events on a raw JS-mounted subtree (`phoenix.ui.mount`).
   Mounted {
      mount:   u32,
      press:   Option<u32>,
      dismiss: Option<u32>,
   },
}

/// A node to create, with the handler its subtree answers presses with.
pub struct Draft {
   kind:  ViewKind,
   props: Props,
   kids:  Vec<Self>,
   press: Option<Handler>,
   src:   Option<u32>,
}

impl Draft {
   fn from_node(node: &Node) -> Self {
      Self {
         kind:  node.kind.clone(),
         props: node.props.clone(),
         kids:  node.kids.iter().map(Self::from_node).collect(),
         press: None,
         src:   node.src,
      }
   }

   fn find(&mut self, src: u32) -> Option<&mut Self> {
      if self.src == Some(src) {
         return Some(self);
      }
      self.kids.iter_mut().find_map(|kid| kid.find(src))
   }

   fn set(&mut self, key: Prop, value: Option<Value>) {
      set_prop(&mut self.props, key, value);
   }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
   Target(Target),
   Suggestions,
   /// A plugin menu sheet.
   Menu(u32),
   /// A plugin full-screen page pushed onto a navigator.
   Page(u32),
   /// A native top in-app notification banner attached to the root view.
   Banner(u32),
   /// A raw JS-mounted subtree (`phoenix.ui.mount`).
   Mounted(u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Before {
   Start,
   Node(i32),
   End,
}

/// Plugin nodes living under one of Phoenix's parents.
struct Slot {
   kind:   SlotKind,
   /// The Phoenix node the slot was opened for.
   anchor: i32,
   parent: i32,
   before: Before,
   place:  Place,
   roots:  Vec<i32>,
   all:    Vec<i32>,
}

#[derive(Default)]
struct Patches {
   mirror:                 Mirror,
   templates:              Templates,
   seen:                   Seen,
   contributions:          Vec<Contribution>,
   commands:               Vec<Command>,
   interceptors:           usize,
   pickers:                usize,
   event_hooks:            Vec<EventHookMeta>,
   node_watchers:          Vec<NodeWatcher>,
   prop_rules:             Vec<PropPatch>,
   /// Live lists discovered for each `PatchTarget::List` prop patch (by patch
   /// id).
   patched_lists:          HashMap<i32, HashSet<u64>>,
   slots:                  Vec<Slot>,
   anchors:                HashMap<Target, HashSet<i32>>,
   handlers:               HashMap<i32, Handler>,
   results:                HashMap<(ContributionId, Place), Outcome>,
   pending:                HashSet<(ContributionId, Place)>,
   next_id:                i32,
   /// The channel last seen open.
   channel:                Option<String>,
   suggestions:            commands::Suggestions,
   /// Sends plugins already saw, let through once when Rust gets them back.
   resent:                 Vec<Vec<u8>>,
   /// The user a member or header decoration's place is about, once the
   /// worker read it from Phoenix's cache.
   users:                  HashMap<Place, Option<String>>,
   /// Member slots that take the room a stretched name had, by parent.
   grown:                  HashSet<i32>,
   /// Ops for Phoenix's own nodes, sent after the batch that creates them.
   late:                   Vec<Op>,
   /// Message text color plugins asked for, by message id.
   tints:                  HashMap<String, i64>,
   /// Phoenix's author and content text nodes: their message and props as
   /// Phoenix set them.
   texts:                  HashMap<i32, (String, Props)>,
   /// The plugin tab open on each profile pager.
   shown:                  HashMap<i32, ContributionId>,
   /// The height Rust last gave each pager.
   rust_heights:           HashMap<i32, Value>,
   /// The height of each plugin page, by pager.
   page_heights:           HashMap<(i32, ContributionId), f64>,
   /// What is typed in the text boxes of plugin pages, so a redraw keeps it.
   typed:                  HashMap<(ContributionId, Place, u32), String>,
   /// A plugin tab's text and bar, by the tab.
   tab_parts:              HashMap<i32, (i32, i32)>,
   /// Open menu sheets and who waits for them.
   menus:                  HashMap<u32, Sender<i32>>,
   /// Pagers whose own tab labels we color, since a plugin tab was opened:
   /// their animation stops at Phoenix's last tab and would light it up.
   static_tabs:            HashSet<i32>,
   /// Accessibility ids hidden by `phoenix.ui.hide`, with active refcount.
   hidden:                 HashMap<String, usize>,
   /// `(handle, plugin, aid)` entries for `phoenix.ui.hide`.
   hidden_entries:         Vec<(u64, String, String)>,
   /// Node ids on which `phoenix.ui.hide` currently holds `hidden: true`
   /// (`View.GONE`). Reconciled on every batch so dynamic swaps (like
   /// `"Record Voice Message"` -> `"send"` inside the same wrapper) restore
   /// visibility automatically.
   hidden_nodes:           HashSet<i32>,
   /// When the root view first became ready, so cold-start banners wait for
   /// the splash screen to clear.
   root_ready_at:          Option<Instant>,
   /// Banners waiting for the root view / cold-start settle delay.
   pending_banners:        Vec<BannerSpec>,
   banner_flush_scheduled: bool,
   /// Tap actions for currently visible in-app notification banners, by id.
   banner_actions:         HashMap<u32, BannerAction>,
}

static ROOT_ID: AtomicI32 = AtomicI32::new(0);

static PATCHES: LazyLock<Mutex<Patches>> = LazyLock::new(|| {
   Mutex::new(Patches {
      next_id: FIRST_ID,
      ..Patches::default()
   })
});
/// Set once a plugin adds anything, so builds without UI plugins never
/// decode a batch.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Your messages' text as Discord has it, by the text plugin hooks turned it
/// into. Phoenix fills the edit box from the hooked text, and saving it would
/// post what the plugins added.
static ORIGINALS: Mutex<VecDeque<(String, String)>> = Mutex::new(VecDeque::new());
static GUARDING: AtomicBool = AtomicBool::new(false);
const ORIGINALS_CAP: usize = 256;

/// Remembers that a plugin hook turned your message `original` into `shown`.
pub fn remember_original(shown: String, original: String) {
   let mut originals = ORIGINALS.lock().unwrap_or_else(PoisonError::into_inner);
   originals.retain(|(known, _)| *known != shown);
   if originals.len() == ORIGINALS_CAP {
      originals.pop_front();
   }
   originals.push_back((shown, original));
   drop(originals);
   GUARDING.store(true, Ordering::Release);
}

fn patches() -> MutexGuard<'static, Patches> {
   PATCHES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Hooks the UI bridge and starts the worker that runs plugin callbacks.
pub fn start() {
   let (sender, receiver) = mpsc::channel::<Job>();
   if WORKER.set(sender).is_err() {
      log::warn!("patches: already started");
      return;
   }
   emoji::install();
   bridge::install(Box::new(PatchSink));
   // The chat header and message box are built once, before the first
   // gateway event would attach the bridge.
   #[cfg(target_os = "android")]
   bridge::jni::attach();
   thread::spawn(move || {
      for job in receiver {
         let ops = run(job);
         if !ops.is_empty() && !bridge::apply_ops(&encode(&ops)) {
            log::warn!("patches: the UI update did not apply");
         }
      }
   });
}

/// Adds `contribution` and draws it wherever its target is on screen.
pub fn add(contribution: Contribution) {
   ACTIVE.store(true, Ordering::Release);
   let target = contribution.target;
   let mut state = patches();
   state.contributions.push(contribution);
   state
      .contributions
      .sort_by_key(|contribution| std::cmp::Reverse(contribution.priority));
   drop(state);
   queue(Job::Refresh(target));
}

pub fn remove(id: ContributionId) {
   let mut state = patches();
   let Some(at) = state
      .contributions
      .iter()
      .position(|contribution| contribution.id == id)
   else {
      return;
   };
   let target = state.contributions.remove(at).target;
   state
      .results
      .retain(|(contribution, _), _| *contribution != id);
   drop(state);
   queue(Job::Refresh(target));
}

/// Draws `id` again, asking its plugin anew: everywhere, or only at `place`,
/// where the old result stays up until the new one is in.
pub fn update(id: ContributionId, place: Option<Place>) {
   let mut state = patches();
   if let Some(place) = place {
      if state.pending.insert((id, place.clone())) {
         queue(Job::Evaluate(id, place));
      }
      return;
   }
   let Some(target) = state
      .contributions
      .iter()
      .find(|contribution| contribution.id == id)
      .map(|contribution| contribution.target)
   else {
      return;
   };
   state
      .results
      .retain(|(contribution, _), _| *contribution != id);
   drop(state);
   queue(Job::Refresh(target));
}

/// Draws member or header decoration `id` again for `user`: anew where it
/// is on screen, and when it next shows elsewhere.
pub fn update_user(id: ContributionId, user: &str) {
   let mut state = patches();
   let Some(target) = state
      .contributions
      .iter()
      .find(|contribution| contribution.id == id)
      .map(|contribution| contribution.target)
   else {
      return;
   };
   let Patches {
      slots,
      users,
      results,
      pending,
      ..
   } = &mut *state;
   let about = |place: &Place| {
      match place {
         Place::User(known) => known.as_deref() == Some(user),
         _ => {
            users
               .get(place)
               .is_some_and(|known| known.as_deref() == Some(user))
         },
      }
   };
   let shown = slots
      .iter()
      .filter(|slot| slot.kind == SlotKind::Target(target) && about(&slot.place))
      .map(|slot| slot.place.clone())
      .collect::<HashSet<_>>();
   results.retain(|(contribution, place), _| {
      *contribution != id || shown.contains(place) || !about(place)
   });
   let asked = shown
      .into_iter()
      .filter(|place| pending.insert((id, place.clone())))
      .collect::<Vec<_>>();
   drop(state);
   for place in asked {
      queue(Job::Evaluate(id, place));
   }
}

/// What [`update_button`] does to a button's tint.
#[derive(Clone, Copy)]
pub enum TintChange {
   Keep,
   Clear,
   Set(i64),
}

/// Updates a button contribution's icon, label, or tint and redraws its target.
pub fn update_button(
   id: ContributionId,
   icon: Option<String>,
   label: Option<String>,
   tint: TintChange,
) {
   let mut state = patches();
   let Some(contribution) = state.contributions.iter_mut().find(|known| known.id == id) else {
      return;
   };
   if let Some(icon) = icon {
      contribution.icon = Some(icon);
   }
   if let Some(label) = label {
      contribution.label = label;
   }
   match tint {
      TintChange::Keep => {},
      TintChange::Clear => contribution.tint = None,
      TintChange::Set(color) => contribution.tint = Some(color),
   }
   let target = contribution.target;
   drop(state);
   queue(Job::Refresh(target));
}

/// The current Phoenix root view id, if seen.
pub fn root_id() -> Option<i32> {
   patches().mirror.root()
}

/// Runs `with` on the mirrored view tree under `root`, or under the root node
/// when `root` is `None`, for the debug inspector (`GET /tree`).
pub fn with_tree<R>(
   root: Option<i32>,
   depth: usize,
   with: impl FnOnce(Option<TreeJson<'_>>) -> R,
) -> R {
   let state = patches();
   let id = root.or_else(|| state.mirror.root());
   with(id.map(|id| TreeJson::new(&state.mirror, id, depth)))
}

/// A plugin settled a `render` or `visible` it returned as a promise.
pub fn evaluated(contribution: ContributionId, place: Place, outcome: Outcome) {
   queue(Job::Evaluated(contribution, place, outcome));
}

/// How many `onSend` interceptors plugins hold, so sends are taken only
/// while one exists.
pub fn set_interceptors(count: usize) {
   if count > 0 {
      ACTIVE.store(true, Ordering::Release);
   }
   patches().interceptors = count;
}

/// How many `onEmojiPick` handlers plugins hold, so picker presses are taken,
/// and Nitro-locked emojis offered, only while one exists.
pub fn set_emoji_pickers(count: usize) {
   if count > 0 {
      ACTIVE.store(true, Ordering::Release);
   }
   patches().pickers = count;
   emoji::unlock(count > 0);
}

/// Drops everything `plugin` added.
pub fn forget(plugin: &str) {
   let mut state = patches();
   let targets = state
      .contributions
      .iter()
      .filter(|contribution| contribution.plugin == plugin)
      .map(|contribution| contribution.target)
      .collect::<HashSet<_>>();
   let gone = state
      .contributions
      .iter()
      .filter(|contribution| contribution.plugin == plugin)
      .map(|contribution| contribution.id)
      .collect::<HashSet<_>>();
   state
      .contributions
      .retain(|contribution| contribution.plugin != plugin);
   state
      .results
      .retain(|(contribution, _), _| !gone.contains(contribution));
   let had_commands = state
      .commands
      .iter()
      .any(|command| command.plugin == plugin);
   state.commands.retain(|command| command.plugin != plugin);
   let mut unhidden = Vec::new();
   state.hidden_entries.retain(|(_, owner, aid)| {
      if owner == plugin {
         unhidden.push(aid.clone());
         false
      } else {
         true
      }
   });
   for aid in &unhidden {
      if let Some(count) = state.hidden.get_mut(aid) {
         *count = count.saturating_sub(1);
         if *count == 0 {
            state.hidden.remove(aid);
         }
      }
   }
   state.event_hooks.retain(|hook| hook.plugin != plugin);
   state
      .node_watchers
      .retain(|watcher| watcher.plugin != plugin);
   let had_list_patches = state
      .prop_rules
      .iter()
      .any(|patch| patch.plugin == plugin && patch.target == PatchTarget::List);
   state.prop_rules.retain(|patch| patch.plugin != plugin);
   drop(state);
   if !unhidden.is_empty() {
      queue(Job::RefreshHidden);
   }
   for target in targets {
      queue(Job::Refresh(target));
   }
   if had_commands {
      queue(Job::Suggest);
   }
   if had_list_patches {
      queue(Job::RefreshPropPatches);
   }
}

const HIDDEN: Prop = Prop::named("hidden");

fn number(value: &Value) -> Option<f64> {
   match value {
      Value::F64(number) => Some(*number),
      Value::I32(number) => Some(f64::from(*number)),
      _ => None,
   }
}

const BACKGROUND: Prop = Prop::named("background");
