use std::sync::atomic::{
   AtomicI64,
   Ordering,
};

use phoenix_ui_mobile::{
   Node,
   Op,
   Prop,
   Props,
   ViewKind,
   mirror::Created,
   prop,
};
use phoenix_wire::{
   Key,
   Value,
   encode_view_event,
};

use crate::{
   bridge,
   js::messages,
   patches::{
      ACTIVE,
      Before,
      Draft,
      FORCE_VALUE,
      Handler,
      Patches,
      Place,
      Sent,
      Slot,
      SlotKind,
      patches,
      worker::{
         Job,
         press_context,
         queue,
      },
   },
};

/// A slash command, as the `/` suggestions list it.
#[derive(Clone)]
pub struct Command {
   pub name:        String,
   pub description: String,
   pub icon:        Option<String>,
   pub plugin:      String,
}

pub fn add_command(command: Command) {
   ACTIVE.store(true, Ordering::Release);
   let mut state = patches();
   state
      .commands
      .retain(|existing| existing.name != command.name);
   state.commands.push(command);
   drop(state);
   queue(Job::Suggest);
}

pub fn remove_command(name: &str) {
   patches().commands.retain(|command| command.name != name);
   queue(Job::Suggest);
}

pub fn commands() -> Vec<Command> {
   patches().commands.clone()
}

/// From the `suggestions` scroll to the list of rows: stack, view, the second
/// view, then four more views down.
const ROWS_PATH: [usize; 7] = [0, 0, 1, 0, 0, 0, 0];
const SEQ: Key = Prop::named("seq").key();
const START: Key = Prop::named("start").key();
const END: Key = Prop::named("end").key();
const ICON_TINT: i64 = 4_289_440_946;

/// The `/` suggestions popup and the plugin rows in it.
#[derive(Default)]
pub struct Suggestions {
   pub scroll: Option<i32>,
   /// The commands and rows node the plugin rows were drawn for.
   key:        String,
   row:        Option<Node>,
}

impl Patches {
   pub fn has_suggestions(&self) -> bool {
      self
         .slots
         .iter()
         .any(|slot| slot.kind == SlotKind::Suggestions)
   }

   fn rows_node(&self) -> Option<i32> {
      let mut node = self.suggestions.scroll?;
      for index in ROWS_PATH {
         node = *self.mirror.kids(node).get(index)?;
      }
      Some(node)
   }

   /// Keeps one of Rust's suggestion rows as the look for plugin rows.
   pub fn learn_suggestion_row(&mut self, created: &Created<'_>) {
      let Some(rows) = self.rows_node() else {
         return;
      };
      let Some(row) = self
         .mirror
         .kids(rows)
         .iter()
         .filter(|id| {
            created
               .get(id)
               .is_some_and(|(kind, _)| **kind == ViewKind::View)
         })
         .find_map(|id| self.mirror.snapshot(*id, created))
         .filter(|row| row.kids.len() == 3 && row.kids[1].kids.len() == 2)
      else {
         return;
      };
      let mut row = row;
      row.props
         .retain(|(key, _)| *key != prop!("background") && *key != Prop::ACCESSIBILITY_LABEL);
      self.suggestions.row = Some(row);
   }

   /// Forgets the plugin rows and returns their node ids.
   pub fn drop_suggestions(&mut self) -> Vec<i32> {
      self.suggestions.scroll = None;
      self.suggestions.key.clear();
      let (gone, kept) = std::mem::take(&mut self.slots)
         .into_iter()
         .partition::<Vec<_>, _>(|slot| slot.kind == SlotKind::Suggestions);
      self.slots = kept;
      let mut ids = Vec::new();
      for slot in gone {
         self.forget_slot(&slot);
         ids.extend(slot.all);
      }
      ids
   }

   pub fn suggested(&mut self) -> Vec<Op> {
      let mut out = Vec::new();
      self.suggest(&mut out);
      out
   }

   pub fn pick(&mut self, input: i32, name: &str) -> Vec<Op> {
      let mut out = vec![force_text(input, &format!("/{name} "))];
      self.mirror.set_composer_text(input, format!("/{name} "));
      self.suggest(&mut out);
      out
   }

   /// Brings the plugin rows in line with the message box. Appends ops to
   /// `out` and returns whether anything changed.
   pub fn suggest(&mut self, out: &mut Vec<Op>) -> bool {
      if self.suggestions.scroll.is_none() {
         return false;
      }
      let rows = self.rows_node();
      let typed = self
         .mirror
         .composer()
         .and_then(|(_, text)| text.strip_prefix('/'))
         .filter(|rest| !rest.contains(char::is_whitespace))
         .map(str::to_lowercase);
      let matching = match (&typed, rows) {
         (Some(typed), Some(_)) => {
            self
               .commands
               .iter()
               .filter(|command| command.name.starts_with(typed.as_str()))
               .map(|command| command.name.clone())
               .collect()
         },
         _ => Vec::new(),
      };
      let key = format!("{}|{rows:?}", matching.join(","));
      if key == self.suggestions.key {
         return false;
      }
      self.suggestions.key = key;

      let mut touched = Vec::new();
      let mut deletes = Vec::new();
      if let Some(at) = self
         .slots
         .iter()
         .position(|slot| slot.kind == SlotKind::Suggestions)
      {
         let slot = self.slots.remove(at);
         self.forget_slot(&slot);
         touched.push(slot.parent);
         deletes.extend(slot.all);
      }
      if let Some(rows) = rows
         && !matching.is_empty()
      {
         let mut roots = Vec::new();
         let mut all = Vec::new();
         for name in &matching {
            let draft = self.suggestion_row(name);
            roots.push(self.emit(draft, None, out, &mut all));
         }
         self.slots.push(Slot {
            kind: SlotKind::Suggestions,
            anchor: rows,
            parent: rows,
            before: Before::Start,
            place: Place::Screen,
            roots,
            all,
         });
         touched.push(rows);
      }
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
      true
   }

   fn suggestion_row(&self, name: &str) -> Draft {
      let command = self.commands.iter().find(|command| command.name == name);
      let description = command.map_or("", |command| command.description.as_str());
      let icon = command
         .and_then(|command| command.icon.as_deref())
         .unwrap_or("res://ic_mana_puzzle_piece_plus");
      let plugin = command.map_or("", |command| command.plugin.as_str());

      let mut row = Draft::from_node(self.suggestions.row.as_ref().unwrap_or(&FALLBACK_ROW));
      row.set(
         Prop::ACCESSIBILITY_LABEL,
         Some(Value::Str(format!("/{name} {description}"))),
      );
      row.set(
         Prop::ACCESSIBILITY_ID,
         Some(Value::Str(format!("phoenix.command.{name}"))),
      );
      if let [image, middle, right] = row.kids.as_mut_slice() {
         image.set(Prop::URI, Some(Value::Str(icon.to_owned())));
         image.set(Prop::TINT, Some(Value::I64(ICON_TINT)));
         image.set(prop!("pool"), None);
         if let [title, subtitle] = middle.kids.as_mut_slice() {
            title.set(Prop::TEXT, Some(Value::Str(format!("/ {name}"))));
            subtitle.set(Prop::TEXT, Some(Value::Str(description.to_owned())));
         }
         right.set(Prop::TEXT, Some(Value::Str(plugin.to_owned())));
      }
      row.press = Some(Handler::Pick(name.to_owned()));
      row
   }
}

/// The view frame for the message box's text becoming `text`.
pub fn change_text(input: i32, text: &str) -> Vec<u8> {
   encode_view_event(
      input,
      Prop::CHANGE_TEXT.key(),
      &Value::Obj(vec![(Prop::TEXT.key(), Value::Str(text.to_owned()))]),
   )
}

/// Sets the message box to `text` with the caret at its end. Rust ignores
/// `value` while the keyboard has edits in flight, and applies `forceValue`
/// whenever its `seq` changes.
pub fn force_text(input: i32, text: &str) -> Op {
   // Counted from 2^40, so it never equals one of Rust's.
   static SEQUENCE: AtomicI64 = AtomicI64::new(1 << 40);

   let end = Value::I32(i32::try_from(text.encode_utf16().count()).unwrap_or(i32::MAX));
   Op::Update {
      id:    input,
      props: vec![(
         FORCE_VALUE,
         Value::Obj(vec![
            (SEQ, Value::I64(SEQUENCE.fetch_add(1, Ordering::Relaxed))),
            (Prop::TEXT.key(), Value::Str(text.to_owned())),
            (START, end.clone()),
            (END, end),
         ]),
      )],
   }
}

/// Runs the commands and send interceptors on `text`, then sends what they
/// left, or nothing.
pub fn submit(input: i32, text: &str, frame: &[u8]) -> Vec<Op> {
   let context = press_context(&Place::Screen);
   let sent = phoenix_runtime::engine().map_or(Sent::Unchanged, |engine| {
      messages::submit(&engine, text, &context)
   });
   if !matches!(sent, Sent::Cancel) {
      patches().resent.push(frame.to_vec());
   }
   let delivered = match &sent {
      Sent::Cancel => bridge::inject(&change_text(input, "")),
      Sent::Text(text) => bridge::inject(&change_text(input, text)) && bridge::inject(frame),
      Sent::Unchanged => bridge::inject(frame),
   };
   if !delivered {
      patches().resent.retain(|sent| sent != frame);
      log::warn!("patches: the message box did not take the send back");
   }
   let Sent::Cancel = sent else {
      return Vec::new();
   };
   let mut state = patches();
   state.mirror.set_composer_text(input, String::new());
   let mut out = vec![force_text(input, "")];
   state.suggest(&mut out);
   out
}

static FALLBACK_ROW: std::sync::LazyLock<Node> = std::sync::LazyLock::new(|| {
   let node = |kind: ViewKind, props: Props, kids: Vec<Node>| {
      Node {
         kind,
         props,
         kids,
         src: None,
      }
   };
   let named = Prop::named;
   let text = |size: f64, weight: i32, line: f64, color: i64, extra: Props| {
      let mut props = vec![
         (Prop::TEXT, Value::Str(String::new())),
         (Prop::COLOR, Value::I64(color)),
         (Prop::FONT_SIZE, Value::F64(size)),
         (named("fontWeight"), Value::I32(weight)),
         (Prop::LINE_HEIGHT, Value::F64(line)),
         (named("lineHeightExact"), Value::Bool(true)),
         (named("maxLines"), Value::I32(1)),
      ];
      props.extend(extra);
      node(ViewKind::Text, props, Vec::new())
   };
   node(
      ViewKind::View,
      vec![
         (named("direction"), Value::Str("row".to_owned())),
         (Prop::PADDING, Value::Rect([16.0, 8.0, 16.0, 8.0])),
         (named("width"), Value::Str("fill".to_owned())),
         (Prop::HEIGHT, Value::F64(48.0)),
         (named("align"), Value::Str("center".to_owned())),
         (Prop::ROLE, Value::Str("button".to_owned())),
         (Prop::ON, Value::Arr(vec![Value::Str("press".to_owned())])),
      ],
      vec![
         node(
            ViewKind::Image,
            vec![
               (Prop::URI, Value::Str(String::new())),
               (Prop::MARGIN, Value::Rect([0.0, 0.0, 16.0, 0.0])),
               (named("width"), Value::F64(32.0)),
               (Prop::HEIGHT, Value::F64(32.0)),
               (Prop::RADIUS, Value::F64(16.0)),
            ],
            Vec::new(),
         ),
         node(
            ViewKind::View,
            vec![
               (named("direction"), Value::Str("column".to_owned())),
               (Prop::HEIGHT, Value::Str("fill".to_owned())),
               (named("minWidth"), Value::F64(0.0)),
               (named("flex"), Value::F64(1.0)),
               (named("justify"), Value::Str("end".to_owned())),
               (named("align"), Value::Str("stretch".to_owned())),
            ],
            vec![
               text(16.0, 600, 20.0, 4_294_704_123, vec![(
                  Prop::HEIGHT,
                  Value::F64(20.0),
               )]),
               text(12.0, 500, 16.0, 4_293_914_609, vec![(
                  Prop::HEIGHT,
                  Value::F64(16.0),
               )]),
            ],
         ),
         text(14.0, 500, 18.0, 4_288_059_294, vec![
            (Prop::PADDING, Value::Rect([16.0, 0.0, 0.0, 0.0])),
            (named("minWidth"), Value::F64(0.0)),
            (named("maxWidth"), Value::Str("40%".to_owned())),
            (named("shrink"), Value::Bool(true)),
         ]),
      ],
   )
});
