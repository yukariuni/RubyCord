//! The bridge sink that splices plugin nodes into Rust's UI batches.

use std::{
   collections::{
      HashMap,
      HashSet,
   },
   sync::{
      PoisonError,
      atomic::Ordering,
   },
};

use memchr::memmem;
use phoenix_ui_mobile::{
   Op,
   Prop,
   Props,
   ViewKind,
   decode,
   encode,
   get_prop,
   mirror::Created,
   prop,
   set_prop,
};
use phoenix_wire::Value;

use crate::{
   bridge::{
      Action,
      Sink,
   },
   patches::{
      ACTIVE,
      FORCE_VALUE,
      GUARDING,
      HIDDEN,
      ORIGINALS,
      PAGE,
      Patches,
      ROOT_ID,
      aids,
      patches,
      profile::profile_user,
      props::apply_prop_patch,
      targets::Target,
      tints::recolored,
   },
};

/// Swaps hooked text Rust puts in an input back to what Discord has. Returns
/// whether it swapped any.
fn restore_originals(ops: &mut [Op]) -> bool {
   let originals = ORIGINALS.lock().unwrap_or_else(PoisonError::into_inner);
   let original = |text: &str| {
      originals
         .iter()
         .find(|(shown, _)| shown == text)
         .map(|(_, original)| original.clone())
   };
   let mut restored = false;
   for op in ops {
      let (Op::Update { props, .. } | Op::Create { props, .. }) = op else {
         continue;
      };
      for (key, value) in props.iter_mut() {
         let text = match value {
            Value::Str(text) if *key == Prop::VALUE => text,
            Value::Obj(fields) if *key == FORCE_VALUE => {
               let Some((_, Value::Str(text))) = fields
                  .iter_mut()
                  .find(|(field, _)| *field == Prop::TEXT.key())
               else {
                  continue;
               };
               text
            },
            _ => continue,
         };
         if let Some(original) = original(text) {
            *text = original;
            restored = true;
         }
      }
   }
   restored
}

/// Unwraps custom badge icon URLs encoded as `/badge-icons/phx_u_<hex>.png`
/// (from `phoenix.discord.onResponse` plugins injecting entries into profile
/// `badges[]` and the badge directory) back to their real `https://` image URI.
///
/// The badge sheet draws animated art at 180 inside a 120 slot with negative
/// margins. Unwrapped art keeps the static 120 size so small GIFs are not
/// scaled up.
fn unwrap_badge_uris(ops: &mut [Op]) -> bool {
   const MARKER: &str = "/badge-icons/phx_u_";
   const BADGE_ART: i32 = 120;
   let mut changed = false;
   for op in ops {
      let (Op::Create { props, .. } | Op::Update { props, .. }) = op else {
         continue;
      };
      let mut unwrapped = false;
      for (key, value) in props.iter_mut() {
         if *key != Prop::URI {
            continue;
         }
         let Value::Str(uri) = value else {
            continue;
         };
         let Some(pos) = uri.find(MARKER) else {
            continue;
         };
         let hex = &uri[pos + MARKER.len()..];
         let hex = hex.split(['.', '?', '#']).next().unwrap_or(hex);
         if let Some(decoded) = decode_hex_url(hex) {
            *uri = decoded;
            unwrapped = true;
         }
      }
      if !unwrapped {
         continue;
      }
      changed = true;
      for (key, value) in props.iter_mut() {
         let is_size = *key == prop!("width") || *key == Prop::HEIGHT;
         match value {
            Value::Rect(margin)
               if *key == Prop::MARGIN && margin.iter().any(|side| *side < 0.0) =>
            {
               *margin = [0.0, 0.0, 0.0, 12.0];
            },
            Value::F64(size) if is_size && *size > f64::from(BADGE_ART) => {
               *size = f64::from(BADGE_ART);
            },
            Value::I32(size) if is_size && *size > BADGE_ART => *size = BADGE_ART,
            _ => {},
         }
      }
   }
   changed
}

fn decode_hex_url(hex: &str) -> Option<String> {
   if hex.is_empty() || !hex.len().is_multiple_of(2) {
      return None;
   }
   let mut bytes = Vec::with_capacity(hex.len() / 2);
   for pair in hex.as_bytes().as_chunks::<2>().0 {
      let hi = (pair[0] as char).to_digit(16)?;
      let lo = (pair[1] as char).to_digit(16)?;
      bytes.push(u8::try_from((hi << 4) | lo).ok()?);
   }
   let url = String::from_utf8(bytes).ok()?;
   url.starts_with("https://").then_some(url)
}

/// Feeds a batch to the mirror while no patch is active, so one turned on
/// later starts from the live tree.
fn track(ops: &[Op], debug: bool) {
   let mut state = patches();
   state.mirror.apply(ops, debug);
   if let Some(found) = state.mirror.root() {
      ROOT_ID.store(found, Ordering::Release);
      state.note_root_ready();
   }
}

pub struct PatchSink;

impl Sink for PatchSink {
   fn ops(&self, batch: &[u8]) -> Action {
      let active = ACTIVE.load(Ordering::Acquire);
      let debug = phoenix_runtime::remote::enabled();
      let has_badge_uri = memmem::find(batch, b"/badge-icons/phx_u_").is_some();
      let no_haptics = crate::bridge::haptics_disabled();
      if !active && !GUARDING.load(Ordering::Acquire) && !has_badge_uri && !no_haptics {
         let root = ROOT_ID.load(Ordering::Acquire);
         if !debug && root != 0 && memmem::find(batch, &root.to_le_bytes()).is_none() {
            return Action::Pass;
         }
         if let Ok(ops) = decode(batch) {
            if debug {
               crate::debug::record_ops(&ops);
            }
            track(&ops, debug);
         }
         return Action::Pass;
      }
      let mut ops = match decode(batch) {
         Ok(ops) => ops,
         Err(err) => {
            log::warn!("patches: undecodable batch: {err}");
            return Action::Pass;
         },
      };
      if debug {
         crate::debug::record_ops(&ops);
      }
      let root = ROOT_ID.load(Ordering::Acquire);
      if !active && (debug || root == 0 || memmem::find(batch, &root.to_le_bytes()).is_some()) {
         track(&ops, debug);
      }
      let restored = restore_originals(&mut ops)
         | (has_badge_uri && unwrap_badge_uris(&mut ops))
         | (no_haptics && phoenix_ui_mobile::strip_haptic_props(&mut ops));
      let rewritten = if active {
         patches().rewrite(ops, restored)
      } else {
         restored.then_some(ops)
      };
      rewritten.map_or(Action::Pass, |ops| Action::Replace(encode(&ops)))
   }

   fn dispatch(&self, frame: &[u8]) -> Action {
      if !ACTIVE.load(Ordering::Acquire) && !phoenix_runtime::remote::enabled() {
         return Action::Pass;
      }
      patches().filter(frame)
   }
}

impl Patches {
   /// Mirrors `ops` and splices plugin nodes in. `None` leaves the batch as
   /// it was.
   fn rewrite(&mut self, ops: Vec<Op>, restored: bool) -> Option<Vec<Op>> {
      let mut mine = Vec::new();
      let mut touched = HashSet::new();
      let mut patch_ops: HashMap<i32, (Props, Vec<String>)> = HashMap::new();
      let mut list_updates: HashSet<i32> = HashSet::new();
      let extra_deletes;
      {
         let (created, deleted) = self.mirror.apply(&ops, phoenix_runtime::remote::enabled());
         if let Some(found) = self.mirror.root() {
            ROOT_ID.store(found, Ordering::Release);
            self.note_root_ready();
         }
         extra_deletes = self.drop_deleted(&deleted);
         self.track_texts(&ops, &created, &deleted);

         let mut named = created
            .keys()
            .copied()
            .filter(|id| self.mirror.accessibility_id(*id).is_some())
            .collect::<Vec<_>>();
         named.sort_unstable();

         let skip_action_placement =
            self.match_prop_patches(&named, &created, &mut patch_ops, &mut list_updates);
         self.notify_watchers(&named);
         self.learn(&named, &ops, &created);
         self.anchor(
            named,
            skip_action_placement,
            &created,
            &mut mine,
            &mut touched,
         );
      }

      let composer_slash = self
         .mirror
         .composer()
         .is_some_and(|(_, text)| text.starts_with('/'));
      let mut extra = Vec::new();
      if !self.commands.is_empty()
         && self.suggestions.scroll.is_some()
         && (self.has_suggestions() || composer_slash)
      {
         self.suggest(&mut extra);
      }

      let hidden_ops = self.reconcile_hidden();
      let slot_parents = self
         .slots
         .iter()
         .map(|slot| slot.parent)
         .collect::<HashSet<_>>();
      if touched.is_empty()
         && extra_deletes.is_empty()
         && extra.is_empty()
         && hidden_ops.is_empty()
         && patch_ops.is_empty()
         && list_updates.is_empty()
         && !restored
         && !self.rewrites(&ops, &slot_parents)
      {
         return None;
      }

      let mut splice = Splice {
         patch_ops,
         list_updates,
         extra_deletes,
         slot_parents,
         touched,
         left: Vec::new(),
         recolor: Vec::new(),
      };
      let mut out = mine;
      for op in ops {
         self.splice(op, &mut splice, &mut out);
      }
      for list_id in splice.list_updates {
         if let Some(op) = self.list_update_op(list_id) {
            out.push(op);
         }
      }
      for parent in splice.touched {
         out.push(Op::Children {
            id:  parent,
            ids: self.merged(parent),
         });
      }
      out.extend(hidden_ops);
      out.extend(extra);
      for pager in splice.left {
         out.extend(self.leave_tab(pager, None));
      }
      for (pager, index) in splice.recolor {
         out.extend(self.rust_label_colors(pager, index));
      }
      out.append(&mut self.late);
      Some(out)
   }

   fn track_texts(&mut self, ops: &[Op], created: &Created<'_>, deleted: &[i32]) {
      for id in deleted {
         self.texts.remove(id);
      }
      for (id, (kind, props)) in created {
         if **kind == ViewKind::Text
            && let Some(message) = self.text_message(*id)
         {
            self.texts.insert(*id, (message, (*props).clone()));
         }
      }
      for op in ops {
         if let Op::Update { id, props } = op
            && let Some(height) = get_prop(props, Prop::HEIGHT)
            && self
               .anchors
               .get(&Target::Tab)
               .is_some_and(|pagers| pagers.contains(id))
         {
            self.rust_heights.insert(*id, height.clone());
         }
         if let Op::Update { id, props } = op
            && let Some((_, stored)) = self.texts.get_mut(id)
         {
            for (key, value) in props {
               set_prop(stored, *key, Some(value.clone()));
            }
         }
      }
   }

   fn learn(&mut self, named: &[i32], ops: &[Op], created: &Created<'_>) {
      for id in named {
         let aid = self
            .mirror
            .accessibility_id(*id)
            .unwrap_or_default()
            .to_owned();
         self.templates.learn(&self.mirror, *id, &aid, created);
         if aid == aids::SUGGESTIONS {
            self.suggestions.scroll = Some(*id);
         }
         if aid == aids::PROFILE_TAB_PAGES {
            self.seen.profile_user = profile_user(ops);
         }
      }
      if self.suggestions.scroll.is_some() {
         self.learn_suggestion_row(created);
      }
   }

   fn anchor(
      &mut self,
      named: Vec<i32>,
      skip_action_placement: bool,
      created: &Created<'_>,
      mine: &mut Vec<Op>,
      touched: &mut HashSet<i32>,
   ) {
      for id in named {
         let aid = self
            .mirror
            .accessibility_id(id)
            .unwrap_or_default()
            .to_owned();
         for target in Target::ALL {
            if skip_action_placement && target == Target::Action {
               continue;
            }
            if !target.matches(&aid) {
               continue;
            }
            self.anchors.entry(target).or_default().insert(id);
            if target == Target::Tab
               && let Some((_, props)) = created.get(&id)
               && let Some(height) = get_prop(props, Prop::HEIGHT)
            {
               self.rust_heights.insert(id, height.clone());
            }
            if let Some(parent) = self.place(target, id, mine) {
               touched.insert(parent);
            }
         }
      }
   }

   fn rewrites(&self, ops: &[Op], slot_parents: &HashSet<i32>) -> bool {
      let rewrites_children = ops.iter().any(|op| {
         matches!(op, Op::Children { id, .. } | Op::ChildEdits { id, .. }
            if slot_parents.contains(id))
      });
      let unhides_hidden = !self.hidden_nodes.is_empty()
         && ops.iter().any(|op| {
            matches!(op, Op::Update { id, props }
               if self.hidden_nodes.contains(id) && get_prop(props, HIDDEN).is_some())
         });
      let tabbed = ops.iter().any(|op| {
         matches!(op, Op::Update { id, .. }
            if self.shown.contains_key(id) || self.static_tabs.contains(id))
      });
      let recolors = !self.tints.is_empty()
         && ops.iter().any(|op| {
            matches!(op, Op::Create { id, .. } | Op::Update { id, .. } if self.tint_of(*id).is_some())
         });
      let updates_list_on = !self.patched_lists.is_empty()
         && ops.iter().any(|op| {
            matches!(op, Op::Create { id, props, .. } | Op::Update { id, props }
               if self.patched_lists.contains_key(id) && get_prop(props, Prop::ON).is_some())
         });
      unhides_hidden || updates_list_on || rewrites_children || recolors || tabbed
   }

   fn splice(&self, op: Op, splice: &mut Splice, out: &mut Vec<Op>) {
      match op {
         Op::Create {
            id,
            kind,
            mut props,
         } if splice.patch_ops.contains_key(&id) => {
            if let Some((extra_props, add_on)) = splice.patch_ops.get(&id) {
               apply_prop_patch(&mut props, extra_props, add_on, None);
            }
            out.push(Op::Create { id, kind, props });
         },
         Op::Create {
            id,
            kind,
            mut props,
         } if self.patched_lists.contains_key(&id) && get_prop(&props, Prop::ON).is_some() => {
            let (extra_props, add_on) = self.list_patch_for(id);
            apply_prop_patch(
               &mut props,
               &extra_props,
               &add_on,
               Some(self.mirror.list_on(id)),
            );
            splice.list_updates.remove(&id);
            out.push(Op::Create { id, kind, props });
         },
         Op::Update { id, mut props }
            if self.patched_lists.contains_key(&id) && get_prop(&props, Prop::ON).is_some() =>
         {
            let (extra_props, add_on) = self.list_patch_for(id);
            apply_prop_patch(
               &mut props,
               &extra_props,
               &add_on,
               Some(self.mirror.list_on(id)),
            );
            splice.list_updates.remove(&id);
            out.push(Op::Update { id, props });
         },
         Op::Update { id, mut props }
            if self.shown.contains_key(&id) || self.static_tabs.contains(&id) =>
         {
            if let Some(page) = get_prop(&props, PAGE) {
               // Rust opened one of its tabs.
               if self.shown.contains_key(&id) {
                  splice.left.push(id);
               }
               let index = page
                  .field(Prop::INDEX.key())
                  .and_then(Value::as_i32)
                  .and_then(|index| usize::try_from(index).ok());
               splice.recolor.push((id, index));
            } else if self.shown.contains_key(&id)
               && get_prop(&props, Prop::HEIGHT).is_some()
               && let Some(height) = self
                  .shown
                  .get(&id)
                  .and_then(|shown| self.page_heights.get(&(id, *shown)))
            {
               // Rust sized the pager to its own page, under ours.
               set_prop(&mut props, Prop::HEIGHT, Some(Value::F64(*height)));
            }
            out.push(Op::Update { id, props });
         },
         Op::Update { id, mut props }
            if self.hidden_nodes.contains(&id) && get_prop(&props, HIDDEN).is_some() =>
         {
            set_prop(&mut props, HIDDEN, Some(Value::Bool(true)));
            out.push(Op::Update { id, props });
         },
         Op::Children { id, .. } | Op::ChildEdits { id, .. }
            if splice.slot_parents.contains(&id) =>
         {
            out.push(Op::Children {
               id,
               ids: self.merged(id),
            });
            splice.touched.remove(&id);
         },
         Op::Delete { mut ids } if !splice.extra_deletes.is_empty() => {
            ids.append(&mut splice.extra_deletes);
            out.push(Op::Delete { ids });
         },
         Op::Create { id, kind, props } if let Some(color) = self.tint_of(id) => {
            out.push(Op::Create {
               id,
               kind,
               props: recolored(&props, color, true),
            });
         },
         Op::Update { id, props } if let Some(color) = self.tint_of(id) => {
            out.push(Op::Update {
               id,
               props: recolored(&props, color, false),
            });
         },
         op => out.push(op),
      }
   }
}

struct Splice {
   patch_ops:     HashMap<i32, (Props, Vec<String>)>,
   list_updates:  HashSet<i32>,
   extra_deletes: Vec<i32>,
   slot_parents:  HashSet<i32>,
   touched:       HashSet<i32>,
   left:          Vec<i32>,
   recolor:       Vec<(i32, Option<usize>)>,
}
