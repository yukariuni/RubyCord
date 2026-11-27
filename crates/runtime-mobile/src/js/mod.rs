//! The mobile share of the `phoenix` object: `phoenix.ui`, `phoenix.commands`,
//! `phoenix.bridge` and the message box hooks.

pub mod bridge;
pub mod messages;
pub mod mount;
pub mod page;
pub mod ui;
pub mod view;
pub mod wire;

use std::{
   cell::RefCell,
   collections::HashMap,
};

use rquickjs::{
   Ctx,
   Function,
   JsLifetime,
};

use crate::{
   js::{
      ui::Callbacks,
      view::EventHook,
   },
   patches::{
      self,
      ContributionId,
   },
};

/// What plugins registered with the mobile UI, kept in the runtime's userdata
/// next to the core's registry.
#[derive(Default)]
pub struct Mobile<'js> {
   pub ui:            RefCell<HashMap<ContributionId, Callbacks<'js>>>,
   /// Slash commands by lowercase name, with the plugin that registered each.
   pub commands:      RefCell<HashMap<String, (String, Function<'js>)>>,
   /// `phoenix.messages.onEmojiPick` handlers in registration order, by id.
   pub pickers:       RefCell<Vec<(u64, String, Function<'js>)>>,
   /// `phoenix.ui.onEvent` handlers, by id.
   pub event_hooks:   RefCell<Vec<EventHook<'js>>>,
   /// `phoenix.ui.onNode` watchers, by id.
   pub node_watchers: RefCell<HashMap<u64, (String, Function<'js>)>>,
   /// Open full-screen plugin pages (`phoenix.ui.openPage`) and their button
   /// callbacks, by page id.
   pub open_pages:    RefCell<HashMap<u32, (String, Vec<Function<'js>>)>>,
   /// Active in-app notification banner tap callbacks (`phoenix.ui.notify`), by
   /// banner id.
   pub banners:       RefCell<HashMap<u32, (String, Function<'js>)>>,
   /// Active raw JS-mounted subtrees (`phoenix.ui.mount`) and their callbacks,
   /// by mount id.
   pub mounts:        RefCell<HashMap<u32, (String, Vec<Function<'js>>)>>,
}

// SAFETY: every field is bound to 'js, and Changed swaps exactly that lifetime.
unsafe impl<'js> JsLifetime<'js> for Mobile<'js> {
   type Changed<'to> = Mobile<'to>;
}

/// Drops what `name` registered and takes down its pages, banners and mounts.
pub fn unload(ctx: &Ctx<'_>, name: &str) {
   if let Some(core) = ctx.userdata::<phoenix_runtime::js::Registry<'_>>() {
      patches::set_interceptors(core.interceptors.borrow().len());
   }
   let Some(registry) = ctx.userdata::<Mobile<'_>>() else {
      return;
   };
   registry
      .ui
      .borrow_mut()
      .retain(|_, callbacks| callbacks.plugin != name);
   registry
      .commands
      .borrow_mut()
      .retain(|_, (plugin, _)| plugin != name);
   let mut pickers = registry.pickers.borrow_mut();
   pickers.retain(|(_, plugin, _)| plugin != name);
   patches::set_emoji_pickers(pickers.len());
   drop(pickers);
   registry
      .event_hooks
      .borrow_mut()
      .retain(|hook| hook.plugin != name);
   registry
      .node_watchers
      .borrow_mut()
      .retain(|_, (plugin, _)| plugin != name);
   for id in take_owned(&registry.open_pages, name) {
      patches::pages::close_page(id);
   }
   for id in take_owned(&registry.banners, name) {
      patches::banners::dismiss_banner(id);
   }
   for id in take_owned(&registry.mounts, name) {
      patches::mount::unmount(id);
   }
   crate::bridge::forget_haptics(name);
   patches::forget(name);
}

/// Removes the entries `plugin` owns from `map` and returns their ids.
fn take_owned<T>(map: &RefCell<HashMap<u32, (String, T)>>, plugin: &str) -> Vec<u32> {
   let mut map = map.borrow_mut();
   let owned = map
      .iter()
      .filter(|(_, (owner, _))| owner == plugin)
      .map(|(id, _)| *id)
      .collect::<Vec<_>>();
   for id in &owned {
      map.remove(id);
   }
   owned
}
