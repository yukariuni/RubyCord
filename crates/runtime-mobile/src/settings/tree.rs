//! The node tree pages are built as, and the motion slots its animations use.

use std::collections::HashMap;

use phoenix_ui_mobile::{
   Node,
   Prop,
   Props,
   ViewKind,
   motion::Worklet,
   set_prop,
};
use phoenix_wire::Value;

use crate::settings::{
   Press,
   Slot,
};

pub struct Tree {
   pub id:      i32,
   pub kind:    ViewKind,
   pub props:   Props,
   pub kids:    Vec<Self>,
   pub src:     Option<u32>,
   pub press:   Option<Press>,
   pub slot:    Option<Slot>,
   pub animate: Option<Animate>,
}

/// A node the host animates between two prop sets, like a switch's knob.
pub struct Animate {
   pub worklet: &'static Worklet,
   /// The props of the other state.
   pub other:   Props,
   /// Set when the node starts at `other` and springs to its own props.
   pub flipped: bool,
}

impl Tree {
   pub fn from_template(node: &Node) -> Self {
      Self {
         id:      0,
         kind:    node.kind.clone(),
         props:   node.props.clone(),
         kids:    node.kids.iter().map(Self::from_template).collect(),
         src:     node.src,
         press:   None,
         slot:    None,
         animate: None,
      }
   }

   pub fn set(&mut self, key: Prop, value: Option<Value>) {
      set_prop(&mut self.props, key, value);
   }

   pub fn find(&mut self, src: u32) -> Option<&mut Self> {
      if self.src == Some(src) {
         return Some(self);
      }
      self.kids.iter_mut().find_map(|kid| kid.find(src))
   }

   pub fn slots(&self) -> Vec<(i32, Slot)> {
      let mut slots = self
         .slot
         .map(|slot| (self.id, slot))
         .into_iter()
         .collect::<Vec<_>>();
      slots.extend(self.kids.iter().flat_map(Self::slots));
      slots
   }

   /// Marks the animated nodes under the one `press` targets as just flipped.
   pub fn flip(&mut self, press: Press, inside: bool) {
      let inside = inside || (self.press == Some(press) && self.animate.is_some());
      if inside && let Some(animate) = &mut self.animate {
         animate.flipped = true;
      }
      for kid in &mut self.kids {
         kid.flip(press, inside);
      }
   }

   pub fn ids(&self) -> Vec<i32> {
      let mut ids = vec![self.id];
      ids.extend(self.kids.iter().flat_map(Self::ids));
      ids
   }
}

/// Whether `new` can update `old` in place: the same nodes, handlers and
/// animations, and no prop dropped (an update can only set props).
pub fn same_shape(old: &Tree, new: &Tree) -> bool {
   old.kind == new.kind
      && old.press == new.press
      && old.slot == new.slot
      && match (&old.animate, &new.animate) {
         (None, None) => {
            old.props
               .iter()
               .all(|(key, _)| new.props.iter().any(|(k, _)| k == key))
         },
         (Some(a), Some(b)) => std::ptr::eq(a.worklet, b.worklet),
         _ => false,
      }
      && old.kids.len() == new.kids.len()
      && old
         .kids
         .iter()
         .zip(&new.kids)
         .all(|(a, b)| same_shape(a, b))
}

pub fn text(value: &str) -> Value {
   Value::Str(value.to_owned())
}

/// A colored piece of a text's `spans`.
pub fn span(piece: &str, color: i64) -> Value {
   Value::Obj(vec![
      (Prop::TEXT.key(), text(piece)),
      (Prop::COLOR.key(), Value::I64(color)),
   ])
}

/// Motion slots for animated nodes, reused once a node is deleted.
pub struct SlotPool {
   pub next:  i32,
   pub free:  Vec<(i32, i32)>,
   pub owned: HashMap<i32, (i32, i32)>,
}

impl SlotPool {
   /// The first of `count` slots for `node`, and whether they were used
   /// before.
   pub fn take(&mut self, node: i32, count: i32) -> (i32, bool) {
      let (first, reused) = if let Some(at) = self.free.iter().position(|&(_, size)| size == count)
      {
         (self.free.swap_remove(at).0, true)
      } else {
         self.next -= count;
         (self.next, false)
      };
      self.owned.insert(node, (first, count));
      (first, reused)
   }

   pub fn release(&mut self, node: i32) {
      if let Some(block) = self.owned.remove(&node) {
         self.free.push(block);
      }
   }
}
