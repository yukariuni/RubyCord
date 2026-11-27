//! Builds, rerenders and closes phoenix's pages on the navigator.

use phoenix_ui_mobile::{
   Op,
   Prop,
   Props,
   motion::{
      self,
      Spring,
   },
   prop,
   templates,
};
use phoenix_wire::Value;

use crate::settings::{
   Field,
   Job,
   Page,
   PageKind,
   Press,
   Settings,
   Slot,
   TITLE,
   code::{
      code_sections,
      code_spans,
   },
   console::{
      console_sections,
      log_lines,
   },
   pages::{
      install_sections,
      main_sections,
   },
   plugin::plugin_sections,
   tree::{
      Tree,
      same_shape,
      text,
   },
};

/// Ids Phoenix's captured page layout uses for the parts phoenix fills in.
const PAGE_HEADER_SPACER: u32 = 0x4E1;
const PAGE_CONTENT: u32 = 0x4E4;
const PAGE_HEADER: u32 = 0x562;
const PAGE_BACK: u32 = 0x565;
const PAGE_TITLE: u32 = 0x56A;

impl Settings {
   pub fn page_index(&self) -> usize {
      self.screen.as_ref().map_or(0, |screen| {
         (screen.rust_pages.as_ref().map_or(0, Vec::len) + screen.stack.len()).saturating_sub(1)
      })
   }

   pub fn build(&mut self, tree: &mut Tree, inherited: Option<Press>, ops: &mut Vec<Op>) -> i32 {
      let press = tree.press.or(inherited);
      let kids = tree
         .kids
         .iter_mut()
         .map(|kid| self.build(kid, press, ops))
         .collect::<Vec<i32>>();
      tree.id = self.next_id;
      self.next_id += 1;
      if let Some(press) = press {
         self.handlers.insert(tree.id, press);
      }
      let mut props = tree.props.clone();
      let motion = tree.animate.as_ref().map(|animate| {
         let (first, reused) = self.slots.take(tree.id, animate.worklet.slot_count());
         let bound = animate.worklet.bind(first);
         props.push(bound.prop());
         let start = if animate.flipped {
            &animate.other
         } else {
            &tree.props
         };
         let mut commands = bound.rest(start);
         if reused {
            commands.extend(bound.pin(start, Spring::SWITCH));
         }
         if animate.flipped {
            commands.extend(bound.spring(start, &tree.props, Spring::SWITCH));
         }
         commands
      });
      ops.push(Op::Create {
         id: tree.id,
         kind: tree.kind.clone(),
         props,
      });
      if let Some(commands) = motion {
         self.motion.extend(motion::words(&commands));
      }
      if !kids.is_empty() {
         ops.push(Op::Children {
            id:  tree.id,
            ids: kids,
         });
      }
      tree.id
   }

   pub fn forget(&mut self, ids: &[i32]) {
      for id in ids {
         self.handlers.remove(id);
         self.slots.release(*id);
         self.log_rows.remove(id);
      }
   }

   fn navigator_ops(&self) -> Vec<Op> {
      let Some(screen) = &self.screen else {
         return Vec::new();
      };
      let Some(navigator) = screen.navigator else {
         return Vec::new();
      };
      let mut ids = screen.rust_pages.clone().unwrap_or_default();
      ids.extend(screen.stack.iter().map(|page| page.root));
      vec![Op::Children { id: navigator, ids }, Op::Update {
         id:    navigator,
         props: vec![(
            Prop::INDEX,
            Value::I32(i32::try_from(self.page_index()).unwrap_or(i32::MAX)),
         )],
      }]
   }

   fn slots(&self) -> impl Iterator<Item = (i32, Slot)> + '_ {
      self
         .screen
         .iter()
         .flat_map(|screen| &screen.stack)
         .flat_map(|page| page.slots.iter().copied())
   }

   pub fn field(&self, node: i32) -> Option<Field> {
      self.slots().find_map(|(id, slot)| {
         match slot {
            Slot::Input(field) if id == node => Some(field),
            _ => None,
         }
      })
   }

   /// Fills every open console's log box with the current lines.
   // ponytail: rebuilds all lines per refresh (refreshes coalesce through
   // LOG_QUEUED); prepend only the new lines if this ever shows up in a trace.
   fn log_ops(&mut self) -> Vec<Op> {
      let boxes = self
         .slots()
         .filter(|(_, slot)| *slot == Slot::Log)
         .map(|(id, _)| id)
         .collect::<Vec<i32>>();
      let mut ops = Vec::new();
      for log_box in boxes {
         let mut lines = log_lines();
         let ids = lines
            .iter_mut()
            .map(|line| self.build(line, None, &mut ops))
            .collect::<Vec<i32>>();
         let old = self
            .log_rows
            .insert(log_box, ids.clone())
            .unwrap_or_default();
         if let Some(page) = self.screen.as_mut().and_then(|screen| {
            screen
               .stack
               .iter_mut()
               .find(|page| page.slots.contains(&(log_box, Slot::Log)))
         }) {
            page.ids.retain(|id| !old.contains(id));
            page.ids.extend(&ids);
         }
         ops.push(Op::Children { id: log_box, ids });
         if !old.is_empty() {
            self.forget(&old);
            ops.push(Op::Delete { ids: old });
         }
      }
      ops
   }

   /// Empties the inputs that hold `field`. `forceValue` rather than `value`,
   /// which the host ignores while the keyboard has edits in flight.
   fn clear_ops(&mut self, field: Field) -> Vec<Op> {
      let inputs = self
         .slots()
         .filter(|(_, slot)| *slot == Slot::Input(field))
         .map(|(id, _)| id)
         .collect::<Vec<i32>>();
      inputs
         .into_iter()
         .map(|id| {
            self.force_seq += 1;
            Op::Update {
               id,
               props: vec![(
                  prop!("forceValue"),
                  Value::Obj(vec![
                     (prop!("seq").key(), Value::I64(self.force_seq)),
                     (prop!("text").key(), text("")),
                     (prop!("start").key(), Value::I32(0)),
                     (prop!("end").key(), Value::I32(0)),
                  ]),
               )],
            }
         })
         .collect()
   }

   pub fn run(&mut self, job: Job) -> Vec<Op> {
      match job {
         Job::Draft(field, draft) => {
            let ops = match field {
               Field::Code(_) => {
                  self
                     .slots()
                     .filter(|(_, slot)| *slot == Slot::Input(field))
                     .map(|(id, _)| {
                        Op::Update {
                           id,
                           props: vec![(prop!("valueSpans"), code_spans(&draft))],
                        }
                     })
                     .collect()
               },
               _ => Vec::new(),
            };
            self.drafts.insert(field, draft);
            ops
         },
         Job::Log => self.log_ops(),
         Job::Submit(Field::Eval) => {
            let mut ops = self.log_ops();
            ops.extend(self.clear_ops(Field::Eval));
            ops
         },
         Job::Submit(Field::Code(_)) | Job::Press(Press::PickFile | Press::OpenSite) => Vec::new(),
         Job::Refresh
         | Job::Press(
            Press::SaveCode(_) | Press::Reload(_) | Press::ApplyUpdate | Press::Listed(_),
         )
         | Job::Picked(_)
         | Job::Submit(Field::Install | Field::Setting(..)) => self.rerender_all(None),
         Job::Press(Press::OpenMain) => self.open_page(PageKind::Main),
         Job::Press(Press::OpenConsole) => {
            let mut ops = self.open_page(PageKind::Console);
            ops.extend(self.log_ops());
            ops
         },
         Job::Press(Press::OpenPlugin(index)) => self.open_page(PageKind::Plugin(index)),
         Job::Press(Press::OpenInstall) => {
            self.install_status = None;
            self.open_page(PageKind::Install)
         },
         Job::Press(Press::EditCode(index)) => self.open_page(PageKind::Code(index)),
         Job::Press(
            press @ (Press::Toggle(_)
            | Press::Setting(..)
            | Press::Choose(..)
            | Press::ToggleStaff
            | Press::ToggleRemoteShell),
         ) => self.rerender_all(Some(press)),
         Job::Press(Press::Back) => self.close_top(),
         Job::Press(Press::Uninstall(_)) => {
            let mut ops = self.close_top();
            ops.extend(self.rerender_all(None));
            ops
         },
         Job::NavigateTo(index) => {
            let mut ops = Vec::new();
            while self
               .screen
               .as_ref()
               .is_some_and(|screen| !screen.stack.is_empty())
               && self.page_index() > index
            {
               ops.extend(self.close_top());
            }
            ops
         },
      }
   }

   fn render(&self, kind: PageKind) -> (String, Vec<Tree>) {
      match kind {
         PageKind::Main => (TITLE.to_owned(), main_sections(&self.plugins)),
         PageKind::Console => ("Console".to_owned(), console_sections()),
         PageKind::Install => {
            (
               "Install plugin".to_owned(),
               install_sections(self.install_status.as_ref(), &self.plugins),
            )
         },
         PageKind::Code(index) => {
            let source = self.drafts.get(&Field::Code(index)).map(String::as_str);
            let name = self
               .plugins
               .get(index)
               .map_or("plugin", |plugin| plugin.name.as_str());
            ("Edit code".to_owned(), code_sections(index, name, source))
         },
         PageKind::Plugin(index) => {
            self.plugins.get(index).map_or_else(
               || ("Plugin".to_owned(), Vec::new()),
               |plugin| (plugin.name.clone(), plugin_sections(index, plugin)),
            )
         },
      }
   }

   fn build_sections(&mut self, sections: &mut [Tree], ops: &mut Vec<Op>) -> (Vec<i32>, Vec<i32>) {
      let roots = sections
         .iter_mut()
         .map(|section| self.build(section, None, ops))
         .collect();
      let ids = sections.iter().flat_map(Tree::ids).collect();
      (roots, ids)
   }

   /// Rebuilds every open page. The switches `flipped` targets spring from
   /// their old state.
   fn rerender_all(&mut self, flipped: Option<Press>) -> Vec<Op> {
      let kinds = self.screen.as_ref().map_or_default(|screen| {
         screen
            .stack
            .iter()
            .map(|page| page.kind)
            .collect::<Vec<PageKind>>()
      });
      let mut ops = Vec::new();
      for (position, kind) in kinds.into_iter().enumerate() {
         // Rebuilding these would drop what is being typed into them.
         if matches!(kind, PageKind::Console | PageKind::Code(_)) {
            continue;
         }
         let (_, mut sections) = self.render(kind);
         // Same layout: update the changed nodes and spring the flipped switch,
         // so the other rows are not rebuilt (and do not flicker).
         let old = self
            .screen
            .as_mut()
            .and_then(|screen| screen.stack.get_mut(position))
            .map_or_default(|page| std::mem::take(&mut page.trees));
         if old.len() == sections.len() && old.iter().zip(&sections).all(|(a, b)| same_shape(a, b))
         {
            for (old, new) in old.iter().zip(&mut sections) {
               self.patch(old, new, &mut ops);
            }
            if let Some(page) = self
               .screen
               .as_mut()
               .and_then(|screen| screen.stack.get_mut(position))
            {
               page.trees = sections;
            }
            continue;
         }
         if let Some(press) = flipped {
            for section in &mut sections {
               section.flip(press, false);
            }
         }
         let (roots, ids) = self.build_sections(&mut sections, &mut ops);
         let Some(page) = self
            .screen
            .as_mut()
            .and_then(|screen| screen.stack.get_mut(position))
         else {
            continue;
         };
         page.slots = sections.iter().flat_map(Tree::slots).collect();
         page.trees = sections;
         let old = std::mem::replace(&mut page.sections, ids.clone());
         page.ids.retain(|id| !old.contains(id));
         page.ids.extend(ids);
         ops.push(Op::Children {
            id:  page.content,
            ids: roots,
         });
         ops.push(Op::Delete { ids: old.clone() });
         self.forget(&old);
      }
      ops
   }

   /// Gives `new` the ids of `old` (same shape) and sends what changed.
   fn patch(&mut self, old: &Tree, new: &mut Tree, ops: &mut Vec<Op>) {
      new.id = old.id;
      if let Some(animate) = &new.animate {
         if old.props != new.props
            && let Some(&(first, _)) = self.slots.owned.get(&old.id)
         {
            let commands =
               animate
                  .worklet
                  .bind(first)
                  .spring(&old.props, &new.props, Spring::SWITCH);
            self.motion.extend(motion::words(&commands));
         }
      } else {
         let changed = new
            .props
            .iter()
            .filter(|(key, value)| !old.props.iter().any(|(k, v)| k == key && v == value))
            .cloned()
            .collect::<Props>();
         if !changed.is_empty() {
            ops.push(Op::Update {
               id:    old.id,
               props: changed,
            });
         }
      }
      for (old, new) in old.kids.iter().zip(&mut new.kids) {
         self.patch(old, new, ops);
      }
   }

   fn open_page(&mut self, kind: PageKind) -> Vec<Op> {
      let Some(screen) = &self.screen else {
         return Vec::new();
      };
      if screen.rust_pages.is_none() {
         log::warn!("settings: no navigator pages known yet");
         return Vec::new();
      }
      let header = screen.header.clone();
      let (title, sections) = self.render(kind);
      let title = title.as_str();

      let mut page = Tree::from_template(&templates().page);
      page.set(Prop::PANE_TITLE, Some(text(title)));
      if let Some(title_node) = page.find(PAGE_TITLE) {
         title_node.set(Prop::TEXT, Some(text(title)));
      }
      if let Some(header) = header {
         let value = |key: Prop| {
            header
               .iter()
               .find(|(existing, _)| *existing == key)
               .map(|(_, value)| value.clone())
         };
         let height = value(Prop::HEIGHT);
         if let Some(node) = page.find(PAGE_HEADER) {
            for key in [Prop::PADDING, Prop::HEIGHT] {
               if let Some(found) = value(key) {
                  node.set(key, Some(found));
               }
            }
         }
         if let (Some(node), Some(height)) = (page.find(PAGE_HEADER_SPACER), height) {
            node.set(Prop::HEIGHT, Some(height));
         }
      }
      if let Some(back) = page.find(PAGE_BACK) {
         back.set(Prop::ACCESSIBILITY_LABEL, Some(text("Back")));
         back.press = Some(Press::Back);
      }
      if let Some(content) = page.find(PAGE_CONTENT) {
         content.kids = sections;
      }

      let mut ops = Vec::new();
      let root = self.build(&mut page, None, &mut ops);
      let ids = page.ids();
      let slots = page.slots();
      let Some(content) = page.find(PAGE_CONTENT) else {
         log::warn!("settings: the page layout has no content node");
         return Vec::new();
      };
      let sections = content.kids.iter().flat_map(Tree::ids).collect();
      let trees = std::mem::take(&mut content.kids);
      let content = content.id;
      if let Some(screen) = &mut self.screen {
         screen.stack.push(Page {
            root,
            ids,
            kind,
            content,
            sections,
            slots,
            trees,
         });
      }
      ops.extend(self.navigator_ops());
      ops
   }

   fn close_top(&mut self) -> Vec<Op> {
      let Some(page) = self.screen.as_mut().and_then(|screen| screen.stack.pop()) else {
         return Vec::new();
      };
      self.forget(&page.ids);
      let mut ops = self.navigator_ops();
      ops.push(Op::Delete { ids: page.ids });
      ops
   }
}
