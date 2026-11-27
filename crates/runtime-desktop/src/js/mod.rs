//! The desktop share of the `phoenix` object: `phoenix.ui` and message
//! accessories, and `phoenix.native` for the console.

pub mod native;
pub mod ui;

use std::{cell::RefCell, collections::HashMap};

use rquickjs::{Ctx, Function, JsLifetime};

#[cfg(target_arch = "x86_64")]
use crate::counts::{self, Count};
use crate::{
   accessories, composer, context_menu, double_click, emoji,
   js::ui::{DesktopAccessory, DesktopAction, DesktopMember},
   members::Place,
};

/// What plugins registered with the desktop UI, kept in the runtime's userdata
/// next to the core's registry.
#[derive(Default)]
pub struct Desktop<'js> {
   /// `phoenix.ui.addMessageAccessory` callbacks.
   pub accessories: RefCell<Vec<DesktopAccessory<'js>>>,
   /// `phoenix.ui.addMemberDecoration` callbacks.
   pub members: RefCell<Vec<DesktopMember<'js>>>,
   /// `phoenix.ui.addMessageHeaderDecoration` callbacks.
   pub headers: RefCell<Vec<DesktopMember<'js>>>,
   /// `phoenix.ui.addMessageAction` callbacks.
   pub actions: RefCell<Vec<DesktopAction<'js>>>,
   /// Active `phoenix.ui.openPage` button callbacks, keyed by button ID.
   pub page_buttons: RefCell<HashMap<u64, (u64, String, Function<'js>)>>,
   /// `phoenix.messages.onEmojiPick` handlers by id and plugin. Desktop has
   /// no picker presses to give them, only the unlock they turn on.
   pub emoji_pickers: RefCell<Vec<(u64, String)>>,
   /// `phoenix.ui.onEvent("rowDoubleTap")` handlers by id and plugin.
   pub double_taps: RefCell<Vec<(u64, String, Function<'js>)>>,
   /// `phoenix.ui.hide` holds by id and plugin.
   pub hides: RefCell<Vec<(u64, String)>>,
   /// `phoenix.ui.hideCount` holds by id and plugin.
   #[cfg(target_arch = "x86_64")]
   pub hidden_counts: RefCell<Vec<(u64, String, Count)>>,
}

impl<'js> Desktop<'js> {
   /// The decorations `place` draws.
   pub const fn decorations(&self, place: Place) -> &RefCell<Vec<DesktopMember<'js>>> {
      match place {
         Place::Members => &self.members,
         Place::Headers => &self.headers,
      }
   }
}

// SAFETY: every field is bound to 'js, and Changed swaps exactly that lifetime.
unsafe impl<'js> JsLifetime<'js> for Desktop<'js> {
   type Changed<'to> = Desktop<'to>;
}

/// Drops the accessories, member decorations, context menu actions, open page
/// callbacks, emoji pick handlers and hidden counts `name` registered and
/// redraws Discord without them.
pub fn unload(ctx: &Ctx<'_>, name: &str) {
   let Some(registry) = ctx.userdata::<Desktop<'_>>() else {
      return;
   };
   let mut list = registry.accessories.borrow_mut();
   let before = list.len();
   list.retain(|entry| entry.plugin != name);
   if list.len() != before {
      accessories::set_active_count(list.len());
      accessories::clear_cache();
      accessories::invalidate_discord_windows();
   }
   drop(list);

   for place in [Place::Members, Place::Headers] {
      let mut decorations = registry.decorations(place).borrow_mut();
      let before = decorations.len();
      decorations.retain(|entry| entry.plugin != name);
      if decorations.len() != before {
         place.decorations().set_active_count(decorations.len());
         place.decorations().clear_cache();
         accessories::invalidate_discord_windows();
      }
   }

   let mut actions = registry.actions.borrow_mut();
   let before_actions = actions.len();
   actions.retain(|entry| entry.plugin != name);
   if actions.len() != before_actions {
      let meta = actions.iter().map(DesktopAction::meta).collect();
      context_menu::sync_actions(meta);
   }
   drop(actions);

   registry
      .page_buttons
      .borrow_mut()
      .retain(|_, (_, plugin, _)| plugin != name);

   let mut pickers = registry.emoji_pickers.borrow_mut();
   pickers.retain(|(_, plugin)| plugin != name);
   emoji::unlock(!pickers.is_empty());
   drop(pickers);

   let mut taps = registry.double_taps.borrow_mut();
   taps.retain(|(_, plugin, _)| plugin != name);
   double_click::set_active(!taps.is_empty());
   drop(taps);

   registry.hides.borrow_mut().retain(|(_, plugin)| {
      let held = plugin == name;
      if held {
         composer::hide_gift(false);
      }
      !held
   });

   #[cfg(target_arch = "x86_64")]
   {
      let mut hidden = registry.hidden_counts.borrow_mut();
      hidden.retain(|(_, plugin, _)| plugin != name);
      counts::hide(hidden.iter().map(|&(.., count)| count));
   }
}
