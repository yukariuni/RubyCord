//! Rewrites Rust's UI batches to splice phoenix's nodes in, and takes view
//! events on them out of Java's dispatches.

use std::collections::{
   HashMap,
   HashSet,
};

use memchr::memmem;
use phoenix_ui_mobile::{
   ChildEdit,
   Op,
   Prop,
   Props,
   ViewKind,
   decode,
   encode,
};
use phoenix_wire::{
   DispatchEvent,
   Value,
   decode_dispatch,
   without_frames,
};

use crate::{
   bridge::Action,
   settings::{
      Entry,
      ICON_PLUGIN,
      Job,
      Press,
      Screen,
      Settings,
      TITLE,
      WORKER,
      rows::{
         button_row,
         section,
      },
      settings,
   },
};

const LOGOUT_ROW: &str = "settings.row.logout";

/// Splices phoenix's nodes into a Rust-to-Java UI batch.
pub fn rewrite_ops(batch: &[u8]) -> Action {
   // Most batches carry neither the Settings list nor a navigator, so they
   // pass without being decoded.
   let relevant = settings().screen.is_some()
      || memmem::find(batch, LOGOUT_ROW.as_bytes()).is_some()
      || memmem::find(batch, ViewKind::Navigator.as_str().as_bytes()).is_some();
   if !relevant {
      return Action::Pass;
   }
   let ops = match decode(batch) {
      Ok(ops) => ops,
      Err(err) => {
         log::warn!("settings: undecodable batch: {err}");
         return Action::Pass;
      },
   };
   settings()
      .rewrite(ops)
      .map_or(Action::Pass, |ops| Action::Replace(encode(&ops)))
}

/// Takes view events on phoenix's nodes out of a Java-to-Rust dispatch.
pub fn filter_dispatch(frame: &[u8]) -> Action {
   let Ok(items) = decode_dispatch(frame) else {
      return Action::Pass;
   };
   let mut taken = Vec::new();
   let mut jobs = Vec::new();
   {
      let state = settings();
      let Some(screen) = &state.screen else {
         return Action::Pass;
      };
      for item in &items {
         let DispatchEvent::View {
            node,
            event,
            payload,
         } = &item.event
         else {
            continue;
         };
         let job = if *event == Prop::NAVIGATE.key()
            && !screen.stack.is_empty()
            && Some(*node) == screen.navigator
         {
            let index = payload
               .field(Prop::INDEX.key())
               .and_then(Value::as_i32)
               .and_then(|index| usize::try_from(index).ok());
            Some(Job::NavigateTo(
               index.unwrap_or_else(|| state.page_index().saturating_sub(1)),
            ))
         } else if let Some(field) = state.field(*node) {
            if *event == Prop::CHANGE_TEXT.key() {
               text_of(payload).map(|text| Job::Draft(field, text))
            } else {
               (*event == Prop::SUBMIT.key()).then_some(Job::Submit(field))
            }
         } else if let Some(press) = state.handlers.get(node) {
            (*event == Prop::PRESS.key()).then_some(Job::Press(*press))
         } else {
            continue;
         };
         taken.push(item.offset);
         jobs.extend(job);
      }
   }

   if let Some(worker) = WORKER.get() {
      for job in jobs {
         if worker.send(job).is_err() {
            log::warn!("settings: the worker stopped");
            break;
         }
      }
   }
   match (taken.len(), frame.first()) {
      (0, _) => Action::Pass,
      (count, _) if count == items.len() => Action::Drop,
      (_, Some(4)) => Action::Replace(without_frames(frame, &taken)),
      _ => Action::Drop,
   }
}

/// A `changeText` payload is the text itself or an object holding it.
fn text_of(payload: &Value) -> Option<String> {
   match payload {
      Value::Str(text) => Some(text.clone()),
      Value::Obj(_) => {
         [Prop::TEXT, Prop::VALUE]
            .into_iter()
            .find_map(|prop| payload.field(prop.key())?.as_str().map(str::to_owned))
      },
      _ => {
         log::warn!("settings: unexpected changeText payload {payload:?}");
         None
      },
   }
}

impl Settings {
   fn entry_ops(&mut self) -> (Entry, Vec<Op>) {
      let row = button_row(TITLE, ICON_PLUGIN, Some(Press::OpenMain), true);
      let mut section = section("", vec![row]);
      let mut ops = Vec::new();
      let root = self.build(&mut section, None, &mut ops);
      (
         Entry {
            root,
            ids: section.ids(),
         },
         ops,
      )
   }

   fn rewrite(&mut self, ops: Vec<Op>) -> Option<Vec<Op>> {
      let mut creates = HashMap::new();
      let mut parents = HashMap::new();
      for op in &ops {
         match op {
            Op::Create { id, kind, props } => {
               if *kind == ViewKind::Navigator {
                  self.navigators.insert(*id);
               }
               creates.insert(*id, (kind, props));
            },
            Op::Children { id, ids } => {
               for kid in ids {
                  parents.insert(*kid, *id);
               }
            },
            _ => {},
         }
      }
      self.find_screen(&creates, &parents);
      self.screen.as_ref()?;
      let stacks = creates
         .iter()
         .filter(|(_, (kind, _))| **kind == ViewKind::Stack)
         .map(|(id, _)| *id)
         .collect::<HashSet<i32>>();
      drop(creates);

      let mut changed = false;
      let mut out = Vec::with_capacity(ops.len());
      let mut rest = ops.into_iter();
      while let Some(op) = rest.next() {
         let screen = self.screen.as_ref().expect("checked above");
         let (list, navigator) = (screen.list, screen.navigator);
         match op {
            Op::Children { id, ids } if id == list => {
               let search = ids.first().copied().filter(|first| stacks.contains(first));
               self.list_changed(ids, search, &mut out);
               changed = true;
            },
            Op::ChildEdits { id, edits } if id == list && screen.rust_rows.is_some() => {
               let ids = apply_edits(screen.rust_rows.as_deref().unwrap_or_default(), &edits);
               let search = screen.search;
               self.list_changed(ids, search, &mut out);
               changed = true;
            },
            Op::Children { id, ids } if Some(id) == navigator => {
               let original = Op::Children {
                  id,
                  ids: ids.clone(),
               };
               changed |= self.navigator_changed(original, ids, &mut out);
            },
            Op::ChildEdits { id, edits }
               if Some(id) == navigator && screen.rust_pages.is_some() =>
            {
               let ids = apply_edits(screen.rust_pages.as_deref().unwrap_or_default(), &edits);
               changed |= self.navigator_changed(Op::ChildEdits { id, edits }, ids, &mut out);
            },
            Op::Delete { mut ids }
               if ids.contains(&list)
                  || navigator.is_some_and(|navigator| ids.contains(&navigator)) =>
            {
               // Java's delete does not recurse, so phoenix's nodes are listed
               // here.
               let screen = self.screen.take().expect("checked above");
               let ours = screen
                  .entry
                  .into_iter()
                  .flat_map(|entry| entry.ids)
                  .chain(screen.stack.into_iter().flat_map(|page| page.ids))
                  .collect::<Vec<i32>>();
               self.forget(&ours);
               ids.extend(ours);
               out.push(Op::Delete { ids });
               out.extend(rest.by_ref());
               changed = true;
            },
            other => out.push(other),
         }
      }
      changed.then_some(out)
   }

   fn find_screen(
      &mut self,
      creates: &HashMap<i32, (&ViewKind, &Props)>,
      parents: &HashMap<i32, i32>,
   ) {
      for (id, (_, props)) in creates {
         let is_logout = props.iter().any(|(key, value)| {
            *key == Prop::ACCESSIBILITY_ID
               && matches!(value, Value::Str(found) if found == LOGOUT_ROW)
         });
         if !is_logout {
            continue;
         }
         let Some(list) = parents
            .get(id)
            .and_then(|row| parents.get(row))
            .and_then(|group| parents.get(group))
            .copied()
         else {
            continue;
         };
         if self
            .screen
            .as_ref()
            .is_some_and(|screen| screen.list == list)
         {
            continue;
         }

         let presented = |node: &i32| {
            creates.get(node).is_some_and(|(_, props)| {
               props
                  .iter()
                  .any(|(key, _)| *key == Prop::PRESENTATION || *key == Prop::UIKIT_PUSH)
            })
         };
         let mut navigator = None;
         let mut cursor = Some(list);
         while let Some(node) = cursor {
            if self.navigators.contains(&node) {
               navigator = Some(node);
               break;
            }
            if presented(&node) {
               navigator = parents.get(&node).copied();
               break;
            }
            cursor = parents.get(&node).copied();
         }
         let header = creates
            .values()
            .find(|(_, props)| props.iter().any(|(key, _)| *key == Prop::STACK_HEADER))
            .map(|(_, props)| (*props).clone());
         self.screen = Some(Screen {
            list,
            rust_rows: None,
            search: None,
            navigator,
            rust_pages: None,
            header,
            entry: None,
            stack: Vec::new(),
         });
      }
   }

   fn list_changed(&mut self, ids: Vec<i32>, search: Option<i32>, out: &mut Vec<Op>) {
      if self
         .screen
         .as_ref()
         .is_some_and(|screen| screen.entry.is_none())
      {
         let (entry, ops) = self.entry_ops();
         out.extend(ops);
         if let Some(screen) = &mut self.screen {
            screen.entry = Some(entry);
         }
      }
      let Some(screen) = &mut self.screen else {
         return;
      };
      if screen.search.is_none() {
         screen.search = search;
      }
      let at = usize::from(screen.search.is_some() && ids.first() == screen.search.as_ref());
      let mut with_entry = ids.clone();
      if let Some(entry) = &screen.entry {
         with_entry.insert(at, entry.root);
      }
      screen.rust_rows = Some(ids);
      out.push(Op::Children {
         id:  screen.list,
         ids: with_entry,
      });
   }

   /// Records Rust's pages on the navigator. While phoenix has pages open,
   /// they stay on top of Rust's.
   fn navigator_changed(&mut self, original: Op, ids: Vec<i32>, out: &mut Vec<Op>) -> bool {
      let Some(screen) = &mut self.screen else {
         out.push(original);
         return false;
      };
      let (Some(navigator), false) = (screen.navigator, screen.stack.is_empty()) else {
         screen.rust_pages = Some(ids);
         out.push(original);
         return false;
      };
      let mut all = ids.clone();
      all.extend(screen.stack.iter().map(|page| page.root));
      screen.rust_pages = Some(ids);
      out.push(Op::Children {
         id:  navigator,
         ids: all,
      });
      true
   }
}

fn apply_edits(old: &[i32], edits: &[ChildEdit]) -> Vec<i32> {
   let mut out = Vec::new();
   let mut cursor = 0;
   for edit in edits {
      let keep = usize::try_from(edit.keep).unwrap_or(0);
      let del = usize::try_from(edit.del).unwrap_or(0);
      out.extend(old.iter().skip(cursor).take(keep));
      cursor += keep + del;
      out.extend(&edit.ins);
   }
   out.extend(old.iter().skip(cursor));
   out
}
