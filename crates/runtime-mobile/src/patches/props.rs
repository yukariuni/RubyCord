//! Declarative prop patches from `phoenix.ui.patchProps`.

use std::{
   collections::{
      HashMap,
      HashSet,
   },
   sync::atomic::{
      AtomicU64,
      Ordering,
   },
   time::{
      Duration,
      Instant,
   },
};

use phoenix_ui_mobile::{
   Op,
   Prop,
   Props,
   get_prop,
   mirror::Created,
   set_prop,
};
use phoenix_wire::Value;

use crate::patches::{
   ACTIVE,
   HIDDEN,
   Patches,
   PropPatch,
   aids,
   nodes::NodeMatch,
   patches,
   worker::{
      Job,
      queue,
   },
};

/// Which node a declarative [`PropPatch`] targets when a matching node mounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchTarget {
   /// The matching node itself.
   Node,
   /// The enclosing sheet (and its created outer wrapper), for zero-flash
   /// suppression.
   Sheet,
   /// The enclosing virtualized/row-gesture list.
   List,
}

/// Registers a declarative Rust-side prop patch (`phoenix.ui.patchProps`).
pub fn add_prop_patch(
   plugin: String,
   matcher: NodeMatch,
   target: PatchTarget,
   props: Props,
   add_on: Vec<String>,
   once: bool,
   timeout: Option<Duration>,
) -> u64 {
   static NEXT: AtomicU64 = AtomicU64::new(1);
   let id = NEXT.fetch_add(1, Ordering::Relaxed);
   ACTIVE.store(true, Ordering::Release);
   let is_list = target == PatchTarget::List;
   patches().prop_rules.push(PropPatch {
      id,
      plugin,
      matcher,
      target,
      props,
      add_on,
      once,
      deadline: timeout.map(|dur| Instant::now() + dur),
   });
   if is_list {
      queue(Job::RefreshPropPatches);
   }
   id
}

pub fn remove_prop_patch(id: u64) {
   let mut state = patches();
   let was_list = state
      .prop_rules
      .iter()
      .any(|patch| patch.id == id && patch.target == PatchTarget::List);
   state.prop_rules.retain(|patch| patch.id != id);
   drop(state);
   if was_list {
      queue(Job::RefreshPropPatches);
   }
}

impl Patches {
   pub fn match_prop_patches(
      &mut self,
      named: &[i32],
      created: &Created<'_>,
      patch_ops: &mut HashMap<i32, (Props, Vec<String>)>,
      list_updates: &mut HashSet<i32>,
   ) -> bool {
      let mut skip_action_placement = false;
      // Expire timed prop patches before matching.
      if !self.prop_rules.is_empty() {
         let now = Instant::now();
         self
            .prop_rules
            .retain(|patch| patch.deadline.is_none_or(|deadline| now <= deadline));
      }
      // Apply declarative Rust-side prop patches synchronously.
      if !self.prop_rules.is_empty() {
         let mut consumed_once = Vec::new();
         for id in named {
            let Some(aid) = self.mirror.accessibility_id(*id) else {
               continue;
            };
            for patch in &self.prop_rules {
               if !patch.matcher.matches(aid) {
                  continue;
               }
               match patch.target {
                  PatchTarget::Node => {
                     let entry = patch_ops.entry(*id).or_default();
                     entry.0.extend(patch.props.iter().cloned());
                     entry.1.extend(patch.add_on.iter().cloned());
                     if patch.once {
                        consumed_once.push(patch.id);
                     }
                  },
                  PatchTarget::Sheet => {
                     if let Some(sheet) = self.mirror.sheet_of(*id) {
                        let outer = self
                           .mirror
                           .parent(sheet)
                           .filter(|parent| created.contains_key(parent));
                        for target_id in std::iter::once(sheet).chain(outer) {
                           let entry = patch_ops.entry(target_id).or_default();
                           entry.0.extend(patch.props.iter().cloned());
                           entry.1.extend(patch.add_on.iter().cloned());
                        }
                        if aid == aids::MESSAGE_ACTIONS
                           && patch
                              .props
                              .iter()
                              .any(|(k, v)| *k == HIDDEN && *v == Value::Bool(true))
                        {
                           skip_action_placement = true;
                        }
                        if patch.once {
                           consumed_once.push(patch.id);
                        }
                     }
                  },
                  PatchTarget::List => {
                     if let Some(list_id) = self.mirror.enclosing_list(*id) {
                        let is_new = self
                           .patched_lists
                           .entry(list_id)
                           .or_default()
                           .insert(patch.id);
                        if is_new {
                           list_updates.insert(list_id);
                        }
                        if patch.once {
                           consumed_once.push(patch.id);
                        }
                     }
                  },
               }
            }
         }
         if !consumed_once.is_empty() {
            self
               .prop_rules
               .retain(|patch| !consumed_once.contains(&patch.id));
         }
      }
      skip_action_placement
   }

   pub fn list_patch_for(&self, list_id: i32) -> (Props, Vec<String>) {
      let Some(patch_ids) = self.patched_lists.get(&list_id) else {
         return (Vec::new(), Vec::new());
      };
      let mut props = Vec::new();
      let mut add_on = Vec::new();
      for patch in &self.prop_rules {
         if patch.target == PatchTarget::List && patch_ids.contains(&patch.id) {
            props.extend(patch.props.iter().cloned());
            add_on.extend(patch.add_on.iter().cloned());
         }
      }
      (props, add_on)
   }

   pub fn list_update_op(&self, list_id: i32) -> Option<Op> {
      if !self.mirror.is_row_list(list_id) {
         return None;
      }
      let (extra_props, add_on) = self.list_patch_for(list_id);
      let mut props = Vec::new();
      apply_prop_patch(
         &mut props,
         &extra_props,
         &add_on,
         Some(self.mirror.list_on(list_id)),
      );
      if props.is_empty() {
         return None;
      }
      Some(Op::Update { id: list_id, props })
   }

   /// Recomputes `PatchTarget::List` patches across live lists (discovering
   /// already-mounted lists and restoring original `on` arrays when removed).
   pub fn refresh_list_patches(&mut self) -> Vec<Op> {
      let mut affected: HashSet<i32> = self.patched_lists.keys().copied().collect();
      let list_patches = self
         .prop_rules
         .iter()
         .filter(|patch| patch.target == PatchTarget::List)
         .map(|patch| (patch.id, patch.matcher.clone()))
         .collect::<Vec<_>>();
      for (patch_id, matcher) in &list_patches {
         for node in self.mirror.nodes_where(&|aid| matcher.matches(aid)) {
            if let Some(list_id) = self.mirror.enclosing_list(node) {
               self
                  .patched_lists
                  .entry(list_id)
                  .or_default()
                  .insert(*patch_id);
               affected.insert(list_id);
            }
         }
      }
      let active_ids: HashSet<u64> = list_patches.iter().map(|(id, _)| *id).collect();
      for ids in self.patched_lists.values_mut() {
         ids.retain(|id| active_ids.contains(id));
      }
      let mut out = Vec::new();
      for list_id in affected {
         if !self.mirror.is_row_list(list_id) {
            continue;
         }
         let (extra_props, add_on) = self.list_patch_for(list_id);
         let mut props = Vec::new();
         // Always emit Prop::ON so removed `addOn` entries restore Rust's
         // original list.
         let mut on = self.mirror.list_on(list_id).to_vec();
         for event in &add_on {
            if !on.iter().any(|v| matches!(v, Value::Str(s) if s == event)) {
               on.push(Value::Str(event.clone()));
            }
         }
         props.push((Prop::ON, Value::Arr(on)));
         for (key, value) in extra_props {
            set_prop(&mut props, key, Some(value));
         }
         out.push(Op::Update { id: list_id, props });
      }
      self.patched_lists.retain(|_, ids| !ids.is_empty());
      out
   }
}

pub fn apply_prop_patch(
   props: &mut Props,
   extra: &Props,
   add_on: &[String],
   base_on: Option<&[Value]>,
) {
   for (key, value) in extra {
      set_prop(props, *key, Some(value.clone()));
   }
   if !add_on.is_empty() {
      let mut on = base_on.map_or_else(
         || {
            match get_prop(props, Prop::ON) {
               Some(Value::Arr(existing)) => existing.clone(),
               _ => Vec::new(),
            }
         },
         ToOwned::to_owned,
      );
      for event in add_on {
         if !on.iter().any(|v| matches!(v, Value::Str(s) if s == event)) {
            on.push(Value::Str(event.clone()));
         }
      }
      set_prop(props, Prop::ON, Some(Value::Arr(on)));
   }
}
