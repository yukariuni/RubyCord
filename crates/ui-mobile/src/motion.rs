//! Phoenix's motion driver, which runs springs natively on the host so an
//! animation costs one batch instead of one per frame.
//!
//! - A view's `worklet` prop binds its animated channels to numbered slots.
//! - [`Op::Motion`](crate::Op::Motion) words set slots or spring them to a
//!   target.
//! - Channel numbers are the cases of the host's setter, `mh.e0.d`.

use phoenix_wire::Value;

use crate::{
   Prop,
   Props,
   prop,
};

const MAGIC: i32 = -1_792_780_819;
const HEADER: usize = 2;
const ENTRY: usize = 6;
const SET: i32 = 8;
const SPRING: i32 = 2;

#[derive(Debug, flub::Error)]
pub enum WorkletError {
   #[error("worklet does not start with the program magic")]
   Magic,
   #[error("worklet ends inside its channel table")]
   Truncated,
   #[error("worklet binds unknown channel {0}")]
   Channel(i32),
}

/// A view property the host can drive from a slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
   TranslateX,
   TranslateY,
   Scale,
   Rotation,
   Alpha,
   Background,
   BorderColor,
   BorderWidth,
   Height,
   Width,
}

impl TryFrom<i32> for Channel {
   type Error = WorkletError;

   fn try_from(number: i32) -> Result<Self, WorkletError> {
      Ok(match number {
         1 => Self::TranslateX,
         2 => Self::TranslateY,
         3 => Self::Scale,
         6 => Self::Rotation,
         7 => Self::Alpha,
         8 => Self::Background,
         10 => Self::BorderColor,
         16 => Self::BorderWidth,
         18 => Self::Height,
         19 => Self::Width,
         other => return Err(WorkletError::Channel(other)),
      })
   }
}

impl Channel {
   /// The value this channel rests at for a view with `props`.
   fn level(self, props: &Props) -> Level {
      let (key, absent) = match self {
         Self::TranslateX => (prop!("translateX"), 0.0),
         Self::TranslateY => (prop!("translateY"), 0.0),
         Self::Scale => (prop!("scale"), 1.0),
         Self::Rotation => (prop!("rotation"), 0.0),
         Self::Alpha => (prop!("opacity"), 1.0),
         Self::Background => return Level::Argb(color(props, prop!("background"))),
         Self::BorderColor => return Level::Argb(color(props, prop!("borderColor"))),
         Self::BorderWidth => (prop!("borderWidth"), 0.0),
         Self::Height => (prop!("height"), f32::NAN),
         Self::Width => (prop!("width"), f32::NAN),
      };
      Level::Scalar(number(props, key).unwrap_or(absent))
   }
}

#[derive(Clone, Copy)]
enum Level {
   Scalar(f32),
   Argb([f32; 4]),
}

/// Compares bits, since an unset size is NaN and must equal itself.
impl PartialEq for Level {
   fn eq(&self, other: &Self) -> bool {
      match (self, other) {
         (Self::Scalar(left), Self::Scalar(right)) => left.to_bits() == right.to_bits(),
         (Self::Argb(left), Self::Argb(right)) => left.map(f32::to_bits) == right.map(f32::to_bits),
         _ => false,
      }
   }
}

fn lookup(props: &Props, key: Prop) -> Option<&Value> {
   props
      .iter()
      .find(|(existing, _)| *existing == key)
      .map(|(_, value)| value)
}

#[expect(
   clippy::cast_possible_truncation,
   reason = "slots hold f32, and view sizes fit one"
)]
fn number(props: &Props, key: Prop) -> Option<f32> {
   match lookup(props, key)? {
      Value::F64(value) => Some(*value as f32),
      _ => None,
   }
}

fn color(props: &Props, key: Prop) -> [f32; 4] {
   let argb = match lookup(props, key) {
      Some(Value::I32(value)) => value.cast_unsigned(),
      Some(Value::I64(value)) => u32::try_from(*value).unwrap_or_default(),
      _ => 0,
   };
   argb.to_be_bytes().map(f32::from)
}

/// The spring Phoenix gives its own switches.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
   pub mass:      f32,
   pub stiffness: f32,
   pub damping:   f32,
}

impl Spring {
   pub const SWITCH: Self = Self {
      mass:      1.0,
      stiffness: 300.0,
      damping:   27.0,
   };
}

#[derive(Clone, Copy, Debug)]
pub enum Command {
   Set {
      slot:  i32,
      value: f32,
   },
   Spring {
      slot:   i32,
      target: f32,
      spring: Spring,
   },
}

/// The words of an [`Op::Motion`](crate::Op::Motion) running `commands`.
#[must_use]
pub fn words(commands: &[Command]) -> Vec<i32> {
   let bits = |value: f32| value.to_bits().cast_signed();
   commands
      .iter()
      .flat_map(|command| {
         match *command {
            Command::Set { slot, value } => vec![SET, slot, bits(value)],
            // The words around the spring's own are copied from Phoenix's
            // switch toggle, and their meaning is unknown.
            Command::Spring {
               slot,
               target,
               spring,
            } => {
               vec![
                  SPRING,
                  slot,
                  0,
                  7,
                  2,
                  bits(target),
                  bits(spring.mass),
                  bits(spring.stiffness),
                  bits(spring.damping),
                  0,
                  bits(0.01),
                  bits(2.0),
                  0,
               ]
            },
         }
      })
      .collect()
}

/// A worklet program captured from Phoenix, with its slots still unassigned.
#[derive(Debug)]
pub struct Worklet {
   words:    Vec<i32>,
   channels: Vec<(Channel, bool)>,
}

impl TryFrom<Vec<i32>> for Worklet {
   type Error = WorkletError;

   fn try_from(words: Vec<i32>) -> Result<Self, WorkletError> {
      if words.first() != Some(&MAGIC) {
         return Err(WorkletError::Magic);
      }
      let count = words
         .get(1)
         .and_then(|count| usize::try_from(*count).ok())
         .ok_or(WorkletError::Truncated)?;
      if words.len() < HEADER + count * ENTRY {
         return Err(WorkletError::Truncated);
      }
      let channels = words[HEADER..HEADER + count * ENTRY]
         .as_chunks::<ENTRY>()
         .0
         .iter()
         .map(|entry| Ok((Channel::try_from(entry[0])?, entry[1] != 0)))
         .collect::<Result<_, WorkletError>>()?;

      Ok(Self { words, channels })
   }
}

impl Worklet {
   /// How many slots [`Self::bind`] uses.
   #[must_use]
   pub fn slot_count(&self) -> i32 {
      self
         .channels
         .iter()
         .map(|&(_, argb)| if argb { 4 } else { 1 })
         .sum()
   }

   /// The program with its slots numbered from `first`.
   #[must_use]
   pub fn bind(&self, first: i32) -> Bound {
      let mut words = self.words.clone();
      let mut next = first;
      let mut slots = Vec::with_capacity(self.channels.len());
      for (&(channel, argb), entry) in self
         .channels
         .iter()
         .zip(words[HEADER..].as_chunks_mut::<ENTRY>().0)
      {
         let taken = if argb {
            [next, next + 1, next + 2, next + 3]
         } else {
            [next; 4]
         };
         next += if argb { 4 } else { 1 };
         entry[2..].copy_from_slice(&taken);
         slots.push((channel, taken));
      }
      Bound { words, slots }
   }
}

/// A worklet program with its own slots.
pub struct Bound {
   words: Vec<i32>,
   slots: Vec<(Channel, [i32; 4])>,
}

impl Bound {
   /// The `worklet` prop that attaches this program to a view.
   #[must_use]
   pub fn prop(&self) -> (Prop, Value) {
      (prop!("worklet"), Value::Words(self.words.clone()))
   }

   /// Places every channel at its value for `props`.
   #[must_use]
   pub fn rest(&self, props: &Props) -> Vec<Command> {
      self.commands(props, |_| true, |slot, value| Command::Set { slot, value })
   }

   /// Springs each channel whose value differs between `from` and `to`.
   #[must_use]
   pub fn spring(&self, from: &Props, to: &Props, spring: Spring) -> Vec<Command> {
      self.commands(
         to,
         |channel| channel.level(from) != channel.level(to),
         |slot, target| {
            Command::Spring {
               slot,
               target,
               spring,
            }
         },
      )
   }

   /// Springs every channel to where it already rests for `props`. A spring
   /// that a deleted view left running keeps driving its slots, and placing
   /// them does not stop it.
   #[must_use]
   pub fn pin(&self, props: &Props, spring: Spring) -> Vec<Command> {
      self.commands(
         props,
         |_| true,
         |slot, target| {
            Command::Spring {
               slot,
               target,
               spring,
            }
         },
      )
   }

   fn commands(
      &self,
      props: &Props,
      moves: impl Fn(Channel) -> bool,
      command: impl Fn(i32, f32) -> Command,
   ) -> Vec<Command> {
      self
         .slots
         .iter()
         .filter(|(channel, _)| moves(*channel))
         .flat_map(|&(channel, slots)| {
            match channel.level(props) {
               Level::Scalar(value) => vec![command(slots[0], value)],
               Level::Argb(parts) => {
                  slots
                     .into_iter()
                     .zip(parts)
                     .map(|(slot, part)| command(slot, part))
                     .collect()
               },
            }
         })
         .collect()
   }
}
