use std::collections::{
   HashMap,
   HashSet,
};

use phoenix_wire::Value;

use crate::{
   ChildEdit,
   Node,
   Op,
   Prop,
   Props,
   ViewKind,
   get_prop,
   set_prop,
};

const FIRST: Prop = Prop::named("first");
const FORCE_VALUE: Prop = Prop::named("forceValue");

fn composer_text(props: &Props) -> Option<&str> {
   if let Some(Value::Obj(fields)) = get_prop(props, FORCE_VALUE)
      && let Some((_, Value::Str(text))) = fields.iter().find(|(key, _)| *key == Prop::TEXT.key())
   {
      return Some(text);
   }
   match get_prop(props, Prop::VALUE) {
      Some(Value::Str(text)) => Some(text),
      _ => None,
   }
}

/// Rust's view tree as its batches describe it: who holds whom, and the
/// accessibility id each node carries.
#[derive(Default)]
pub struct Mirror {
   root:         Option<i32>,
   kids:         HashMap<i32, Vec<i32>>,
   parent:       HashMap<i32, i32>,
   ids:          HashMap<i32, String>,
   /// Debug-only node kind & prop snapshots, populated only when `capture` is
   /// enabled (e.g. when the remote debug shell is active or in unit tests).
   kinds:        HashMap<i32, ViewKind>,
   props:        HashMap<i32, Props>,
   sheets:       HashSet<i32>,
   navigators:   Vec<i32>,
   stack_header: Option<Props>,
   /// The list index of each virtualized list's first mounted row.
   firsts:       HashMap<i32, i32>,
   /// Original `on` event list of each virtualized/row-gesture list.
   list_on:      HashMap<i32, Vec<Value>>,
   /// The message box inputs and their text, newest last.
   composer:     Vec<(i32, String)>,
}

/// What one batch created, by node.
pub type Created<'ops> = HashMap<i32, (&'ops ViewKind, &'ops Props)>;

impl Mirror {
   /// Applies `ops` and returns the nodes they created, plus the nodes they
   /// deleted. When `capture` is true, also keeps each node's `ViewKind` and
   /// full `Props` for live tree inspection.
   pub fn apply<'ops>(&mut self, ops: &'ops [Op], capture: bool) -> (Created<'ops>, Vec<i32>) {
      let mut created = HashMap::new();
      let mut deleted = Vec::new();
      for op in ops {
         match op {
            Op::Create { id, kind, props } => {
               created.insert(*id, (kind, props));
               self.create(*id, kind, props, capture);
            },
            Op::Children { id, ids } => {
               for kid in ids {
                  self.parent.insert(*kid, *id);
               }
               self.kids.insert(*id, ids.clone());
            },
            Op::ChildEdits { id, edits } => {
               let old = self.kids.remove(id).unwrap_or_default();
               let new = apply_edits(&old, edits);
               for kid in edits.iter().flat_map(|edit| &edit.ins) {
                  self.parent.insert(*kid, *id);
               }
               self.kids.insert(*id, new);
            },
            Op::Delete { ids } => {
               for id in ids {
                  self.delete(*id);
               }
               deleted.extend_from_slice(ids);
            },
            Op::Root { id } => {
               self.root = Some(*id);
            },
            Op::Update { id, props } => self.update(*id, props, capture),
            _ => {},
         }
      }
      (created, deleted)
   }

   fn create(&mut self, id: i32, kind: &ViewKind, props: &Props, capture: bool) {
      if capture {
         self.kinds.insert(id, kind.clone());
         self.props.insert(id, props.clone());
      }
      let is_list = get_prop(props, FIRST).is_some();
      if let Some(Value::I32(first)) = get_prop(props, FIRST) {
         self.firsts.insert(id, *first);
      }
      if let Some(Value::Arr(on)) = get_prop(props, Prop::ON)
         && (is_list
            || on
               .iter()
               .any(|value| matches!(value, Value::Str(name) if name == "rowLongPress")))
      {
         self.list_on.insert(id, on.clone());
      }
      if let Some(Value::Str(aid)) = get_prop(props, Prop::ACCESSIBILITY_ID) {
         self.name(id, aid);
      }
      if let Some(text) = composer_text(props) {
         self.set_composer_text(id, text.to_owned());
      }
      if *kind == ViewKind::Sheet {
         self.sheets.insert(id);
      }
      if *kind == ViewKind::Navigator && !self.navigators.contains(&id) {
         self.navigators.push(id);
      }
      if get_prop(props, Prop::STACK_HEADER).is_some() {
         self.stack_header = Some(props.clone());
      }
   }

   fn update(&mut self, id: i32, props: &Props, capture: bool) {
      if capture {
         let stored = self.props.entry(id).or_default();
         for (key, value) in props {
            set_prop(stored, *key, Some(value.clone()));
         }
      }
      if let Some(Value::I32(first)) = get_prop(props, FIRST) {
         self.firsts.insert(id, *first);
      }
      match get_prop(props, Prop::ACCESSIBILITY_ID) {
         Some(Value::Str(aid)) => self.name(id, aid),
         Some(Value::Null) => {
            self.ids.remove(&id);
         },
         _ => {},
      }
      if (self.firsts.contains_key(&id) || self.list_on.contains_key(&id))
         && let Some(Value::Arr(on)) = get_prop(props, Prop::ON)
      {
         self.list_on.insert(id, on.clone());
      }
      if let Some(text) = composer_text(props) {
         self.set_composer_text(id, text.to_owned());
      }
   }

   /// Records `aid` as the accessibility id of `id`.
   fn name(&mut self, id: i32, aid: &str) {
      if aid == "composer" && !self.composer.iter().any(|(input, _)| *input == id) {
         self.composer.push((id, String::new()));
      }
      self.ids.insert(id, aid.to_owned());
   }

   fn delete(&mut self, id: i32) {
      if self.root == Some(id) {
         self.root = None;
      }
      self.kids.remove(&id);
      self.parent.remove(&id);
      self.ids.remove(&id);
      self.kinds.remove(&id);
      self.props.remove(&id);
      self.sheets.remove(&id);
      self.navigators.retain(|nav| *nav != id);
      self.firsts.remove(&id);
      self.list_on.remove(&id);
      self.composer.retain(|(input, _)| *input != id);
   }

   /// The row at list index `index` of the virtualized list `id`.
   #[must_use]
   pub fn row(&self, id: i32, index: i32) -> Option<i32> {
      let offset = index - self.firsts.get(&id).copied().unwrap_or(0);
      self.kids(id).get(usize::try_from(offset).ok()?).copied()
   }

   #[must_use]
   pub fn is_row_list(&self, id: i32) -> bool {
      self.list_on.contains_key(&id) || self.firsts.contains_key(&id)
   }

   #[must_use]
   pub fn list_on(&self, id: i32) -> &[Value] {
      self.list_on.get(&id).map_or(&[], Vec::as_slice)
   }

   #[must_use]
   pub fn kids(&self, id: i32) -> &[i32] {
      self.kids.get(&id).map_or(&[], Vec::as_slice)
   }

   /// The root node set by `Op::Root`, or the top ancestor of a live navigator.
   #[must_use]
   pub fn root(&self) -> Option<i32> {
      if let Some(root) = self.root
         && self.has_kids(root)
      {
         return Some(root);
      }
      let mut cur = *self.navigators.first()?;
      for _ in 0..64 {
         let Some(parent) = self.parent(cur) else {
            break;
         };
         cur = parent;
      }
      self.has_kids(cur).then_some(cur)
   }

   #[must_use]
   pub fn has_kids(&self, id: i32) -> bool {
      self.kids.contains_key(&id)
   }

   #[must_use]
   pub fn parent(&self, id: i32) -> Option<i32> {
      self.parent.get(&id).copied()
   }

   #[must_use]
   pub fn accessibility_id(&self, id: i32) -> Option<&str> {
      self.ids.get(&id).map(String::as_str)
   }

   #[must_use]
   pub fn kind(&self, id: i32) -> Option<&ViewKind> {
      self.kinds.get(&id)
   }

   #[must_use]
   pub fn props(&self, id: i32) -> Option<&Props> {
      self.props.get(&id)
   }

   /// Every live node whose accessibility id equals `aid`.
   #[must_use]
   pub fn nodes_with_id(&self, aid: &str) -> Vec<i32> {
      self
         .ids
         .iter()
         .filter_map(|(id, existing)| (existing == aid).then_some(*id))
         .collect()
   }

   /// Every live node whose accessibility id passes `test`.
   #[must_use]
   pub fn nodes_where(&self, test: &impl Fn(&str) -> bool) -> Vec<i32> {
      self
         .ids
         .iter()
         .filter_map(|(id, existing)| test(existing).then_some(*id))
         .collect()
   }

   /// Walks up from `id` to its enclosing virtualized/row-gesture list, if any.
   #[must_use]
   pub fn enclosing_list(&self, mut id: i32) -> Option<i32> {
      for _ in 0..16 {
         let parent = self.parent(id)?;
         if self.is_row_list(parent) {
            return Some(parent);
         }
         id = parent;
      }
      None
   }

   /// Whether `id` is currently tracked in the mirror.
   #[must_use]
   pub fn exists(&self, id: i32) -> bool {
      self.parent.contains_key(&id)
         || self.kids.contains_key(&id)
         || self.ids.contains_key(&id)
         || self.sheets.contains(&id)
         || self.firsts.contains_key(&id)
   }

   /// The first node at or under `id`, depth first, whose accessibility id
   /// passes `test`.
   #[must_use]
   pub fn find_down(&self, id: i32, test: &impl Fn(&str) -> bool, depth: usize) -> Option<i32> {
      if self.accessibility_id(id).is_some_and(test) {
         return Some(id);
      }
      if depth == 0 {
         return None;
      }
      self
         .kids(id)
         .iter()
         .find_map(|kid| self.find_down(*kid, test, depth - 1))
   }

   /// The outermost live `Navigator` (not inside a sheet) that currently holds
   /// pages — Phoenix's root navigator above `main-tabs` and chat.
   #[must_use]
   pub fn active_navigator(&self) -> Option<i32> {
      self
         .navigators
         .iter()
         .copied()
         .filter(|id| !self.kids(*id).is_empty() && self.sheet_of(*id).is_none())
         .min_by_key(|id| {
            let mut depth = 0usize;
            let mut cur = self.parent(*id);
            while let Some(parent) = cur {
               depth += 1;
               cur = self.parent(parent);
               if depth > 64 {
                  break;
               }
            }
            (depth, *id)
         })
   }

   /// Header padding/height from Phoenix's own stack header, if seen.
   #[must_use]
   pub const fn stack_header(&self) -> Option<&Props> {
      self.stack_header.as_ref()
   }

   /// The sheet holding `id`, if any.
   #[must_use]
   pub fn sheet_of(&self, id: i32) -> Option<i32> {
      let mut node = self.parent(id);
      for _ in 0..40 {
         let current = node?;
         if self.sheets.contains(&current) {
            return Some(current);
         }
         node = self.parent(current);
      }
      None
   }

   #[must_use]
   pub fn composer(&self) -> Option<(i32, &str)> {
      self
         .composer
         .last()
         .map(|(input, text)| (*input, text.as_str()))
   }

   #[must_use]
   pub fn is_composer(&self, id: i32) -> bool {
      self.composer.iter().any(|(input, _)| *input == id)
   }

   pub fn set_composer_text(&mut self, id: i32, text: String) {
      if let Some((_, current)) = self.composer.iter_mut().find(|(input, _)| *input == id) {
         *current = text;
      }
   }

   /// `id` and what this batch created under it, as a template.
   #[must_use]
   pub fn snapshot(&self, id: i32, created: &Created<'_>) -> Option<Node> {
      let (kind, props) = created.get(&id)?;
      Some(Node {
         kind:  (*kind).clone(),
         props: (*props).clone(),
         kids:  self
            .kids(id)
            .iter()
            .filter_map(|kid| self.snapshot(*kid, created))
            .collect(),
         src:   None,
      })
   }
}

fn apply_edits(old: &[i32], edits: &[ChildEdit]) -> Vec<i32> {
   let mut out = Vec::with_capacity(old.len());
   let mut at = 0;
   for edit in edits {
      let keep = usize::try_from(edit.keep).unwrap_or_default();
      let del = usize::try_from(edit.del).unwrap_or_default();
      out.extend(old.iter().skip(at).take(keep));
      at = (at + keep + del).min(old.len());
      out.extend_from_slice(&edit.ins);
   }
   out.extend(old.iter().skip(at));
   out
}

#[cfg(test)]
mod tests {
   use super::*;
   use crate::{
      decode,
      encode,
      inspect::TreeJson,
      prop,
   };

   #[test]
   fn tracks_root_children_edits_and_deletes() {
      let mut mirror = Mirror::default();
      let batch = [
         Op::Root { id: 1 },
         Op::Create {
            id:    1,
            kind:  ViewKind::View,
            props: vec![],
         },
         Op::Create {
            id:    2,
            kind:  ViewKind::View,
            props: vec![(Prop::ACCESSIBILITY_ID, Value::Str("composer".to_owned()))],
         },
         Op::Create {
            id:    3,
            kind:  ViewKind::Text,
            props: vec![(Prop::TEXT, Value::Str("hello".to_owned()))],
         },
         Op::Children {
            id:  1,
            ids: vec![2, 3],
         },
      ];
      // Round-trip through binary UI wire codec before applying to Mirror.
      let decoded = decode(&encode(&batch)).expect("ui batch round-trip");
      mirror.apply(&decoded, true);
      assert_eq!(mirror.root(), Some(1));
      assert_eq!(mirror.kids(1), &[2, 3]);
      assert_eq!(mirror.parent(2), Some(1));
      assert_eq!(mirror.accessibility_id(2), Some("composer"));
      assert_eq!(mirror.composer(), Some((2, "")));
      mirror.apply(
         &[Op::Update {
            id:    2,
            props: vec![(
               FORCE_VALUE,
               Value::Obj(vec![
                  (Prop::named("seq").key(), Value::I64(1)),
                  (Prop::TEXT.key(), Value::Str(":artiwail: ".to_owned())),
               ]),
            )],
         }],
         false,
      );
      assert_eq!(mirror.composer(), Some((2, ":artiwail: ")));

      mirror.apply(
         &[
            Op::ChildEdits {
               id:    1,
               edits: vec![ChildEdit {
                  keep: 1,
                  del:  1,
                  ins:  vec![4],
               }],
            },
            Op::Update {
               id:    2,
               props: vec![(Prop::VALUE, Value::Str("draft".to_owned()))],
            },
         ],
         true,
      );
      assert_eq!(mirror.kids(1), &[2, 4]);
      assert_eq!(mirror.composer(), Some((2, "draft")));

      let dump = serde_json::to_value(TreeJson::new(&mirror, 1, 3)).expect("tree serializes");
      assert_eq!(dump["id"], 1);
      assert_eq!(dump["kind"], "view");
      assert_eq!(dump["children"][0]["aid"], "composer");

      mirror.apply(&[Op::Delete { ids: vec![2] }], true);
      assert!(!mirror.exists(2));
      assert_eq!(mirror.composer(), None);
   }

   #[test]
   fn finds_sheets_lists_and_navigators() {
      let mut mirror = Mirror::default();
      mirror.apply(
         &[
            Op::Root { id: 1 },
            Op::Create {
               id:    1,
               kind:  ViewKind::View,
               props: vec![],
            },
            Op::Create {
               id:    10,
               kind:  ViewKind::Navigator,
               props: vec![],
            },
            Op::Create {
               id:    20,
               kind:  ViewKind::Sheet,
               props: vec![],
            },
            Op::Create {
               id:    21,
               kind:  ViewKind::View,
               props: vec![(
                  Prop::ACCESSIBILITY_ID,
                  Value::Str("message.actions.root".to_owned()),
               )],
            },
            Op::Create {
               id:    30,
               kind:  ViewKind::List,
               props: vec![
                  (prop!("first"), Value::I32(5)),
                  (
                     Prop::ON,
                     Value::Arr(vec![Value::Str("rowLongPress".to_owned())]),
                  ),
               ],
            },
            Op::Create {
               id:    31,
               kind:  ViewKind::View,
               props: vec![],
            },
            Op::Children {
               id:  1,
               ids: vec![10, 20, 30],
            },
            Op::Children {
               id:  10,
               ids: vec![30],
            },
            Op::Children {
               id:  20,
               ids: vec![21],
            },
            Op::Children {
               id:  30,
               ids: vec![31],
            },
         ],
         true,
      );
      assert_eq!(mirror.active_navigator(), Some(10));
      assert_eq!(mirror.sheet_of(21), Some(20));
      assert_eq!(mirror.enclosing_list(31), Some(30));
      assert_eq!(mirror.row(30, 5), Some(31));
      assert_eq!(
         mirror.find_down(1, &|aid| aid == "message.actions.root", 4),
         Some(21)
      );
   }
}
