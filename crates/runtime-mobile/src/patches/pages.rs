//! Full-screen plugin pages pushed onto Phoenix's active navigator.

use std::sync::atomic::Ordering;

use phoenix_ui_mobile::{
   Op,
   Prop,
   ViewKind,
   get_prop,
   prop,
};
use phoenix_wire::Value;

use crate::{
   js::page::page_closed,
   patches::{
      ACTIVE,
      Before,
      Draft,
      Handler,
      Patches,
      Place,
      Slot,
      SlotKind,
      patches,
      worker::{
         Job,
         queue,
      },
   },
};

/// A button row on a plugin full-screen page.
pub struct PageButton {
   pub label:  String,
   pub icon:   Option<String>,
   pub danger: bool,
   pub index:  u32,
}

/// A section on a plugin full-screen page.
pub enum PageSection {
   Code {
      title:     Option<String>,
      text:      String,
      monospace: bool,
      highlight: bool,
   },
   Text {
      title: Option<String>,
      text:  String,
   },
   Buttons {
      title:   Option<String>,
      buttons: Vec<PageButton>,
   },
}

/// Pushes a full-screen page onto Phoenix's active navigator.
pub fn open_page(id: u32, title: String, sections: Vec<PageSection>) {
   ACTIVE.store(true, Ordering::Release);
   queue(Job::OpenPage {
      id,
      title,
      sections,
   });
}

/// Closes a plugin full-screen page pushed with [`open_page`].
pub fn close_page(id: u32) {
   queue(Job::ClosePage(id));
}

const PAGE_HEADER_SPACER: u32 = 0x4E1;
const PAGE_CONTENT: u32 = 0x4E4;
const PAGE_HEADER: u32 = 0x562;
const PAGE_BACK: u32 = 0x565;
const PAGE_TITLE: u32 = 0x56A;
const RADII: [Prop; 5] = [
   Prop::RADIUS,
   Prop::RADIUS_TOP_LEFT,
   Prop::RADIUS_TOP_RIGHT,
   Prop::RADIUS_BOTTOM_LEFT,
   Prop::RADIUS_BOTTOM_RIGHT,
];

impl Patches {
   /// The topmost plugin page pushed onto `navigator`, if any.
   pub fn top_page_on(&self, navigator: i32) -> Option<u32> {
      self.slots.iter().rev().find_map(|slot| {
         match slot.kind {
            SlotKind::Page(id) if slot.parent == navigator => Some(id),
            _ => None,
         }
      })
   }

   /// Pushes a full-screen native Phoenix page onto the active `Navigator`.
   pub fn open_page(&mut self, id: u32, title: &str, sections: Vec<PageSection>) -> Vec<Op> {
      let Some(navigator) = self.mirror.active_navigator() else {
         log::warn!("open_page: no active navigator");
         return Vec::new();
      };
      log::info!("open_page: pushing page {id} onto navigator {navigator}");
      let layouts = phoenix_ui_mobile::templates();
      let mut page = Draft::from_node(&layouts.page);
      page.set(Prop::PANE_TITLE, Some(Value::Str(title.to_owned())));
      if let Some(title_node) = page.find(PAGE_TITLE) {
         title_node.set(Prop::TEXT, Some(Value::Str(title.to_owned())));
      }
      let (header_padding, header_height) = self.mirror.stack_header().map_or(
         (Value::Rect([6.0, 44.0, 6.0, 0.0]), Value::F64(100.0)),
         |header| {
            let padding = get_prop(header, Prop::PADDING)
               .cloned()
               .unwrap_or(Value::Rect([6.0, 44.0, 6.0, 0.0]));
            let height = get_prop(header, Prop::HEIGHT)
               .cloned()
               .unwrap_or(Value::F64(100.0));
            (padding, height)
         },
      );
      if let Some(node) = page.find(PAGE_HEADER) {
         node.set(Prop::PADDING, Some(header_padding));
         node.set(Prop::HEIGHT, Some(header_height.clone()));
      }
      if let Some(node) = page.find(PAGE_HEADER_SPACER) {
         node.set(Prop::HEIGHT, Some(header_height));
      }
      if let Some(back) = page.find(PAGE_BACK) {
         back.set(
            Prop::ACCESSIBILITY_LABEL,
            Some(Value::Str("Back".to_owned())),
         );
         back.press = Some(Handler::PageBack(id));
      }

      let built_sections = sections
         .into_iter()
         .map(|section| self.page_section(id, section))
         .collect::<Vec<_>>();

      if let Some(content) = page.find(PAGE_CONTENT) {
         content.kids = built_sections;
      }

      let mut out = Vec::new();
      let mut all = Vec::new();
      let root = self.emit(page, None, &mut out, &mut all);
      self.slots.push(Slot {
         kind: SlotKind::Page(id),
         anchor: navigator,
         parent: navigator,
         before: Before::End,
         place: Place::Screen,
         roots: vec![root],
         all,
      });
      let ids = self.merged(navigator);
      let index = i32::try_from(ids.len().saturating_sub(1)).unwrap_or(i32::MAX);
      out.push(Op::Children { id: navigator, ids });
      out.push(Op::Update {
         id:    navigator,
         props: vec![(Prop::INDEX, Value::I32(index))],
      });
      out
   }

   fn page_section(&self, id: u32, section: PageSection) -> Draft {
      let layouts = phoenix_ui_mobile::templates();
      let mut root = Draft::from_node(&layouts.section);
      let mut group = Draft::from_node(&layouts.group);
      let (heading, rows) = match section {
         PageSection::Buttons { title, buttons } => {
            let rows = buttons
               .into_iter()
               .map(|btn| self.page_button(id, btn))
               .collect::<Vec<_>>();
            (title, rows)
         },
         PageSection::Code {
            title,
            text,
            monospace,
            highlight,
         } => (title, vec![boxed(code(&text, monospace, highlight))]),
         PageSection::Text { title, text } => {
            let content = Draft {
               kind:  ViewKind::Text,
               props: vec![
                  (Prop::TEXT, Value::Str(text)),
                  (Prop::COLOR, Value::I64(0xFFF6_F6F7)),
                  (Prop::FONT_SIZE, Value::F64(14.0)),
                  (Prop::LINE_HEIGHT, Value::F64(20.0)),
                  (Prop::SELECTABLE, Value::Bool(true)),
               ],
               kids:  Vec::new(),
               press: None,
               src:   None,
            };
            (title, vec![boxed(content)])
         },
      };
      group.kids = rows
         .into_iter()
         .enumerate()
         .flat_map(|(index, row)| {
            let divider = (index > 0).then(|| Draft::from_node(&layouts.divider));
            divider.into_iter().chain([row])
         })
         .collect();
      if let Some(heading) = heading.filter(|h| !h.is_empty()) {
         let mut header = Draft::from_node(&layouts.header);
         header.set(Prop::TEXT, Some(Value::Str(heading)));
         root.kids.push(header);
      }
      root.kids.push(group);
      root
   }

   fn page_button(&self, id: u32, btn: PageButton) -> Draft {
      let mut row_root = Draft::from_node(&phoenix_ui_mobile::templates().button_row);
      for radius in RADII {
         row_root.set(radius, None);
      }
      row_root.set(
         Prop::ACCESSIBILITY_LABEL,
         Some(Value::Str(btn.label.clone())),
      );
      row_root.set(
         Prop::ACCESSIBILITY_ID,
         Some(Value::Str(format!("phoenix-page-{}", btn.label))),
      );
      row_root.press = Some(Handler::PageButton {
         page:  id,
         index: btn.index,
      });
      if let Some(row) = row_root.kids.first_mut() {
         let icon = self.templates.resolve_icon(btn.icon.as_deref()).to_owned();
         if let Some(img) = row.kids.first_mut() {
            img.set(Prop::URI, Some(Value::Str(icon)));
            if btn.danger {
               img.set(Prop::TINT, Some(Value::I64(0xFFF2_3F42)));
            }
         }
         if let Some(label) = row.kids.get_mut(1) {
            label.set(Prop::TEXT, Some(Value::Str(btn.label)));
            if btn.danger {
               label.set(Prop::COLOR, Some(Value::I64(0xFFF2_3F42)));
            }
         }
         row.kids.truncate(2);
      }
      row_root
   }

   fn close_page(&mut self, id: u32) -> Vec<Op> {
      let Some(at) = self
         .slots
         .iter()
         .position(|slot| slot.kind == SlotKind::Page(id))
      else {
         return Vec::new();
      };
      let slot = self.slots.remove(at);
      self.forget_ids(&slot.all);
      let mut out = Vec::new();
      if self.mirror.has_kids(slot.parent) {
         let ids = self.merged(slot.parent);
         let index = i32::try_from(ids.len().saturating_sub(1)).unwrap_or(i32::MAX);
         out.push(Op::Children {
            id: slot.parent,
            ids,
         });
         out.push(Op::Update {
            id:    slot.parent,
            props: vec![(Prop::INDEX, Value::I32(index))],
         });
      }
      out.push(Op::Delete { ids: slot.all });
      out
   }
}

fn code(text: &str, monospace: bool, highlight: bool) -> Draft {
   let mut content = Draft {
      kind:  ViewKind::Text,
      props: vec![
         (Prop::TEXT, Value::Str(text.to_owned())),
         (Prop::COLOR, Value::I64(0xFFF6_F6F7)),
         (Prop::FONT_SIZE, Value::F64(12.0)),
         (Prop::LINE_HEIGHT, Value::F64(17.0)),
         (prop!("fontWeight"), Value::I32(400)),
         (Prop::SELECTABLE, Value::Bool(true)),
      ],
      kids:  Vec::new(),
      press: None,
      src:   None,
   };
   if monospace {
      content.set(Prop::FONT_FAMILY, Some(Value::Str("monospace".to_owned())));
   }
   if highlight && text.len() <= 48 * 1024 {
      let spans = crate::highlight::js(text)
         .into_iter()
         .map(|(kind, run)| {
            let color = match kind {
               crate::highlight::Kind::Plain => 0xFFF6_F6F7,
               crate::highlight::Kind::Comment => 0xFF8B_949E,
               crate::highlight::Kind::String => 0xFF7E_E787,
               crate::highlight::Kind::Keyword => 0xFFFF_7B72,
               crate::highlight::Kind::Number => 0xFF79_C0FF,
            };
            Value::Obj(vec![
               (Prop::TEXT.key(), Value::Str(run.to_owned())),
               (Prop::COLOR.key(), Value::I64(color)),
            ])
         })
         .collect();
      content.set(prop!("spans"), Some(Value::Arr(spans)));
   }
   content
}

fn boxed(content: Draft) -> Draft {
   Draft {
      kind:  ViewKind::View,
      props: vec![
         (prop!("width"), Value::Str("fill".to_owned())),
         (prop!("background"), Value::I64(0xFF14_1518)),
         (Prop::PADDING, Value::Rect([14.0, 12.0, 14.0, 12.0])),
      ],
      kids:  vec![content],
      press: None,
      src:   None,
   }
}

pub fn close(id: u32) -> Vec<Op> {
   let ops = patches().close_page(id);
   if let Some(engine) = phoenix_runtime::engine() {
      page_closed(&engine, id);
   }
   ops
}
