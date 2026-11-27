//! Java-to-Rust view events: presses on plugin nodes, page events and sends.

use std::sync::atomic::Ordering;

use phoenix_ui_mobile::{
   Prop,
   inspect::WireJson,
   prop,
};
use phoenix_wire::{
   DispatchEvent,
   Key,
   Value,
   decode_dispatch,
   encode_batch,
   encode_view_event,
};

use crate::{
   bridge,
   bridge::Action,
   patches::{
      ACTIVE,
      EventHookMeta,
      FIRST_ID,
      Handler,
      Patches,
      RESULTS_CAP,
      aids::message_id,
      commands,
      number,
      patches,
      worker::{
         Job,
         queue,
      },
   },
};

/// Registers a raw view event hook (`phoenix.ui.onEvent`).
pub fn add_event_hook(id: u64, plugin: String, event: Key, intercept: bool) {
   ACTIVE.store(true, Ordering::Release);
   patches().event_hooks.push(EventHookMeta {
      id,
      plugin,
      event,
      intercept,
   });
}

pub fn remove_event_hook(id: u64) {
   patches().event_hooks.retain(|hook| hook.id != id);
}

/// Injects a raw view event (`phoenix.ui.dispatch`) straight to Phoenix's Rust
/// guest.
pub fn dispatch_event(node: i32, event: Key, payload: &Value) -> bool {
   let frame = encode_view_event(node, event, payload);
   patches().resent.push(frame.clone());
   if !bridge::inject(&frame) {
      patches().resent.retain(|sent| *sent != frame);
      return false;
   }
   true
}

const DISMISS: Key = Prop::named("dismiss").key();
const ROW_LONG_PRESS: Key = Prop::named("rowLongPress").key();
const PAGE_EVENTS: [Key; 3] = [
   prop!("pageSelect").key(),
   prop!("pageSettle").key(),
   prop!("pageScroll").key(),
];
const LAYOUT: Key = Prop::named("layout").key();
const LONG_PRESS: Key = Prop::named("longPress").key();

impl Patches {
   /// Takes presses on plugin nodes, long presses, and sends out of a
   /// Java-to-Rust dispatch.
   pub fn filter(&mut self, frame: &[u8]) -> Action {
      let Ok(items) = decode_dispatch(frame) else {
         return Action::Pass;
      };
      let mut changed = false;
      let mut frames = Vec::new();
      for item in &items {
         let raw = frame[item.offset..item.offset + item.len].to_vec();
         if let Some(at) = self.resent.iter().position(|sent| *sent == raw) {
            self.resent.swap_remove(at);
            frames.push(raw);
            continue;
         }
         let DispatchEvent::View {
            node,
            event,
            payload,
         } = &item.event
         else {
            if phoenix_runtime::remote::enabled()
               && let DispatchEvent::Host { event, .. } = &item.event
            {
               phoenix_runtime::remote::record(&crate::debug::Trace::HostEvent { event: *event });
            }
            frames.push(raw);
            continue;
         };
         changed |= self.take_view(*node, *event, payload, raw, &mut frames);
      }
      if !changed {
         return Action::Pass;
      }
      match frames.len() {
         0 => Action::Drop,
         1 => Action::Replace(frames.remove(0)),
         _ => Action::Replace(encode_batch(&frames)),
      }
   }

   fn take_view(
      &mut self,
      node: i32,
      event: Key,
      payload: &Value,
      raw: Vec<u8>,
      frames: &mut Vec<Vec<u8>>,
   ) -> bool {
      if phoenix_runtime::remote::enabled() {
         self.record_view(node, event, payload);
      }
      if event == Prop::NAVIGATE.key()
         && let Some(page) = self.top_page_on(node)
      {
         queue(Job::ClosePage(page));
         return true;
      }
      if PAGE_EVENTS.contains(&event) && self.beyond_rust(node, payload) {
         // About a plugin page, which Rust does not know.
         return true;
      }
      if event == Prop::PRESS.key()
         && let Some((pager, index)) = self.rust_tab(node)
         && self.shown.contains_key(&pager)
      {
         queue(Job::Leave { pager, index });
      }
      if event == ROW_LONG_PRESS
         && let Some(index) = payload.field(Prop::INDEX.key()).and_then(Value::as_i32)
      {
         self.seen.long_pressed = self.row_message(node, index);
      }
      if !self.event_hooks.is_empty()
         && self.event_hooks.iter().any(|hook| hook.event == event)
         && self.hook_view(node, event, payload, &raw)
      {
         return true;
      }
      if event == Prop::CHANGE_TEXT.key() && self.mirror.is_composer(node) {
         let text = payload
            .field(Prop::TEXT.key())
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
         self.mirror.set_composer_text(node, text);
         if !self.commands.is_empty() {
            queue(Job::Suggest);
         }
      }
      if node >= FIRST_ID
         && let Some(handler) = self.handlers.get(&node).cloned()
      {
         if event == Prop::PRESS.key() {
            self.pressed(node, handler, frames);
         } else {
            self.page_event(node, &handler, event, payload);
         }
         return true;
      }
      if self.pickers > 0
         && event == Prop::PRESS.key()
         && let Some(id) = self
            .mirror
            .accessibility_id(node)
            .and_then(|aid| aid.strip_prefix("expression.emoji:custom:"))
         && self.in_message_picker(node)
         && let Some((input, text)) = self.mirror.composer()
      {
         queue(Job::Emoji {
            input,
            id: id.to_owned(),
            text: text.to_owned(),
            frame: raw,
         });
         return true;
      }
      if let Some(input) = self.sending(node, event)
         && let Some((_, text)) = self.mirror.composer()
      {
         queue(Job::Submit {
            input,
            text: text.to_owned(),
            frame: raw,
         });
         return true;
      }
      frames.push(raw);
      false
   }

   fn record_view(&self, node: i32, event: Key, payload: &Value) {
      phoenix_runtime::remote::record(&crate::debug::Trace::View {
         node,
         event: Prop::from(event),
         aid: self.mirror.accessibility_id(node),
         payload: WireJson(payload),
      });
   }

   fn hook_view(&self, node: i32, event: Key, payload: &Value, raw: &[u8]) -> bool {
      let intercept = self
         .event_hooks
         .iter()
         .any(|hook| hook.event == event && hook.intercept);
      let message_id = payload
         .field(Prop::INDEX.key())
         .and_then(Value::as_i32)
         .and_then(|index| self.row_message(node, index));
      let aid = self.mirror.accessibility_id(node).map(str::to_owned);
      queue(Job::ViewEvent {
         node,
         event,
         payload: payload.clone(),
         aid,
         message_id,
         frame: intercept.then(|| raw.to_vec()),
      });
      intercept
   }

   fn pressed(&self, node: i32, handler: Handler, frames: &mut Vec<Vec<u8>>) {
      match handler {
         Handler::Press {
            contribution,
            place,
            dismiss,
         } => {
            queue(Job::Press(contribution, place));
            let slot = self.slots.iter().find(|slot| slot.all.contains(&node));
            if dismiss && let Some(sheet) = slot.and_then(|slot| self.mirror.sheet_of(slot.parent))
            {
               frames.push(encode_view_event(sheet, DISMISS, &Value::Obj(Vec::new())));
            }
         },
         Handler::Tab { contribution } => {
            if let Some(slot) = self.slots.iter().find(|slot| slot.all.contains(&node)) {
               queue(Job::Select {
                  pager: slot.anchor,
                  contribution,
               });
            }
         },
         Handler::Block {
            contribution,
            index,
         } => {
            if let Some(slot) = self.slots.iter().find(|slot| slot.all.contains(&node)) {
               queue(Job::Block {
                  contribution,
                  place: slot.place.clone(),
                  index,
                  text: None,
                  input: None,
               });
            }
         },
         Handler::Send {
            contribution,
            index,
         } => {
            if let Some(slot) = self.slots.iter().find(|slot| slot.all.contains(&node)) {
               queue(Job::Send {
                  contribution,
                  place: slot.place.clone(),
                  index,
               });
            }
         },
         Handler::Choice { menu, index } => queue(Job::CloseMenu { menu, index }),
         Handler::PageBack(page) => queue(Job::ClosePage(page)),
         Handler::PageButton { page, index } => queue(Job::PagePress { page, index }),
         Handler::BannerPress(id) => queue(Job::BannerPressed(id)),
         Handler::BannerDismiss(id) => queue(Job::DismissBanner(id)),
         Handler::Mounted {
            mount,
            press: Some(callback),
            ..
         } => queue(Job::MountCallback { mount, callback }),
         Handler::Mounted { press: None, .. }
         | Handler::Input { .. }
         | Handler::Page { .. }
         | Handler::Hold { .. } => {},
         Handler::Pick(name) => {
            if let Some((input, _)) = self.mirror.composer() {
               frames.push(commands::change_text(input, &format!("/{name} ")));
               queue(Job::Pick { input, name });
            }
         },
      }
   }

   /// Takes typing, sends and heights from a plugin page.
   fn page_event(&mut self, node: i32, handler: &Handler, event: Key, payload: &Value) {
      let slot = self.slots.iter().find(|slot| slot.all.contains(&node));
      match *handler {
         Handler::Input {
            contribution,
            index,
         } => {
            let Some(place) = slot.map(|slot| slot.place.clone()) else {
               return;
            };
            if event == Prop::CHANGE_TEXT.key() {
               let text = payload
                  .field(Prop::TEXT.key())
                  .and_then(Value::as_str)
                  .or_else(|| payload.as_str())
                  .unwrap_or_default();
               if self.typed.len() >= RESULTS_CAP {
                  self.typed.clear();
               }
               self
                  .typed
                  .insert((contribution, place, index), text.to_owned());
            } else if event == Prop::SUBMIT.key() {
               let text = self.typed.remove(&(contribution, place.clone(), index));
               queue(Job::Block {
                  contribution,
                  place,
                  index,
                  text: Some(text.unwrap_or_default()),
                  input: Some(node),
               });
            }
         },
         Handler::Hold {
            contribution,
            index,
         } if event == LONG_PRESS => {
            if let Some(slot) = slot {
               queue(Job::Block {
                  contribution,
                  place: slot.place.clone(),
                  index,
                  text: None,
                  input: None,
               });
            }
         },
         Handler::Choice { menu, .. } if event == DISMISS => {
            queue(Job::CloseMenu { menu, index: -1 });
         },
         Handler::Mounted {
            mount,
            dismiss: Some(callback),
            ..
         } if event == DISMISS => {
            queue(Job::MountCallback { mount, callback });
         },
         Handler::Page { contribution } if event == LAYOUT => {
            if let (Some(slot), Some(height)) =
               (slot, payload.field(Prop::HEIGHT.key()).and_then(number))
            {
               queue(Job::PageHeight {
                  pager: slot.anchor,
                  contribution,
                  height,
               });
            }
         },
         _ => {},
      }
   }

   /// The composer input when `event` on `node` sends a message and a plugin
   /// wants to see it first.
   fn sending(&self, node: i32, event: Key) -> Option<i32> {
      if self.commands.is_empty() && self.interceptors == 0 {
         return None;
      }
      let (input, _) = self.mirror.composer()?;
      let submit = event == Prop::SUBMIT.key() && self.mirror.is_composer(node);
      let send = event == Prop::PRESS.key() && self.mirror.accessibility_id(node) == Some("send");
      (submit || send).then_some(input)
   }

   /// The message id in row `index` of list `node`.
   fn row_message(&self, node: i32, index: i32) -> Option<String> {
      let row = self.mirror.row(node, index)?;
      let hit = self
         .mirror
         .find_down(row, &|aid| message_id(aid).is_some(), 8)?;
      message_id(self.mirror.accessibility_id(hit)?).map(str::to_owned)
   }
}
