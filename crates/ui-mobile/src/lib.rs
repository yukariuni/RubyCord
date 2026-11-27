//! Typed codec for Phoenix UI-op batches (`GuestHost.applyOps`), plus
//! Phoenix's own settings layouts.
//!
//! A batch is a version byte followed by ops until the end of the buffer. Op
//! values use the bridge's tagged format from `phoenix_wire`.

use std::{
   fmt,
   str::FromStr,
   sync::OnceLock,
};

use phoenix_wire::{
   DecodeError as WireError,
   Key,
   Reader,
   Value,
   put_i32_len,
   put_ints,
   put_u16_len,
   put_value,
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::motion::Worklet;

pub mod inspect;
pub mod mirror;
pub mod motion;

/// The prop and event name table (`lh/c2.a`, versionCode 356207). The index is
/// the wire key.
pub const PROPS: [&str; 855] = include!("props.in");

const VERSION: u8 = 1;

/// [`Prop::named`] evaluated at compile time, so a name missing from
/// [`PROPS`] fails the build instead of panicking at runtime.
#[macro_export]
macro_rules! prop {
   ($name:literal) => {
      const { $crate::Prop::named($name) }
   };
}

/// A prop or event key. Keys past [`PROPS`] decode from the wire and have no
/// name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Prop(u16);

impl Prop {
   pub const ACCESSIBILITY_CHECKED: Self = Self::named("accessibilityChecked");
   pub const ACCESSIBILITY_ID: Self = Self::named("accessibilityId");
   pub const ACCESSIBILITY_LABEL: Self = Self::named("accessibilityLabel");
   pub const CHANGE_TEXT: Self = Self::named("changeText");
   pub const COLOR: Self = Self::named("color");
   pub const FONT_FAMILY: Self = Self::named("fontFamily");
   pub const FONT_SIZE: Self = Self::named("fontSize");
   pub const GAP: Self = Self::named("gap");
   pub const HEIGHT: Self = Self::named("height");
   pub const INDEX: Self = Self::named("index");
   pub const KEYBOARD: Self = Self::named("keyboard");
   pub const LINE_HEIGHT: Self = Self::named("lineHeight");
   pub const MARGIN: Self = Self::named("margin");
   pub const NAVIGATE: Self = Self::named("navigate");
   pub const ON: Self = Self::named("on");
   pub const PADDING: Self = Self::named("padding");
   pub const PANE_TITLE: Self = Self::named("paneTitle");
   pub const PLACEHOLDER: Self = Self::named("placeholder");
   pub const PRESENTATION: Self = Self::named("presentation");
   pub const PRESS: Self = Self::named("press");
   pub const RADIUS: Self = Self::named("radius");
   pub const RADIUS_BOTTOM_LEFT: Self = Self::named("radiusBottomLeft");
   pub const RADIUS_BOTTOM_RIGHT: Self = Self::named("radiusBottomRight");
   pub const RADIUS_TOP_LEFT: Self = Self::named("radiusTopLeft");
   pub const RADIUS_TOP_RIGHT: Self = Self::named("radiusTopRight");
   pub const RETURN_KEY: Self = Self::named("returnKey");
   pub const ROLE: Self = Self::named("role");
   pub const SECURE: Self = Self::named("secure");
   pub const SELECTABLE: Self = Self::named("selectable");
   pub const STACK_HEADER: Self = Self::named("stackHeader");
   pub const SUBMIT: Self = Self::named("submit");
   pub const TEXT: Self = Self::named("text");
   pub const TINT: Self = Self::named("tint");
   pub const UIKIT_PUSH: Self = Self::named("uikitPush");
   pub const URI: Self = Self::named("uri");
   pub const VALUE: Self = Self::named("value");

   /// Looks up `name` in [`PROPS`]. A missing name fails const evaluation.
   #[must_use]
   pub const fn named(name: &str) -> Self {
      let mut key = 0;
      while key < PROPS.len() {
         if bytes_eq(PROPS[key].as_bytes(), name.as_bytes()) {
            #[expect(clippy::cast_possible_truncation, reason = "PROPS has 855 entries")]
            return Self(key as u16);
         }
         key += 1;
      }
      panic!("name is not in PROPS");
   }

   #[must_use]
   pub const fn key(self) -> Key {
      Key::new(self.0)
   }

   #[must_use]
   pub fn name(self) -> Option<&'static str> {
      PROPS.get(usize::from(self.0)).copied()
   }
}

const fn bytes_eq(left: &[u8], right: &[u8]) -> bool {
   if left.len() != right.len() {
      return false;
   }

   let mut idx = 0;
   while idx < left.len() {
      if left[idx] != right[idx] {
         return false;
      }
      idx += 1;
   }
   true
}

#[derive(Debug, flub::Error)]
#[error("unknown prop {0:?}")]
pub struct UnknownProp(pub String);

impl FromStr for Prop {
   type Err = UnknownProp;

   fn from_str(name: &str) -> Result<Self, Self::Err> {
      PROPS
         .iter()
         .position(|known| *known == name)
         .and_then(|key| u16::try_from(key).ok())
         .map(Self)
         .ok_or_else(|| UnknownProp(name.to_owned()))
   }
}

impl From<Key> for Prop {
   fn from(key: Key) -> Self {
      Self(key.get())
   }
}

impl fmt::Display for Prop {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self.name() {
         Some(name) => formatter.write_str(name),
         None => write!(formatter, "{}", self.0),
      }
   }
}

pub type Props = Vec<(Prop, Value)>;

macro_rules! view_kinds {
   ($($variant:ident = $name:literal,)*) => {
      /// The element type a `Create` names: the host's view kinds as of
      /// versionCode 356207. Names a later build adds pass through as `Other`.
      #[derive(Clone, Debug, PartialEq, Eq)]
      #[non_exhaustive]
      pub enum ViewKind {
         $($variant,)*
         Other(String),
      }

      impl ViewKind {
         #[must_use]
         pub fn as_str(&self) -> &str {
            match self {
               $(Self::$variant => $name,)*
               Self::Other(name) => name,
            }
         }
      }

      impl From<String> for ViewKind {
         fn from(name: String) -> Self {
            match name.as_str() {
               $($name => Self::$variant,)*
               _ => Self::Other(name),
            }
         }
      }
   };
}

view_kinds! {
   SwipeFill = "swipeFill",
   Canvas = "canvas",
   View = "view",
   Text = "text",
   Stack = "stack",
   Image = "image",
   List = "list",
   Scroll = "scroll",
   HScroll = "hscroll",
   Flow = "flow",
   Navigator = "navigator",
   Input = "input",
   Sheet = "sheet",
   Modal = "modal",
   Banner = "banner",
   Popover = "popover",
   SystemOverlay = "systemOverlay",
   Player = "player",
   Lottie = "lottie",
   Rive = "rive",
   Video = "video",
   WebFrame = "webframe",
   Zoom = "zoom",
   Ring = "ring",
   Spinner = "spinner",
   DatePicker = "datePicker",
   Markup = "markup",
   Slider = "slider",
   VectorAnim = "vector_anim",
}

/// Serializes for the debug shell, with motion words as their count.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Op {
   Create {
      id:    i32,
      kind:  ViewKind,
      #[serde(serialize_with = "inspect::props")]
      props: Props,
   },
   Update {
      id:    i32,
      #[serde(serialize_with = "inspect::props")]
      props: Props,
   },
   Children {
      id:  i32,
      ids: Vec<i32>,
   },
   Delete {
      ids: Vec<i32>,
   },
   Root {
      id: i32,
   },
   Focus {
      id: Option<i32>,
   },
   ScrollTo {
      id:    i32,
      #[serde(serialize_with = "inspect::value")]
      value: Value,
   },
   RequestFrame,
   Motion {
      #[serde(serialize_with = "inspect::count")]
      words: Vec<i32>,
   },
   ChildEdits {
      id:    i32,
      edits: Vec<ChildEdit>,
   },
}

/// One cursor step over the parent's cached child list: copy `keep`, skip
/// `del`, append `ins`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChildEdit {
   pub keep: i32,
   pub del:  i32,
   pub ins:  Vec<i32>,
}

#[derive(Debug, flub::Error)]
pub enum DecodeError {
   #[error("view wire version {0}, expected 1")]
   Version(u8),
   #[error("unknown op {op} at byte {at}")]
   UnknownOp { at: usize, op: u8 },
   #[error("bad value in an op")]
   Wire(#[from] WireError),
}

fn read_props(reader: &mut Reader<'_>) -> Result<Props, WireError> {
   (0..reader.u16()?)
      .map(|_| Ok((Prop(reader.u16()?), reader.value()?)))
      .collect()
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Op>, DecodeError> {
   let mut reader = Reader::new(bytes);
   let version = reader.u8()?;
   if version != VERSION {
      return Err(DecodeError::Version(version));
   }

   let mut ops = Vec::new();
   while !reader.at_end() {
      let at = reader.pos();
      let op = match reader.u8()? {
         1 => {
            let id = reader.i32()?;
            let len = reader.u16()?;
            let kind = reader.str(usize::from(len))?;
            Op::Create {
               id,
               kind: kind.into(),
               props: read_props(&mut reader)?,
            }
         },
         2 => {
            Op::Update {
               id:    reader.i32()?,
               props: read_props(&mut reader)?,
            }
         },
         3 => {
            Op::Children {
               id:  reader.i32()?,
               ids: reader.ints()?,
            }
         },
         4 => {
            Op::Delete {
               ids: reader.ints()?,
            }
         },
         5 => Op::Root { id: reader.i32()? },
         6 => {
            Op::Focus {
               id: if reader.u8()? == 0 {
                  None
               } else {
                  Some(reader.i32()?)
               },
            }
         },
         7 => {
            Op::ScrollTo {
               id:    reader.i32()?,
               value: reader.value()?,
            }
         },
         8 => Op::RequestFrame,
         9 => {
            Op::Motion {
               words: reader.ints()?,
            }
         },
         10 => {
            let id = reader.i32()?;
            let edits = (0..reader.count()?)
               .map(|_| {
                  Ok(ChildEdit {
                     keep: reader.i32()?,
                     del:  reader.i32()?,
                     ins:  reader.ints()?,
                  })
               })
               .collect::<Result<_, WireError>>()?;

            Op::ChildEdits { id, edits }
         },
         op => return Err(DecodeError::UnknownOp { at, op }),
      };
      ops.push(op);
   }
   Ok(ops)
}

fn put_props(out: &mut Vec<u8>, props: &[(Prop, Value)]) {
   put_u16_len(out, props.len());
   for (prop, value) in props {
      out.extend_from_slice(&prop.0.to_le_bytes());
      put_value(out, value);
   }
}

#[must_use]
pub fn encode(ops: &[Op]) -> Vec<u8> {
   let mut out = vec![VERSION];
   for op in ops {
      match op {
         Op::Create { id, kind, props } => {
            out.push(1);
            out.extend_from_slice(&id.to_le_bytes());
            put_u16_len(&mut out, kind.as_str().len());
            out.extend_from_slice(kind.as_str().as_bytes());
            put_props(&mut out, props);
         },
         Op::Update { id, props } => {
            out.push(2);
            out.extend_from_slice(&id.to_le_bytes());
            put_props(&mut out, props);
         },
         Op::Children { id, ids } => {
            out.push(3);
            out.extend_from_slice(&id.to_le_bytes());
            put_ints(&mut out, ids);
         },
         Op::Delete { ids } => {
            out.push(4);
            put_ints(&mut out, ids);
         },
         Op::Root { id } => {
            out.push(5);
            out.extend_from_slice(&id.to_le_bytes());
         },
         Op::Focus { id: None } => out.extend_from_slice(&[6, 0]),
         Op::Focus { id: Some(id) } => {
            out.extend_from_slice(&[6, 1]);
            out.extend_from_slice(&id.to_le_bytes());
         },
         Op::ScrollTo { id, value } => {
            out.push(7);
            out.extend_from_slice(&id.to_le_bytes());
            put_value(&mut out, value);
         },
         Op::RequestFrame => out.push(8),
         Op::Motion { words } => {
            out.push(9);
            put_ints(&mut out, words);
         },
         Op::ChildEdits { id, edits } => {
            out.push(10);
            out.extend_from_slice(&id.to_le_bytes());
            put_i32_len(&mut out, edits.len());
            for edit in edits {
               out.extend_from_slice(&edit.keep.to_le_bytes());
               out.extend_from_slice(&edit.del.to_le_bytes());
               put_ints(&mut out, &edit.ins);
            }
         },
      }
   }
   out
}

#[must_use]
pub fn get_prop(props: &Props, key: Prop) -> Option<&Value> {
   props
      .iter()
      .find(|(existing, _)| *existing == key)
      .map(|(_, value)| value)
}

/// Sets `key` to `value` in `props`, or removes it when `value` is `None`.
pub fn set_prop(props: &mut Props, key: Prop, value: Option<Value>) {
   let at = props.iter().position(|(existing, _)| *existing == key);
   match (at, value) {
      (Some(index), Some(value)) => props[index].1 = value,
      (Some(index), None) => {
         props.remove(index);
      },
      (None, Some(value)) => props.push((key, value)),
      (None, None) => {},
   }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
   pub kind:  ViewKind,
   pub props: Props,
   pub kids:  Vec<Self>,
   pub src:   Option<u32>,
}

/// Phoenix's own settings layouts, captured from versionCode 356207, and the
/// message menu's sheet down to its groups, captured from 357200.
#[derive(Debug)]
pub struct Templates {
   pub section:       Node,
   pub header:        Node,
   pub group:         Node,
   pub switch_row:    Node,
   pub divider:       Node,
   pub button_row:    Node,
   pub value_row:     Node,
   pub input:         Node,
   pub page:          Node,
   pub sheet:         Node,
   pub track_off:     Props,
   pub knob_off:      Props,
   pub track_worklet: Worklet,
   pub knob_worklet:  Worklet,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawTemplates {
   section:       RawNode,
   header:        RawNode,
   group:         RawNode,
   switch_row:    RawNode,
   divider:       RawNode,
   button_row:    RawNode,
   value_row:     RawNode,
   input:         RawNode,
   page:          RawNode,
   sheet:         RawNode,
   track_off:     Vec<RawEntry>,
   knob_off:      Vec<RawEntry>,
   track_worklet: Vec<i32>,
   knob_worklet:  Vec<i32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
   #[serde(rename = "type")]
   kind:  String,
   props: Vec<RawEntry>,
   kids:  Vec<Self>,
   src:   Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
   k: String,
   v: RawValue,
}

#[derive(Deserialize)]
struct RawPatch {
   list: i32,
   set:  Vec<(u16, i32)>,
}

#[derive(Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
enum RawValue {
   Null,
   False,
   True,
   I32(i32),
   I64(String),
   F64(f64),
   Str(String),
   Arr(Vec<Self>),
   Obj(Vec<RawEntry>),
   Rect([f32; 4]),
   Map(Vec<RawEntry>),
   Words(Vec<i32>),
   Wpatch(RawPatch),
}

impl From<RawValue> for Value {
   fn from(raw: RawValue) -> Self {
      match raw {
         RawValue::Null => Self::Null,
         RawValue::False => Self::Bool(false),
         RawValue::True => Self::Bool(true),
         RawValue::I32(int) => Self::I32(int),
         RawValue::I64(text) => {
            Self::I64(
               text
                  .parse()
                  .unwrap_or_else(|err| panic!("templates.json: i64 {text:?}: {err}")),
            )
         },
         RawValue::F64(float) => Self::F64(float),
         RawValue::Str(text) => Self::Str(text),
         RawValue::Arr(items) => Self::Arr(items.into_iter().map(Self::from).collect()),
         RawValue::Obj(entries) => {
            Self::Obj(
               props(entries)
                  .into_iter()
                  .map(|(prop, value)| (prop.key(), value))
                  .collect(),
            )
         },
         RawValue::Rect(rect) => Self::Rect(rect),
         RawValue::Map(entries) => {
            Self::Map(
               entries
                  .into_iter()
                  .map(|entry| (entry.k, entry.v.into()))
                  .collect(),
            )
         },
         RawValue::Words(words) => Self::Words(words),
         RawValue::Wpatch(patch) => {
            Self::WPatch {
               list: patch.list,
               set:  patch.set,
            }
         },
      }
   }
}

fn props(entries: Vec<RawEntry>) -> Props {
   entries
      .into_iter()
      .map(|entry| {
         let prop = entry
            .k
            .parse()
            .unwrap_or_else(|err| panic!("templates.json: {err}"));
         (prop, entry.v.into())
      })
      .collect()
}

impl From<RawNode> for Node {
   fn from(raw: RawNode) -> Self {
      Self {
         kind:  raw.kind.into(),
         props: props(raw.props),
         kids:  raw.kids.into_iter().map(Self::from).collect(),
         src:   raw.src,
      }
   }
}

/// View props that trigger native haptic feedback on mobile views.
const HAPTIC_PROPS: [Prop; 6] = [
   prop!("haptic"),
   prop!("haptics"),
   prop!("longPressHaptic"),
   prop!("startHaptic"),
   prop!("stepHaptic"),
   prop!("dropHaptic"),
];

/// Suppresses haptic props in `ops`.
///
/// Boolean `true` becomes `false` so native defaults stay off without changing
/// the wire type, and non-boolean haptic values are removed. Returns whether
/// any op changed.
pub fn strip_haptic_props(ops: &mut [Op]) -> bool {
   let mut changed = false;
   for op in ops {
      let (Op::Create { props, .. } | Op::Update { props, .. }) = op else {
         continue;
      };
      props.retain_mut(|(key, value)| {
         if !HAPTIC_PROPS.contains(key) {
            return true;
         }
         match value {
            Value::Bool(false) | Value::Null => true,
            Value::Bool(true) => {
               *value = Value::Bool(false);
               changed = true;
               true
            },
            _ => {
               changed = true;
               false
            },
         }
      });
   }
   changed
}

pub fn templates() -> &'static Templates {
   static TEMPLATES: OnceLock<Templates> = OnceLock::new();
   TEMPLATES.get_or_init(|| {
      let raw = serde_json::from_str::<RawTemplates>(include_str!("templates.json"))
         .unwrap_or_else(|err| panic!("templates.json: {err}"));

      Templates {
         section:       raw.section.into(),
         header:        raw.header.into(),
         group:         raw.group.into(),
         switch_row:    raw.switch_row.into(),
         divider:       raw.divider.into(),
         button_row:    raw.button_row.into(),
         value_row:     raw.value_row.into(),
         input:         raw.input.into(),
         page:          raw.page.into(),
         sheet:         raw.sheet.into(),
         track_off:     props(raw.track_off),
         knob_off:      props(raw.knob_off),
         track_worklet: Worklet::try_from(raw.track_worklet)
            .unwrap_or_else(|err| panic!("templates.json trackWorklet: {err}")),
         knob_worklet:  Worklet::try_from(raw.knob_worklet)
            .unwrap_or_else(|err| panic!("templates.json knobWorklet: {err}")),
      }
   })
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn strip_haptic_props_disables_bool_and_drops_typed_values() {
      let mut ops = vec![
         Op::Create {
            id:    1,
            kind:  ViewKind::View,
            props: vec![
               (Prop::TEXT, Value::Str("hi".into())),
               (prop!("haptic"), Value::Str("light".into())),
               (prop!("haptics"), Value::Bool(true)),
               (prop!("longPressHaptic"), Value::Str("heavy".into())),
            ],
         },
         Op::Update {
            id:    2,
            props: vec![
               (prop!("startHaptic"), Value::Bool(true)),
               (prop!("stepHaptic"), Value::Str("tick".into())),
               (prop!("dropHaptic"), Value::Bool(false)),
            ],
         },
      ];

      assert!(strip_haptic_props(&mut ops));

      let decoded = decode(&encode(&ops)).expect("re-encoded batch decodes");
      assert_eq!(
         decoded,
         vec![
            Op::Create {
               id:    1,
               kind:  ViewKind::View,
               props: vec![
                  (Prop::TEXT, Value::Str("hi".into())),
                  (prop!("haptics"), Value::Bool(false)),
               ],
            },
            Op::Update {
               id:    2,
               props: vec![
                  (prop!("startHaptic"), Value::Bool(false)),
                  (prop!("dropHaptic"), Value::Bool(false)),
               ],
            },
         ]
      );
      // second pass is a no-op so batches without active haptics are not re-encoded
      assert!(!strip_haptic_props(&mut ops));
   }
}
