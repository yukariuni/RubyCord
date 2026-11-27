//! Message author and text colors that plugins ask for.

use phoenix_ui_mobile::{
   Op,
   Prop,
   Props,
   set_prop,
};
use phoenix_wire::Value;

use crate::patches::{
   Patches,
   aids::{
      MessagePart,
      message_part,
   },
};

impl Patches {
   /// Colors the author and text of `message`, or gives them back Phoenix's
   /// colors.
   pub fn set_tint(&mut self, message: &str, tint: Option<i64>, out: &mut Vec<Op>) {
      let old = match tint {
         Some(color) => self.tints.insert(message.to_owned(), color),
         None => self.tints.remove(message),
      };
      if old == tint {
         return;
      }
      for (id, (owner, props)) in &self.texts {
         if *owner != message {
            continue;
         }
         let props = tint.map_or_else(
            || {
               props
                  .iter()
                  .filter(|(key, _)| *key == Prop::COLOR || *key == SPANS)
                  .cloned()
                  .collect()
            },
            |color| recolored(props, color, true),
         );
         out.push(Op::Update { id: *id, props });
      }
   }

   /// The message whose author or content text `id` is.
   pub fn text_message(&self, id: i32) -> Option<String> {
      let own = self.mirror.accessibility_id(id).and_then(|aid| {
         match message_part(aid) {
            Some((MessagePart::Author, message)) => Some(message),
            _ => None,
         }
      });
      let parent = self
         .mirror
         .parent(id)
         .and_then(|parent| self.mirror.accessibility_id(parent))
         .and_then(|aid| {
            match message_part(aid) {
               Some((MessagePart::Content, message)) => Some(message),
               _ => None,
            }
         });
      own.or(parent).map(str::to_owned)
   }

   pub fn tint_of(&self, id: i32) -> Option<i64> {
      self
         .texts
         .get(&id)
         .and_then(|(message, _)| self.tints.get(message))
         .copied()
   }
}

const SPANS: Prop = Prop::named("spans");

/// `props` with their text and every span in `color`. An update (`all` off)
/// only recolors what it sets.
pub fn recolored(props: &Props, color: i64, all: bool) -> Props {
   let mut props = props.clone();
   let key = Prop::COLOR.key();
   for (prop, value) in &mut props {
      if *prop != SPANS {
         continue;
      }
      let Value::Arr(spans) = value else {
         continue;
      };
      for span in spans {
         let Value::Obj(entries) = span else {
            continue;
         };
         match entries.iter_mut().find(|(field, _)| *field == key) {
            Some((_, old)) => *old = Value::I64(color),
            None => entries.push((key, Value::I64(color))),
         }
      }
   }
   if all || props.iter().any(|(prop, _)| *prop == Prop::COLOR) {
      set_prop(&mut props, Prop::COLOR, Some(Value::I64(color)));
   }
   props
}
