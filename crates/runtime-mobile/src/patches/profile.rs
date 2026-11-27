//! Plugin tabs on profiles: opening them, their pages' heights and text boxes.

use std::sync::atomic::{
   AtomicI32,
   Ordering,
};

use memchr::memmem;
use phoenix_ui_mobile::{
   Op,
   Prop,
   ViewKind,
   get_prop,
   prop,
};
use phoenix_wire::Value;

use crate::{
   js::page::ui_block,
   patches::{
      BACKGROUND,
      ContributionId,
      Draft,
      HIDDEN,
      Handler,
      PAGE,
      Patches,
      Place,
      SlotKind,
      aids,
      commands,
      number,
      patches,
      targets::{
         self,
         Target,
      },
   },
};

impl Patches {
   pub fn page_height(&mut self, pager: i32, contribution: ContributionId, height: f64) -> Vec<Op> {
      self.page_heights.insert((pager, contribution), height);
      if self.shown.get(&pager) == Some(&contribution) {
         vec![Op::Update {
            id:    pager,
            props: vec![(Prop::HEIGHT, Value::F64(height))],
         }]
      } else {
         Vec::new()
      }
   }

   /// The node of text box `index` on `contribution`'s page at `place`.
   pub fn input_node(
      &self,
      contribution: ContributionId,
      place: &Place,
      index: u32,
   ) -> Option<i32> {
      self
         .slots
         .iter()
         .filter(|slot| slot.kind == SlotKind::Target(Target::TabPage) && slot.place == *place)
         .flat_map(|slot| slot.all.iter())
         .copied()
         .find(|node| {
            matches!(self.handlers.get(node), Some(Handler::Input { contribution: owner, index: at })
               if *owner == contribution && *at == index)
         })
   }

   /// Puts back what was typed into the text boxes of a page drawn anew.
   pub fn restore_typed(&self, draft: &mut Draft, place: &Place) {
      if let Some(Handler::Input {
         contribution,
         index,
      }) = &draft.press
         && let Some(text) = self.typed.get(&(*contribution, place.clone(), *index))
      {
         draft.set(Prop::VALUE, Some(Value::Str(text.clone())));
      }
      for kid in &mut draft.kids {
         self.restore_typed(kid, place);
      }
   }

   /// Whether a page event on `node` is about a page past Rust's.
   pub fn beyond_rust(&self, node: i32, payload: &Value) -> bool {
      if !self
         .anchors
         .get(&Target::Tab)
         .is_some_and(|pagers| pagers.contains(&node))
      {
         return false;
      }
      let Some(row) = self.mirror.kids(node).first() else {
         return false;
      };
      let last = self.mirror.kids(*row).len().saturating_sub(1);
      #[expect(clippy::cast_precision_loss, reason = "a handful of pages")]
      let last = last as f64;
      let page = payload
         .field(PAGE.key())
         .and_then(Value::as_i32)
         .map(f64::from);
      let offset = payload.field(prop!("offset").key()).and_then(number);
      page.or(offset).is_some_and(|at| at > last + 0.001)
   }

   /// The pager and index of Rust's profile tab `node`.
   pub fn rust_tab(&self, node: i32) -> Option<(i32, i32)> {
      let row = self.mirror.parent(node)?;
      let slot = self
         .slots
         .iter()
         .find(|slot| slot.parent == row && slot.kind == SlotKind::Target(Target::Tab))?;
      let index = self.mirror.kids(row).iter().position(|tab| *tab == node)?;
      Some((slot.anchor, i32::try_from(index).ok()?))
   }

   /// Opens `contribution`'s page on `pager`, after Rust's pages.
   pub fn select_tab(&mut self, pager: i32, contribution: ContributionId) -> Vec<Op> {
      let Some(at) = self
         .contributions
         .iter()
         .filter(|known| known.target == Target::Tab)
         .position(|known| known.id == contribution)
      else {
         return Vec::new();
      };
      let Some(&row) = self.mirror.kids(pager).first() else {
         return Vec::new();
      };
      let page = self.mirror.kids(row).len() + at;
      let mut out = vec![page_op(pager, page)];
      if let Some(height) = self.page_heights.get(&(pager, contribution)) {
         out.push(Op::Update {
            id:    pager,
            props: vec![(Prop::HEIGHT, Value::F64(*height))],
         });
      }
      if let Some((tabs, indicator)) = targets::tab_row(&self.mirror, pager) {
         out.push(Op::Update {
            id:    indicator,
            props: vec![(HIDDEN, Value::Bool(true))],
         });
         if let Some(strip) = self.mirror.parent(tabs) {
            let index = self.mirror.kids(tabs).len() - 1 + at;
            out.push(Op::ScrollTo {
               id:    strip,
               value: Value::Obj(vec![
                  (
                     Prop::INDEX.key(),
                     Value::I32(i32::try_from(index).unwrap_or_default()),
                  ),
                  (prop!("align").key(), Value::Str("reveal".to_owned())),
                  (prop!("offset").key(), Value::F64(16.0)),
                  (prop!("animated").key(), Value::Bool(true)),
               ]),
            });
         }
      }
      self.shown.insert(pager, contribution);
      self.static_tabs.insert(pager);
      out.extend(self.rust_label_colors(pager, None));
      out.extend(self.tab_looks(pager));
      out
   }

   /// Closes the plugin page open on `pager`, opening Rust's page `index`
   /// unless Rust does that itself.
   pub fn leave_tab(&mut self, pager: i32, index: Option<i32>) -> Vec<Op> {
      if self.shown.remove(&pager).is_none() {
         return Vec::new();
      }
      let mut out = Vec::new();
      if let Some(index) = index {
         out.push(page_op(pager, usize::try_from(index).unwrap_or_default()));
         out.extend(self.rust_label_colors(pager, usize::try_from(index).ok()));
      }
      if let Some(height) = self.rust_heights.get(&pager) {
         out.push(Op::Update {
            id:    pager,
            props: vec![(Prop::HEIGHT, height.clone())],
         });
      }
      if let Some((_, indicator)) = targets::tab_row(&self.mirror, pager) {
         out.push(Op::Update {
            id:    indicator,
            props: vec![(HIDDEN, Value::Null)],
         });
      }
      out.extend(self.tab_looks(pager));
      out
   }

   /// Colors Phoenix's own tab labels on `pager`, `selected` lit, with their
   /// animation off.
   pub fn rust_label_colors(&self, pager: i32, selected: Option<usize>) -> Vec<Op> {
      let (on, off) = self.templates.tab_colors();
      let Some((row, _)) = targets::tab_row(&self.mirror, pager) else {
         return Vec::new();
      };
      let tabs = self.mirror.kids(row);
      tabs[..tabs.len().saturating_sub(1)]
         .iter()
         .enumerate()
         .filter_map(|(index, tab)| {
            let inner = *self.mirror.kids(*tab).first()?;
            let text = *self.mirror.kids(inner).first()?;
            let color = if Some(index) == selected { on } else { off };
            Some(Op::Update {
               id:    text,
               props: vec![
                  (prop!("worklet"), Value::Null),
                  (Prop::COLOR, Value::I64(color)),
               ],
            })
         })
         .collect()
   }

   /// Colors the plugin tabs on `pager` and shows the open one's bar.
   pub fn tab_looks(&self, pager: i32) -> Vec<Op> {
      let (on, off) = self.templates.tab_colors();
      let shown = self.shown.get(&pager);
      let tabs = self
         .contributions
         .iter()
         .filter(|known| known.target == Target::Tab)
         .map(|known| known.id);
      let roots = self
         .slots
         .iter()
         .filter(|slot| slot.anchor == pager && slot.kind == SlotKind::Target(Target::Tab))
         .flat_map(|slot| slot.roots.iter().copied());
      let mut out = Vec::new();
      for (contribution, root) in tabs.zip(roots) {
         let Some((text, bar)) = self.tab_parts.get(&root) else {
            continue;
         };
         let open = shown == Some(&contribution);
         out.push(Op::Update {
            id:    *text,
            props: vec![(Prop::COLOR, Value::I64(if open { on } else { off }))],
         });
         out.push(Op::Update {
            id:    *bar,
            props: vec![(BACKGROUND, Value::I64(if open { on } else { 0 }))],
         });
         out.push(Op::Update {
            id:    root,
            props: vec![(prop!("accessibilitySelected"), Value::Bool(open))],
         });
      }
      out
   }
}

/// Turns `pager` to `page`. Rust counts its own `seq` up from 1, and the host
/// takes any new one.
fn page_op(pager: i32, page: usize) -> Op {
   static SEQUENCE: AtomicI32 = AtomicI32::new(1 << 30);
   Op::Update {
      id:    pager,
      props: vec![(
         PAGE,
         Value::Obj(vec![
            (
               prop!("seq").key(),
               Value::I32(SEQUENCE.fetch_add(1, Ordering::Relaxed)),
            ),
            (
               Prop::INDEX.key(),
               Value::I32(i32::try_from(page).unwrap_or_default()),
            ),
            (prop!("animated").key(), Value::Bool(true)),
         ]),
      )],
   }
}

/// The user a profile batch is about: the id in the avatar image under the
/// node `Avatar`. A default avatar has none, and then it is the profile
/// Phoenix just fetched. Other avatars in the batch (mutual friends) are
/// never taken for it.
pub fn profile_user(ops: &[Op]) -> Option<String> {
   let avatar = ops.iter().find_map(|op| {
      match op {
         Op::Create { id, props, .. }
            if matches!(get_prop(props, Prop::ACCESSIBILITY_ID), Some(Value::Str(aid)) if aid == aids::AVATAR) =>
         {
            Some(*id)
         },
         _ => None,
      }
   });
   let image = avatar.and_then(|avatar| {
      ops.iter().find_map(|op| {
         match op {
            Op::Create { id, kind, props } if *id > avatar && *kind == ViewKind::Image => {
               match get_prop(props, Prop::URI) {
                  Some(Value::Str(uri)) => Some(uri.clone()),
                  _ => None,
               }
            },
            _ => None,
         }
      })
   });
   image
      .as_deref()
      .and_then(avatar_user)
      .or_else(crate::discord::last_profile)
}

/// The user id in an avatar URL, `…/avatars/<id>/<hash>` or
/// `…/users/<id>/avatars/<hash>` for a server avatar.
fn avatar_user(uri: &str) -> Option<String> {
   let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
   if let Some(at) = memmem::find(uri.as_bytes(), b"/users/") {
      let id = uri[at + "/users/".len()..].split('/').next()?;
      return digits(id).then(|| id.to_owned());
   }
   let at = memmem::find(uri.as_bytes(), b"/avatars/")?;
   let id = uri[at + "/avatars/".len()..].split('/').next()?;
   digits(id).then(|| id.to_owned())
}

pub fn send(contribution: ContributionId, place: &Place, index: u32) -> Vec<Op> {
   let mut state = patches();
   let input = state.input_node(contribution, place, index);
   let text = state.typed.remove(&(contribution, place.clone(), index));
   drop(state);
   if let Some(engine) = phoenix_runtime::engine() {
      ui_block(
         &engine,
         contribution,
         place,
         index,
         Some(text.unwrap_or_default()),
      );
   }
   input.map_or_default(|input| vec![commands::force_text(input, "")])
}

pub fn block(
   contribution: ContributionId,
   place: &Place,
   index: u32,
   text: Option<String>,
   input: Option<i32>,
) -> Vec<Op> {
   if let Some(engine) = phoenix_runtime::engine() {
      ui_block(&engine, contribution, place, index, text);
   }
   input.map_or_default(|input| vec![commands::force_text(input, "")])
}
