//! Bounds-checked little-endian reads, since every input is downloaded.

use std::ops::Range;

use crate::Error;

/// `value` as a little-endian u32, the width APK formats give sizes and
/// offsets.
pub fn le32(value: usize) -> Result<[u8; 4], Error> {
   u32::try_from(value)
      .map(u32::to_le_bytes)
      .map_err(|_| Error::Overflow)
}

#[derive(Clone, Copy)]
pub struct Bytes<'a>(pub &'a [u8]);

impl<'a> Bytes<'a> {
   pub fn slice(self, range: Range<usize>) -> Result<&'a [u8], Error> {
      self.0.get(range).ok_or(Error::Truncated)
   }

   pub fn array<const N: usize>(self, at: usize) -> Result<[u8; N], Error> {
      let end = at.checked_add(N).ok_or(Error::Truncated)?;
      let mut out = [0; N];
      out.copy_from_slice(self.slice(at..end)?);
      Ok(out)
   }

   pub fn u8(self, at: usize) -> Result<u8, Error> {
      self.0.get(at).copied().ok_or(Error::Truncated)
   }

   pub fn u16(self, at: usize) -> Result<u16, Error> {
      self.array(at).map(u16::from_le_bytes)
   }

   pub fn u32(self, at: usize) -> Result<u32, Error> {
      self.array(at).map(u32::from_le_bytes)
   }
}
