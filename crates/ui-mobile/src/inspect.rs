//! JSON for the debug shell. Keys are prop names, or `#<key>` past
//! [`PROPS`](crate::PROPS). Motion words and worklet patches show only their
//! size.

use phoenix_wire::Value;
use serde::{
   Serialize,
   Serializer,
   ser::{
      SerializeMap,
      SerializeSeq,
   },
};

use crate::{
   Prop,
   ViewKind,
   mirror::Mirror,
};

impl Serialize for Prop {
   fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      match self.name() {
         Some(name) => serializer.serialize_str(name),
         None => serializer.collect_str(&format_args!("#{}", self.key().get())),
      }
   }
}

impl Serialize for ViewKind {
   fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      serializer.serialize_str(self.as_str())
   }
}

pub struct WireJson<'a>(pub &'a Value);

impl Serialize for WireJson<'_> {
   fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      match self.0 {
         Value::Null => serializer.serialize_unit(),
         Value::Bool(flag) => serializer.serialize_bool(*flag),
         Value::I32(int) => serializer.serialize_i32(*int),
         Value::I64(int) => serializer.serialize_i64(*int),
         Value::F64(float) => serializer.serialize_f64(*float),
         Value::Str(text) => serializer.serialize_str(text),
         Value::Arr(items) => {
            let mut seq = serializer.serialize_seq(Some(items.len()))?;
            for item in items {
               seq.serialize_element(&WireJson(item))?;
            }
            seq.end()
         },
         Value::Obj(fields) => {
            let mut map = serializer.serialize_map(Some(fields.len()))?;
            for (key, item) in fields {
               map.serialize_entry(&Prop::from(*key), &WireJson(item))?;
            }
            map.end()
         },
         Value::Map(pairs) => {
            let mut map = serializer.serialize_map(Some(pairs.len()))?;
            for (key, item) in pairs {
               map.serialize_entry(key, &WireJson(item))?;
            }
            map.end()
         },
         Value::Rect(rect) => rect.serialize(serializer),
         Value::Words(words) => {
            let mut map = serializer.serialize_map(Some(2))?;
            map.serialize_entry("$words", &words.len())?;
            map.serialize_entry("head", &words[..words.len().min(16)])?;
            map.end()
         },
         Value::WPatch { list, set } => {
            let mut map = serializer.serialize_map(Some(2))?;
            map.serialize_entry("$wpatch", list)?;
            map.serialize_entry("len", &set.len())?;
            map.end()
         },
      }
   }
}

pub(crate) fn value<S: Serializer>(value: &Value, serializer: S) -> Result<S::Ok, S::Error> {
   WireJson(value).serialize(serializer)
}

pub(crate) fn props<S: Serializer>(
   props: &[(Prop, Value)],
   serializer: S,
) -> Result<S::Ok, S::Error> {
   let mut map = serializer.serialize_map(Some(props.len()))?;
   for (prop, item) in props {
      map.serialize_entry(prop, &WireJson(item))?;
   }
   map.end()
}

/// The first words of a motion op, enough to read its commands' layout, and
/// how many it has.
pub(crate) fn count<S: Serializer>(words: &[i32], serializer: S) -> Result<S::Ok, S::Error> {
   let mut map = serializer.serialize_map(Some(2))?;
   map.serialize_entry("count", &words.len())?;
   map.serialize_entry("head", &words[..words.len().min(64)])?;
   map.end()
}

/// The live subtree under `id`, cut off below `depth` levels.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeJson<'a> {
   id:          i32,
   #[serde(skip_serializing_if = "Option::is_none")]
   kind:        Option<&'a ViewKind>,
   #[serde(skip_serializing_if = "Option::is_none")]
   aid:         Option<&'a str>,
   #[serde(skip_serializing_if = "<[_]>::is_empty", serialize_with = "props")]
   props:       &'a [(Prop, Value)],
   #[serde(skip_serializing_if = "Option::is_none")]
   child_count: Option<usize>,
   #[serde(skip_serializing_if = "Vec::is_empty")]
   children:    Vec<Self>,
}

impl<'a> TreeJson<'a> {
   #[must_use]
   pub fn new(mirror: &'a Mirror, id: i32, depth: usize) -> Self {
      let kids = mirror.kids(id);
      let (child_count, children) = if depth == 0 && !kids.is_empty() {
         (Some(kids.len()), Vec::new())
      } else {
         let below = depth.saturating_sub(1);
         (
            None,
            kids
               .iter()
               .map(|kid| Self::new(mirror, *kid, below))
               .collect(),
         )
      };
      Self {
         id,
         kind: mirror.kind(id),
         aid: mirror.accessibility_id(id),
         props: mirror.props(id).map_or(&[], Vec::as_slice),
         child_count,
         children,
      }
   }
}
