//! The encoding Phoenix's WASM guest and Java host share on
//! `GuestHost.invoke` and `WasmRuntime.nativeDispatch`.
//!
//! - [`Value`] is the tagged value format (`yq.f` on the view side, `hh.xa` on
//!   the host side).
//! - [`HostCall`] is a guest call into a host module, like `Networking`.
//! - [`decode_dispatch`] splits what the host sends back into view events and
//!   host module events.
//!
//! All integers are little-endian.

use std::str::Utf8Error;

const MAX_DEPTH: usize = 128;
/// Call, event and batch frames all open with this many header bytes.
const HEADER: usize = 16;
const CALL: u8 = 1;
const EVENT: u8 = 3;
const BATCH: u8 = 4;

/// A record field key. What it names depends on the frame, the view prop table
/// for view events and the host's name table (`hh.jb`) for host calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(u16);

impl Key {
   #[must_use]
   pub const fn new(raw: u16) -> Self {
      Self(raw)
   }

   #[must_use]
   pub const fn get(self) -> u16 {
      self.0
   }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
   Null,
   Bool(bool),
   I32(i32),
   I64(i64),
   F64(f64),
   Str(String),
   Arr(Vec<Self>),
   Obj(Vec<(Key, Self)>),
   /// Padding and margin insets as `[left, top, right, bottom]`.
   Rect([f32; 4]),
   Map(Vec<(String, Self)>),
   Words(Vec<i32>),
   /// Copies list `list` from this batch's words table, sets each `(index,
   /// word)`, and appends the result, so it never moves to another batch.
   WPatch {
      list: i32,
      set:  Vec<(u16, i32)>,
   },
}

impl Value {
   /// The field `key` of an [`Value::Obj`].
   #[must_use]
   pub fn field(&self, key: Key) -> Option<&Self> {
      let Self::Obj(entries) = self else {
         return None;
      };
      entries
         .iter()
         .find(|(existing, _)| *existing == key)
         .map(|(_, value)| value)
   }

   #[must_use]
   pub fn as_str(&self) -> Option<&str> {
      match self {
         Self::Str(text) => Some(text),
         _ => None,
      }
   }

   #[must_use]
   pub const fn as_i32(&self) -> Option<i32> {
      match self {
         Self::I32(value) => Some(*value),
         _ => None,
      }
   }
}

#[derive(Debug, flub::Error)]
pub enum DecodeError {
   #[error("input ends early at byte {at}")]
   Underflow { at: usize },
   #[error("negative count {count} at byte {at}")]
   NegativeCount { at: usize, count: i32 },
   #[error("unknown value tag {tag} at byte {at}")]
   UnknownTag { at: usize, tag: u8 },
   #[error("invalid utf-8 at byte {at}")]
   Utf8 {
      at:     usize,
      #[source]
      source: Utf8Error,
   },
   #[error("nesting deeper than {MAX_DEPTH} at byte {at}")]
   TooDeep { at: usize },
   #[error("frame at byte {at} runs past its batch")]
   FrameOverrun { at: usize },
   #[error("frame kind {kind}, expected {expected}")]
   Kind { kind: u8, expected: u8 },
}

/// A cursor over wire bytes.
pub struct Reader<'buf> {
   bytes: &'buf [u8],
   pos:   usize,
}

impl<'buf> Reader<'buf> {
   #[must_use]
   pub const fn new(bytes: &'buf [u8]) -> Self {
      Self { bytes, pos: 0 }
   }

   #[must_use]
   pub const fn pos(&self) -> usize {
      self.pos
   }

   #[must_use]
   pub const fn at_end(&self) -> bool {
      self.pos >= self.bytes.len()
   }

   pub fn take(&mut self, len: usize) -> Result<&'buf [u8], DecodeError> {
      let slice = self
         .bytes
         .get(self.pos..)
         .and_then(|rest| rest.get(..len))
         .ok_or(DecodeError::Underflow { at: self.pos })?;

      self.pos += len;
      Ok(slice)
   }

   fn chunk<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
      let chunk = self
         .bytes
         .get(self.pos..)
         .and_then(<[u8]>::first_chunk::<N>)
         .ok_or(DecodeError::Underflow { at: self.pos })?;

      self.pos += N;
      Ok(*chunk)
   }

   pub fn u8(&mut self) -> Result<u8, DecodeError> {
      self.chunk().map(u8::from_le_bytes)
   }

   pub fn u16(&mut self) -> Result<u16, DecodeError> {
      self.chunk().map(u16::from_le_bytes)
   }

   pub fn i32(&mut self) -> Result<i32, DecodeError> {
      self.chunk().map(i32::from_le_bytes)
   }

   /// An i32 length or count, which must not be negative.
   pub fn count(&mut self) -> Result<usize, DecodeError> {
      let at = self.pos;
      let count = self.i32()?;
      usize::try_from(count).map_err(|_| DecodeError::NegativeCount { at, count })
   }

   pub fn str(&mut self, len: usize) -> Result<String, DecodeError> {
      let at = self.pos;
      let bytes = self.take(len)?;
      std::str::from_utf8(bytes)
         .map(str::to_owned)
         .map_err(|source| DecodeError::Utf8 { at, source })
   }

   /// A counted list of i32s.
   pub fn ints(&mut self) -> Result<Vec<i32>, DecodeError> {
      (0..self.count()?).map(|_| self.i32()).collect()
   }

   pub fn value(&mut self) -> Result<Value, DecodeError> {
      self.value_at(0)
   }

   fn value_at(&mut self, depth: usize) -> Result<Value, DecodeError> {
      let at = self.pos;
      if depth > MAX_DEPTH {
         return Err(DecodeError::TooDeep { at });
      }

      let value = match self.u8()? {
         0 => Value::Null,
         1 => Value::Bool(false),
         2 => Value::Bool(true),
         3 => Value::I32(self.i32()?),
         4 => Value::I64(self.chunk().map(i64::from_le_bytes)?),
         5 => Value::F64(self.chunk().map(f64::from_le_bytes)?),
         6 => {
            let len = self.count()?;
            Value::Str(self.str(len)?)
         },
         7 => {
            Value::Arr(
               (0..self.count()?)
                  .map(|_| self.value_at(depth + 1))
                  .collect::<Result<_, _>>()?,
            )
         },
         8 => {
            Value::Obj(
               (0..self.count()?)
                  .map(|_| Ok((Key(self.u16()?), self.value_at(depth + 1)?)))
                  .collect::<Result<_, DecodeError>>()?,
            )
         },
         9 => {
            Value::Rect([
               self.chunk().map(f32::from_le_bytes)?,
               self.chunk().map(f32::from_le_bytes)?,
               self.chunk().map(f32::from_le_bytes)?,
               self.chunk().map(f32::from_le_bytes)?,
            ])
         },
         10 => {
            let pairs = (0..self.count()?)
               .map(|_| {
                  let len = self.count()?;
                  Ok((self.str(len)?, self.value_at(depth + 1)?))
               })
               .collect::<Result<_, DecodeError>>()?;

            Value::Map(pairs)
         },
         11 => Value::Words(self.ints()?),
         12 => {
            let list = self.i32()?;
            let set = (0..self.u16()?)
               .map(|_| Ok((self.u16()?, self.i32()?)))
               .collect::<Result<_, DecodeError>>()?;

            Value::WPatch { list, set }
         },
         tag => return Err(DecodeError::UnknownTag { at, tag }),
      };
      Ok(value)
   }
}

pub fn put_i32_len(out: &mut Vec<u8>, len: usize) {
   let wire = i32::try_from(len).expect("length exceeds i32::MAX");
   out.extend_from_slice(&wire.to_le_bytes());
}

pub fn put_u16_len(out: &mut Vec<u8>, len: usize) {
   let wire = u16::try_from(len).expect("length exceeds u16::MAX");
   out.extend_from_slice(&wire.to_le_bytes());
}

pub fn put_str(out: &mut Vec<u8>, text: &str) {
   put_i32_len(out, text.len());
   out.extend_from_slice(text.as_bytes());
}

pub fn put_ints(out: &mut Vec<u8>, ints: &[i32]) {
   put_i32_len(out, ints.len());
   for int in ints {
      out.extend_from_slice(&int.to_le_bytes());
   }
}

pub fn put_value(out: &mut Vec<u8>, value: &Value) {
   match value {
      Value::Null => out.push(0),
      Value::Bool(false) => out.push(1),
      Value::Bool(true) => out.push(2),
      Value::I32(int) => {
         out.push(3);
         out.extend_from_slice(&int.to_le_bytes());
      },
      Value::I64(int) => {
         out.push(4);
         out.extend_from_slice(&int.to_le_bytes());
      },
      Value::F64(float) => {
         out.push(5);
         out.extend_from_slice(&float.to_le_bytes());
      },
      Value::Str(text) => {
         out.push(6);
         put_str(out, text);
      },
      Value::Arr(items) => {
         out.push(7);
         put_i32_len(out, items.len());
         for item in items {
            put_value(out, item);
         }
      },
      Value::Obj(entries) => {
         out.push(8);
         put_i32_len(out, entries.len());
         for (key, item) in entries {
            out.extend_from_slice(&key.0.to_le_bytes());
            put_value(out, item);
         }
      },
      Value::Rect(rect) => {
         out.push(9);
         for side in rect {
            out.extend_from_slice(&side.to_le_bytes());
         }
      },
      Value::Map(pairs) => {
         out.push(10);
         put_i32_len(out, pairs.len());
         for (key, item) in pairs {
            put_str(out, key);
            put_value(out, item);
         }
      },
      Value::Words(words) => {
         out.push(11);
         put_ints(out, words);
      },
      Value::WPatch { list, set } => {
         out.push(12);
         out.extend_from_slice(&list.to_le_bytes());
         put_u16_len(out, set.len());
         for (idx, word) in set {
            out.extend_from_slice(&idx.to_le_bytes());
            out.extend_from_slice(&word.to_le_bytes());
         }
      },
   }
}

/// A guest call into a host module through `GuestHost.invoke`.
///
/// The frame is `01 00 u16 module u16 method u16 0 i32 call u32 argsLen`, the
/// args value, then the raw body. Module and method index the host's name
/// table.
#[derive(Clone, Debug, PartialEq)]
pub struct HostCall {
   pub module: u16,
   pub method: u16,
   pub args:   Value,
   pub body:   Vec<u8>,
}

impl HostCall {
   /// The module and method of a call frame from its first 6 bytes, so other
   /// calls need not be copied out of Java.
   #[must_use]
   pub const fn target(head: &[u8]) -> Option<(u16, u16)> {
      match *head {
         [CALL, _, module_lo, module_hi, method_lo, method_hi, ..] => {
            Some((
               u16::from_le_bytes([module_lo, module_hi]),
               u16::from_le_bytes([method_lo, method_hi]),
            ))
         },
         _ => None,
      }
   }

   pub fn decode(frame: &[u8]) -> Result<Self, DecodeError> {
      let (module, method, args, body) = host_frame(frame, CALL)?;
      Ok(Self {
         module,
         method,
         args,
         body: body.to_vec(),
      })
   }

   #[must_use]
   pub fn encode(&self) -> Vec<u8> {
      let mut args = Vec::new();
      put_value(&mut args, &self.args);
      let mut out = Vec::with_capacity(HEADER + args.len() + self.body.len());
      out.extend_from_slice(&[CALL, 0]);
      out.extend_from_slice(&self.module.to_le_bytes());
      out.extend_from_slice(&self.method.to_le_bytes());
      out.extend_from_slice(&[0; 6]);
      put_i32_len(&mut out, args.len());
      out.extend_from_slice(&args);
      out.extend_from_slice(&self.body);
      out
   }
}

/// The module, method, args and body of a host frame of `kind`. An empty args
/// record reads as [`Value::Null`].
fn host_frame(frame: &[u8], kind: u8) -> Result<(u16, u16, Value, &[u8]), DecodeError> {
   let mut reader = Reader::new(frame);
   let found = reader.u8()?;
   if found != kind {
      return Err(DecodeError::Kind {
         kind:     found,
         expected: kind,
      });
   }
   reader.u8()?;
   let module = reader.u16()?;
   let method = reader.u16()?;
   reader.take(6)?;
   let len = reader.count()?;
   let record = reader.take(len)?;
   let args = if record.is_empty() {
      Value::Null
   } else {
      Reader::new(record).value()?
   };
   Ok((module, method, args, &frame[reader.pos..]))
}

/// One frame of a dispatch call. `offset` and `len` are its byte range in the
/// input.
#[derive(Clone, Debug, PartialEq)]
pub struct Dispatch<'a> {
   pub offset: usize,
   pub len:    usize,
   pub event:  DispatchEvent<'a>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DispatchEvent<'a> {
   View {
      node:    i32,
      event:   Key,
      payload: Value,
   },
   /// A host module's event, like a `Networking` reply.
   Host {
      event: u16,
      args:  Value,
      body:  &'a [u8],
   },
   Other,
}

fn u32_at(bytes: &[u8], pos: usize) -> Option<usize> {
   let chunk = bytes.get(pos..)?.first_chunk::<4>()?;
   usize::try_from(u32::from_le_bytes(*chunk)).ok()
}

/// Splits a `nativeDispatch` call into frames.
///
/// A kind-4 batch (`f7.t.d`) is `04 00 u16 0 u16 0 u16 0 u32 0 u32 hdrLen
/// hdr[hdrLen]` then `{u32 len, frame[len]}*`. A view frame (`yq.d`) is
/// `00 00 00 00 01 01 i32 node u16 event` and an optional tagged payload. A
/// host event frame is laid out like a [`HostCall`] with kind 3 and the event
/// in the method field.
pub fn decode_dispatch(bytes: &[u8]) -> Result<Vec<Dispatch<'_>>, DecodeError> {
   let mut out = Vec::new();
   walk(bytes, 0, bytes.len(), 0, &mut out)?;
   Ok(out)
}

fn walk<'a>(
   bytes: &'a [u8],
   offset: usize,
   len: usize,
   depth: usize,
   out: &mut Vec<Dispatch<'a>>,
) -> Result<(), DecodeError> {
   if depth > MAX_DEPTH {
      return Err(DecodeError::TooDeep { at: offset });
   }

   let end = offset + len;
   let frame = &bytes[offset..end];
   match frame {
      [BATCH, ..] => {
         let header = u32_at(frame, 12).ok_or(DecodeError::Underflow { at: offset })?;
         let mut pos = HEADER.saturating_add(header);
         while pos.saturating_add(4) <= len {
            let inner = u32_at(frame, pos).ok_or(DecodeError::Underflow { at: offset + pos })?;
            let start = pos + 4;
            let stop = start
               .checked_add(inner)
               .filter(|&stop| stop <= len)
               .ok_or(DecodeError::FrameOverrun { at: offset + pos })?;

            walk(bytes, offset + start, inner, depth + 1, out)?;
            pos = stop;
         }
      },
      [0, _, _, _, 1, 1, ..] if len >= 12 => {
         let mut reader = Reader::new(frame);
         reader.take(6)?;
         let node = reader.i32()?;
         let event = Key(reader.u16()?);
         let payload = if reader.at_end() {
            Value::Null
         } else {
            reader.value()?
         };
         out.push(Dispatch {
            offset,
            len,
            event: DispatchEvent::View {
               node,
               event,
               payload,
            },
         });
      },
      [EVENT, ..] => {
         // A malformed host event must not hide the view events beside it.
         let event = host_frame(frame, EVENT)
            .map_or(DispatchEvent::Other, |(_, event, args, body)| {
               DispatchEvent::Host { event, args, body }
            });
         out.push(Dispatch { offset, len, event });
      },
      _ => {
         out.push(Dispatch {
            offset,
            len,
            event: DispatchEvent::Other,
         });
      },
   }
   Ok(())
}

/// The view frame (`yq.d`) Java sends Rust when `event` fires on `node`.
#[must_use]
pub fn encode_view_event(node: i32, event: Key, payload: &Value) -> Vec<u8> {
   let mut out = vec![0, 0, 0, 0, 1, 1];
   out.extend_from_slice(&node.to_le_bytes());
   out.extend_from_slice(&event.get().to_le_bytes());
   put_value(&mut out, payload);
   out
}

/// Wraps `frames` in one kind-4 batch with an empty header.
#[must_use]
pub fn encode_batch(frames: &[Vec<u8>]) -> Vec<u8> {
   let mut out = vec![0; HEADER];
   out[0] = BATCH;
   for frame in frames {
      put_i32_len(&mut out, frame.len());
      out.extend_from_slice(frame);
   }
   out
}

/// Rebuilds a kind-4 batch without the top-level frames at `offsets`. Any other
/// input is one frame at offset 0, so it comes back empty when `offsets` holds
/// 0 and unchanged otherwise.
#[must_use]
pub fn without_frames(bytes: &[u8], offsets: &[usize]) -> Vec<u8> {
   let edits = offsets
      .iter()
      .map(|offset| (*offset, None))
      .collect::<Vec<_>>();
   edit_frames(bytes, &edits)
}

/// Rebuilds a kind-4 batch with the top-level frame at each offset dropped
/// (`None`) or replaced. Any other input is one frame at offset 0.
#[must_use]
pub fn edit_frames(bytes: &[u8], edits: &[(usize, Option<Vec<u8>>)]) -> Vec<u8> {
   let edit = |offset: usize| {
      edits
         .iter()
         .find(|(at, _)| *at == offset)
         .map(|(_, new)| new)
   };
   if bytes.first() != Some(&BATCH) {
      return match edit(0) {
         Some(Some(new)) => new.clone(),
         Some(None) => Vec::new(),
         None => bytes.to_vec(),
      };
   }

   let Some(header) = u32_at(bytes, 12) else {
      return bytes.to_vec();
   };

   let mut pos = HEADER.saturating_add(header);
   let mut out = bytes[..pos.min(bytes.len())].to_vec();
   while let Some(inner) = u32_at(bytes, pos) {
      let stop = (pos + 4).saturating_add(inner);
      match edit(pos + 4) {
         None => out.extend_from_slice(&bytes[pos..stop.min(bytes.len())]),
         Some(None) => {},
         Some(Some(new)) => {
            put_i32_len(&mut out, new.len());
            out.extend_from_slice(new);
         },
      }
      pos = stop;
   }
   out
}
