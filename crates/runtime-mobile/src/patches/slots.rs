//! Slots: plugin nodes kept under Phoenix's parents, drawn and redrawn.

use std::collections::HashSet;

use phoenix_ui_mobile::{
   Op,
   Prop,
   ViewKind,
   prop,
};
use phoenix_wire::Value;

use crate::patches::{
   Before,
   ContributionId,
   Draft,
   Handler,
   Outcome,
   Patches,
   Place,
   RESULTS_CAP,
   Slot,
   SlotKind,
   targets::Target,
   worker::{
      Job,
      queue,
   },
};

impl Patches {
   /// Forgets the slots and anchors under `deleted` nodes and returns the
   /// plugin nodes that go with them.
   pub fn drop_deleted(&mut self, deleted: &[i32]) -> Vec<i32> {
      let mut orphans = Vec::new();
      for id in deleted {
         let (gone, kept) = std::mem::take(&mut self.slots)
            .into_iter()
            .partition::<Vec<_>, _>(|slot| slot.parent == *id);
         self.slots = kept;
         self.grown.remove(id);
         for slot in gone {
            if let SlotKind::Menu(menu) = slot.kind
               && let Some(answer) = self.menus.remove(&menu)
            {
               let _ = answer.send(-1);
            }
            self.forget_slot(&slot);
            orphans.extend(slot.all);
         }
         for anchors in self.anchors.values_mut() {
            anchors.remove(id);
         }
         self.shown.remove(id);
         self.static_tabs.remove(id);
         self.rust_heights.remove(id);
         self.page_heights.retain(|(pager, _), _| pager != id);
         self.tab_parts.remove(id);
         self.patched_lists.remove(id);
         self.hidden_nodes.remove(id);
         if self.suggestions.scroll == Some(*id) {
            orphans.extend(self.drop_suggestions());
         }
      }
      orphans
   }

   /// Opens `target`'s slot for the anchor `id` and draws into it. Returns
   /// the parent when it opened a new slot.
   pub fn place(&mut self, target: Target, id: i32, out: &mut Vec<Op>) -> Option<i32> {
      if !self
         .contributions
         .iter()
         .any(|contribution| contribution.target == target.drawn_by())
      {
         return None;
      }
      let aid = self.mirror.accessibility_id(id)?.to_owned();
      let placement = target.place(&self.mirror, id, &aid, &self.seen)?;
      let before = placement.before.map_or(Before::End, Before::Node);
      if let Some(slot) = self
         .slots
         .iter_mut()
         .find(|slot| slot.parent == placement.parent && slot.kind == SlotKind::Target(target))
      {
         slot.before = before;
         return None;
      }
      if target == Target::Member
         && let Some(name) = Target::stretched_name(&self.mirror, id, &aid)
      {
         // The name stops growing and the slot grows in its place, so the
         // decorations follow the name and the time stays at the end.
         self.grown.insert(placement.parent);
         self.late.push(Op::Update {
            id:    name,
            props: vec![
               (prop!("flex"), Value::F64(0.0)),
               (prop!("shrink"), Value::Bool(true)),
            ],
         });
      }
      self.slots.push(Slot {
         kind: SlotKind::Target(target),
         anchor: id,
         parent: placement.parent,
         before,
         place: placement.place,
         roots: Vec::new(),
         all: Vec::new(),
      });
      let index = self.slots.len() - 1;
      self.fill(index, out);
      Some(placement.parent)
   }

   /// Draws slot `index` anew into `out`. The caller deletes what it held.
   fn fill(&mut self, index: usize, out: &mut Vec<Op>) {
      let SlotKind::Target(target) = self.slots[index].kind else {
         return;
      };
      let place = self.slots[index].place.clone();
      let contributions = self
         .contributions
         .iter()
         .filter(|contribution| contribution.target == target.drawn_by())
         .collect::<Vec<_>>();
      for contribution in &contributions {
         let key = (contribution.id, place.clone());
         if (target.evaluates() || contribution.conditional)
            && !self.results.contains_key(&key)
            && self.pending.insert(key)
         {
            queue(Job::Evaluate(contribution.id, place.clone()));
         }
      }
      let ids = contributions
         .iter()
         .map(|contribution| contribution.id)
         .collect::<Vec<_>>();
      let drafts = self
         .templates
         .draw(target, &contributions, &place, &|contribution| {
            self.results.get(&(contribution.id, place.clone())).cloned()
         });
      let drafts = if self.grown.contains(&self.slots[index].parent) {
         vec![Draft {
            kind:  ViewKind::View,
            props: vec![
               (prop!("direction"), Value::Str("row".to_owned())),
               (prop!("flex"), Value::F64(1.0)),
               (prop!("minWidth"), Value::F64(0.0)),
               (prop!("gap"), Value::F64(4.0)),
               (prop!("align"), Value::Str("center".to_owned())),
               (Prop::PADDING, Value::Rect([4.0, 0.0, 0.0, 0.0])),
            ],
            kids:  drafts,
            press: None,
            src:   None,
         }]
      } else {
         drafts
      };
      let mut roots = Vec::new();
      let mut all = Vec::new();
      for mut draft in drafts {
         if target == Target::TabPage {
            self.restore_typed(&mut draft, &place);
         }
         let root = self.emit(draft, None, out, &mut all);
         if target == Target::Tab
            && let [.., text, _, bar, _] = all[..]
         {
            // Kids come first: text, inner, bar, then the tab.
            self.tab_parts.insert(root, (text, bar));
         }
         roots.push(root);
      }
      self.slots[index].roots = roots;
      self.slots[index].all = all;
      if target == Target::Tab {
         out.extend(self.tab_looks(self.slots[index].anchor));
      }
      // The content's tint, once no answer for it is outstanding.
      if target == Target::Content
         && !ids
            .iter()
            .any(|id| self.pending.contains(&(*id, place.clone())))
      {
         let tint = ids.iter().find_map(|id| {
            match self.results.get(&(*id, place.clone())) {
               Some(Outcome::Text(Some(rendered))) => rendered.tint,
               _ => None,
            }
         });
         if let Place::Message(message) = place {
            self.set_tint(&message, tint, out);
         }
      }
   }

   pub fn emit(
      &mut self,
      draft: Draft,
      inherited: Option<&Handler>,
      out: &mut Vec<Op>,
      all: &mut Vec<i32>,
   ) -> i32 {
      let handler = draft.press.or_else(|| inherited.cloned());
      let ids = draft
         .kids
         .into_iter()
         .map(|kid| self.emit(kid, handler.as_ref(), out, all))
         .collect::<Vec<_>>();
      let id = self.next_id;
      self.next_id += 1;
      if let Some(handler) = handler {
         self.handlers.insert(id, handler);
      }
      out.push(Op::Create {
         id,
         kind: draft.kind,
         props: draft.props,
      });
      if !ids.is_empty() {
         out.push(Op::Children { id, ids });
      }
      all.push(id);
      id
   }

   /// `parent`'s children as Rust has them, with the slots spliced in.
   pub fn merged(&self, parent: i32) -> Vec<i32> {
      let mut list = self.mirror.kids(parent).to_vec();
      for slot in self.slots.iter().filter(|slot| slot.parent == parent) {
         let at = match slot.before {
            Before::Start => 0,
            Before::Node(node) => list.iter().position(|id| *id == node).unwrap_or(list.len()),
            Before::End => list.len(),
         };
         list.splice(at..at, slot.roots.iter().copied());
      }
      list
   }

   pub fn forget_slot(&mut self, slot: &Slot) {
      self.forget_ids(&slot.all);
   }

   pub fn forget_ids(&mut self, ids: &[i32]) {
      for id in ids {
         self.handlers.remove(id);
         self.tab_parts.remove(id);
      }
   }

   pub fn redraw(&mut self, target: Target) -> Vec<Op> {
      Target::ALL
         .into_iter()
         .filter(|drawn| drawn.drawn_by() == target)
         .flat_map(|drawn| self.refresh(drawn))
         .collect()
   }

   /// Draws every slot of `target` anew, and opens slots at anchors that had
   /// none.
   pub fn refresh(&mut self, target: Target) -> Vec<Op> {
      let mut out = Vec::new();
      let mut deletes = Vec::new();
      let mut touched = HashSet::new();
      for index in 0..self.slots.len() {
         if self.slots[index].kind != SlotKind::Target(target) {
            continue;
         }
         let old = std::mem::take(&mut self.slots[index].all);
         self.forget_ids(&old);
         deletes.extend(old);
         self.fill(index, &mut out);
         touched.insert(self.slots[index].parent);
      }
      let anchors = self
         .anchors
         .get(&target)
         .map_or_else(Vec::new, |anchors| anchors.iter().copied().collect());
      for anchor in anchors {
         if let Some(parent) = self.place(target, anchor, &mut out) {
            touched.insert(parent);
         }
      }
      let mut ops = self.finish(out, touched, deletes);
      ops.append(&mut self.late);
      ops
   }

   /// Records a plugin's answer and redraws the slots it was for.
   pub fn evaluated(
      &mut self,
      contribution: ContributionId,
      place: Place,
      outcome: Outcome,
   ) -> Vec<Op> {
      let key = (contribution, place);
      self.pending.remove(&key);
      let Some(target) = self
         .contributions
         .iter()
         .find(|known| known.id == contribution)
         .map(|known| known.target)
      else {
         return Vec::new();
      };
      if self.results.len() >= RESULTS_CAP {
         self.results.clear();
      }
      let shows =
         |outcome: &Outcome| !matches!(outcome, Outcome::Text(None) | Outcome::Shown(false));
      let visible = shows(&outcome);
      let place = key.1.clone();
      let was = self.results.insert(key, outcome);
      if !visible && !was.as_ref().is_some_and(shows) {
         return Vec::new();
      }
      let mut out = Vec::new();
      let mut deletes = Vec::new();
      let mut touched = HashSet::new();
      for index in 0..self.slots.len() {
         let slot = &self.slots[index];
         if !matches!(slot.kind, SlotKind::Target(drawn) if drawn.drawn_by() == target)
            || slot.place != place
         {
            continue;
         }
         let old = std::mem::take(&mut self.slots[index].all);
         self.forget_ids(&old);
         deletes.extend(old);
         self.fill(index, &mut out);
         touched.insert(self.slots[index].parent);
      }
      self.finish(out, touched, deletes)
   }

   fn finish(&self, mut out: Vec<Op>, touched: HashSet<i32>, deletes: Vec<i32>) -> Vec<Op> {
      for parent in touched {
         if self.mirror.has_kids(parent) {
            out.push(Op::Children {
               id:  parent,
               ids: self.merged(parent),
            });
         }
      }
      if !deletes.is_empty() {
         out.push(Op::Delete { ids: deletes });
      }
      out
   }
}
