//! Merges config splits' `resources.arsc` (density, language) into base's, so
//! one APK carries every resource. Split type chunks go next to base's chunks
//! of the same type id, with their string values re-pointed into base's global
//! string pool and their keys into base's key pool.

use std::{
   borrow::Cow,
   collections::HashMap,
};

use crate::{
   Error,
   bytes::{
      Bytes,
      le32,
   },
};

const RES_STRING_POOL: u16 = 0x0001;
const RES_TABLE: u16 = 0x0002;
const RES_TABLE_PACKAGE: u16 = 0x0200;
const RES_TABLE_TYPE: u16 = 0x0201;
const RES_TABLE_TYPE_SPEC: u16 = 0x0202;

const TYPE_STRING: u8 = 0x03;
const FLAG_COMPLEX: u16 = 0x01;
const FLAG_COMPACT: u16 = 0x08;
const TYPE_SPARSE: u8 = 0x01;
const TYPE_OFFSET16: u8 = 0x02;
const POOL_UTF8: u32 = 0x100;
const POOL_SORTED: u32 = 0x1;

/// A string pool as its encoded entries, which merging appends to without
/// re-encoding.
pub struct Pool<'a> {
   flags:         u32,
   pub raws:      Vec<Cow<'a, [u8]>>,
   styles:        u32,
   style_offsets: &'a [u8],
   style_data:    &'a [u8],
}

impl<'a> Pool<'a> {
   pub fn parse(chunk: &'a [u8]) -> Result<Self, Error> {
      let bytes = Bytes(chunk);
      let count = bytes.u32(8)? as usize;
      let styles = bytes.u32(12)?;
      let flags = bytes.u32(16)?;
      let strings = bytes.u32(20)? as usize;
      let utf8 = flags & POOL_UTF8 != 0;

      let mut raws = Vec::with_capacity(count);
      for index in 0..count {
         let start = strings + bytes.u32(28 + 4 * index)? as usize;
         let mut at = start;
         if utf8 {
            at += if bytes.u8(at)? & 0x80 != 0 { 2 } else { 1 };
            let mut length = usize::from(bytes.u8(at)?);
            at += 1;
            if length & 0x80 != 0 {
               length = (length & 0x7F) << 8 | usize::from(bytes.u8(at)?);
               at += 1;
            }
            at += length + 1;
         } else {
            let mut length = usize::from(bytes.u16(at)?);
            at += 2;
            if length & 0x8000 != 0 {
               length = (length & 0x7FFF) << 16 | usize::from(bytes.u16(at)?);
               at += 2;
            }
            at += 2 * length + 2;
         }
         raws.push(Cow::Borrowed(bytes.slice(start..at)?));
      }

      let offsets = 28 + 4 * count;
      Ok(Self {
         flags,
         raws,
         styles,
         style_offsets: bytes.slice(offsets..offsets + 4 * styles as usize)?,
         style_data: if styles == 0 {
            &[]
         } else {
            bytes.slice(bytes.u32(24)? as usize..chunk.len())?
         },
      })
   }

   pub const fn utf8(&self) -> bool {
      self.flags & POOL_UTF8 != 0
   }

   pub fn string(&self, index: usize) -> Result<String, Error> {
      let raw = self
         .raws
         .get(index)
         .ok_or_else(|| Error::Resources(format!("string {index} outside the pool")))?;
      decode(raw, self.utf8())
   }

   pub fn push(&mut self, string: &str) -> Result<(), Error> {
      self.raws.push(Cow::Owned(encode(string, self.utf8())?));
      Ok(())
   }

   pub fn build(&self) -> Result<Vec<u8>, Error> {
      let count = self.raws.len();
      let data: usize = self.raws.iter().map(|raw| raw.len()).sum();
      let pad = (4 - data % 4) % 4;
      let strings = 28 + 4 * count + 4 * self.styles as usize;
      let style_start = if self.styles == 0 {
         0
      } else {
         strings + data + pad
      };
      let size = strings + data + pad + self.style_data.len();

      let mut out = Vec::with_capacity(size);
      out.extend_from_slice(&RES_STRING_POOL.to_le_bytes());
      out.extend_from_slice(&28u16.to_le_bytes());
      out.extend_from_slice(&le32(size)?);
      out.extend_from_slice(&le32(count)?);
      out.extend_from_slice(&self.styles.to_le_bytes());
      out.extend_from_slice(&(self.flags & !POOL_SORTED).to_le_bytes());
      out.extend_from_slice(&le32(strings)?);
      out.extend_from_slice(&le32(style_start)?);
      let mut offset = 0;
      for raw in &self.raws {
         out.extend_from_slice(&le32(offset)?);
         offset += raw.len();
      }
      out.extend_from_slice(self.style_offsets);
      for raw in &self.raws {
         out.extend_from_slice(raw);
      }
      out.resize(out.len() + pad, 0);
      out.extend_from_slice(self.style_data);
      Ok(out)
   }
}

fn decode(raw: &[u8], utf8: bool) -> Result<String, Error> {
   let bytes = Bytes(raw);
   if utf8 {
      let mut at = if bytes.u8(0)? & 0x80 != 0 { 2 } else { 1 };
      let mut length = usize::from(bytes.u8(at)?);
      at += 1;
      if length & 0x80 != 0 {
         length = (length & 0x7F) << 8 | usize::from(bytes.u8(at)?);
         at += 1;
      }
      return Ok(String::from_utf8_lossy(bytes.slice(at..at + length)?).into_owned());
   }
   let mut length = usize::from(bytes.u16(0)?);
   let at = if length & 0x8000 == 0 {
      2
   } else {
      length = (length & 0x7FFF) << 16 | usize::from(bytes.u16(2)?);
      4
   };
   let units = bytes
      .slice(at..at + 2 * length)?
      .as_chunks::<2>()
      .0
      .iter()
      .map(|unit| u16::from_le_bytes(*unit))
      .collect::<Vec<u16>>();
   Ok(String::from_utf16_lossy(&units))
}

fn encode(string: &str, utf8: bool) -> Result<Vec<u8>, Error> {
   let too_long = || Error::Resources(format!("string of {} bytes is too long", string.len()));
   let units = string.encode_utf16().count();
   if utf8 {
      let length = |count: usize| -> Result<Vec<u8>, Error> {
         let [high, low] = u16::try_from(count)
            .ok()
            .filter(|&count| count <= 0x7FFF)
            .ok_or_else(too_long)?
            .to_be_bytes();
         Ok(if count > 0x7F {
            vec![0x80 | high, low]
         } else {
            vec![low]
         })
      };
      return Ok([
         length(units)?,
         length(string.len())?,
         string.as_bytes().to_vec(),
         vec![0],
      ]
      .concat());
   }
   let head = if units > 0x7FFF {
      let wide = u32::try_from(units)
         .ok()
         .filter(|&wide| wide <= 0x7FFF_FFFF)
         .ok_or_else(too_long)?;
      let [first, second, third, fourth] = wide.to_be_bytes();
      vec![
         0x8000 | u16::from_be_bytes([first, second]),
         u16::from_be_bytes([third, fourth]),
      ]
   } else {
      vec![u16::try_from(units).map_err(|_| too_long())?]
   };
   Ok(head
      .into_iter()
      .chain(string.encode_utf16())
      .chain([0])
      .flat_map(u16::to_le_bytes)
      .collect())
}

struct Package<'a> {
   id:     u32,
   header: &'a [u8],
   types:  &'a [u8],
   keys:   &'a [u8],
   chunks: Vec<Cow<'a, [u8]>>,
}

struct Table<'a> {
   pool:    &'a [u8],
   package: Package<'a>,
}

fn chunk_at(bytes: Bytes<'_>, at: usize) -> Result<(u16, &[u8]), Error> {
   let size = bytes.u32(at + 4)? as usize;
   if size < 8 {
      return Err(Error::Resources(format!(
         "chunk at {at:#x} has size {size}"
      )));
   }
   Ok((bytes.u16(at)?, bytes.slice(at..at + size)?))
}

fn parse_table(data: &[u8]) -> Result<Table<'_>, Error> {
   let bytes = Bytes(data);
   if bytes.u16(0)? != RES_TABLE {
      return Err(Error::Resources("not a resource table".into()));
   }
   let (mut pool, mut packages) = (None, Vec::new());
   let mut at = usize::from(bytes.u16(2)?);
   while at < data.len() {
      let (kind, chunk) = chunk_at(bytes, at)?;
      match kind {
         RES_STRING_POOL => pool = Some(chunk),
         RES_TABLE_PACKAGE => packages.push(chunk),
         _ => {},
      }
      at += chunk.len();
   }
   let [package] = packages[..] else {
      return Err(Error::Resources(format!(
         "expected one package, found {}",
         packages.len()
      )));
   };
   Ok(Table {
      pool:    pool.ok_or_else(|| Error::Resources("no string pool".into()))?,
      package: parse_package(package)?,
   })
}

fn parse_package(data: &[u8]) -> Result<Package<'_>, Error> {
   let bytes = Bytes(data);
   let types_at = bytes.u32(268)? as usize;
   let keys_at = bytes.u32(276)? as usize;
   let types = bytes.slice(types_at..types_at + bytes.u32(types_at + 4)? as usize)?;
   let keys = bytes.slice(keys_at..keys_at + bytes.u32(keys_at + 4)? as usize)?;

   let mut chunks = Vec::new();
   let mut at = keys_at + keys.len();
   while at < data.len() {
      let (_, chunk) = chunk_at(bytes, at)?;
      chunks.push(Cow::Borrowed(chunk));
      at += chunk.len();
   }
   Ok(Package {
      id: bytes.u32(8)?,
      header: bytes.slice(0..usize::from(bytes.u16(2)?))?,
      types,
      keys,
      chunks,
   })
}

const fn kind(chunk: &[u8]) -> u16 {
   u16::from_le_bytes([chunk[0], chunk[1]])
}

/// Type and spec chunks carry their type id at byte 8.
fn type_id(chunk: &[u8]) -> Option<u8> {
   chunk.get(8).copied()
}

fn is(chunk: &[u8], kinds: &[u16], id: u8) -> bool {
   kinds.contains(&kind(chunk)) && type_id(chunk) == Some(id)
}

/// A copy of type chunk `chunk` with its keys mapped through `keys` and its
/// string values moved up by `strings`.
fn remap_type(chunk: &[u8], keys: &[usize], strings: u32) -> Result<Vec<u8>, Error> {
   let mut out = chunk.to_vec();
   let bytes = Bytes(chunk);
   let header = usize::from(bytes.u16(2)?);
   let flags = bytes.u8(9)?;
   let count = bytes.u32(12)? as usize;
   let entries = bytes.u32(16)? as usize;

   // Deduplicated entries share an offset and must be remapped once.
   let mut offsets = Vec::with_capacity(count);
   for index in 0..count {
      if flags & TYPE_SPARSE != 0 {
         offsets.push(usize::from(bytes.u16(header + 4 * index + 2)?) * 4);
      } else if flags & TYPE_OFFSET16 != 0 {
         let offset = bytes.u16(header + 2 * index)?;
         if offset != 0xFFFF {
            offsets.push(usize::from(offset) * 4);
         }
      } else {
         let offset = bytes.u32(header + 4 * index)?;
         if offset != 0xFFFF_FFFF {
            offsets.push(offset as usize);
         }
      }
   }

   offsets.sort_unstable();
   offsets.dedup();

   let key = |index: usize| {
      keys
         .get(index)
         .copied()
         .ok_or_else(|| Error::Resources(format!("key {index} outside the split's key pool")))
   };
   let set32 = |out: &mut Vec<u8>, at: usize, value: u32| {
      out[at..at + 4].copy_from_slice(&value.to_le_bytes());
   };
   let fix_value = |out: &mut Vec<u8>, at: usize| -> Result<(), Error> {
      if bytes.u8(at + 3)? == TYPE_STRING {
         set32(out, at + 4, bytes.u32(at + 4)?.wrapping_add(strings));
      }
      Ok(())
   };

   for offset in offsets {
      let entry = entries + offset;
      let entry_flags = bytes.u16(entry + 2)?;
      if entry_flags & FLAG_COMPACT != 0 {
         let mapped = u16::try_from(key(usize::from(bytes.u16(entry)?))?)
            .map_err(|_| Error::Resources("compact entry key overflow".into()))?;
         out[entry..entry + 2].copy_from_slice(&mapped.to_le_bytes());
         if (entry_flags >> 8) as u8 == TYPE_STRING {
            set32(
               &mut out,
               entry + 4,
               bytes.u32(entry + 4)?.wrapping_add(strings),
            );
         }
         continue;
      }
      let mapped = le32(key(bytes.u32(entry + 4)? as usize)?)?;
      out[entry + 4..entry + 8].copy_from_slice(&mapped);
      let size = usize::from(bytes.u16(entry)?);
      if entry_flags & FLAG_COMPLEX != 0 {
         for index in 0..bytes.u32(entry + 12)? as usize {
            fix_value(&mut out, entry + size + 12 * index + 4)?;
         }
      } else {
         fix_value(&mut out, entry + size)?;
      }
   }
   Ok(out)
}

/// Merges the split tables into base's and, with `package`, renames the
/// package, which `getIdentifier` looks resources up by.
pub fn merge<'a>(
   base: &'a [u8],
   splits: &'a [Cow<'a, [u8]>],
   package: Option<&str>,
) -> Result<Vec<u8>, Error> {
   let mut merged = Merged::new(base)?;
   for split in splits {
      merged.add(split)?;
   }
   merged.finish(package)
}

/// Base's table, growing as splits merge into it.
struct Merged<'a> {
   id:         u32,
   header:     &'a [u8],
   types:      &'a [u8],
   type_names: Vec<String>,
   pool:       Pool<'a>,
   keys:       Pool<'a>,
   key_index:  HashMap<String, usize>,
   chunks:     Vec<Cow<'a, [u8]>>,
}

fn type_names(types: &[u8]) -> Result<Vec<String>, Error> {
   Pool::parse(types)?
      .raws
      .iter()
      .map(|raw| decode(raw, true))
      .collect()
}

impl<'a> Merged<'a> {
   fn new(base: &'a [u8]) -> Result<Self, Error> {
      let table = parse_table(base)?;
      let keys = Pool::parse(table.package.keys)?;
      let mut key_index = HashMap::new();
      for index in 0..keys.raws.len() {
         key_index.insert(keys.string(index)?, index);
      }
      Ok(Self {
         id: table.package.id,
         header: table.package.header,
         types: table.package.types,
         type_names: type_names(table.package.types)?,
         pool: Pool::parse(table.pool)?,
         keys,
         key_index,
         chunks: table.package.chunks,
      })
   }

   fn add(&mut self, split: &'a [u8]) -> Result<(), Error> {
      let table = parse_table(split)?;
      if table.package.id != self.id {
         return Err(Error::Resources(
            "split package id differs from base".into(),
         ));
      }
      let pool = Pool::parse(table.pool)?;
      if pool.utf8() != self.pool.utf8() {
         return Err(Error::Resources("string pool encodings differ".into()));
      }
      let strings = u32::try_from(self.pool.raws.len()).map_err(|_| Error::Overflow)?;
      self.pool.raws.extend(pool.raws);

      let keys = Pool::parse(table.package.keys)?;
      let mut key_map = Vec::with_capacity(keys.raws.len());
      for (index, raw) in keys.raws.iter().enumerate() {
         let name = keys.string(index)?;
         if let Some(&mapped) = self.key_index.get(&name) {
            key_map.push(mapped);
            continue;
         }
         if keys.utf8() != self.keys.utf8() {
            return Err(Error::Resources("key pool encodings differ".into()));
         }
         self.keys.raws.push(raw.clone());
         self.key_index.insert(name, self.keys.raws.len() - 1);
         key_map.push(self.keys.raws.len() - 1);
      }

      let names = type_names(table.package.types)?;
      for chunk in table.package.chunks {
         match kind(&chunk) {
            RES_TABLE_TYPE_SPEC => {
               let id = type_id(&chunk).ok_or(Error::Truncated)?;
               self.add_spec(chunk, id, &names)?;
            },
            RES_TABLE_TYPE => {
               let id = type_id(&chunk).ok_or(Error::Truncated)?;
               self.add_type(remap_type(&chunk, &key_map, strings)?, id)?;
            },
            _ => {},
         }
      }
      Ok(())
   }

   /// ORs a split's spec flags into base's spec for the type, or adds the spec
   /// when base has none.
   fn add_spec(&mut self, chunk: Cow<'a, [u8]>, id: u8, names: &[String]) -> Result<(), Error> {
      let name = |names: &[String]| names.get(usize::from(id).wrapping_sub(1)).cloned();
      if name(names) != name(&self.type_names) {
         return Err(Error::Resources(format!("type {id} name mismatch")));
      }
      let Some(at) = self.spec(id) else {
         self.chunks.push(chunk);
         return Ok(());
      };
      let mut spec = self.chunks[at].to_vec();
      let (ours, theirs) = (Bytes(&self.chunks[at]), Bytes(&chunk));
      for index in 0..ours.u32(12)?.min(theirs.u32(12)?) as usize {
         let slot = 16 + 4 * index;
         let flags = ours.u32(slot)? | theirs.u32(slot)?;
         spec[slot..slot + 4].copy_from_slice(&flags.to_le_bytes());
      }
      self.chunks[at] = Cow::Owned(spec);
      Ok(())
   }

   /// Puts a type chunk after the last chunk of its type and counts it in the
   /// type's spec.
   fn add_type(&mut self, chunk: Vec<u8>, id: u8) -> Result<(), Error> {
      let at = self
         .chunks
         .iter()
         .rposition(|other| is(other, &[RES_TABLE_TYPE, RES_TABLE_TYPE_SPEC], id))
         .map_or(self.chunks.len(), |last| last + 1);
      self.chunks.insert(at, Cow::Owned(chunk));
      if let Some(spec_at) = self.spec(id) {
         let mut spec = self.chunks[spec_at].to_vec();
         let types = Bytes(&spec).u16(10)?.wrapping_add(1);
         spec[10..12].copy_from_slice(&types.to_le_bytes());
         self.chunks[spec_at] = Cow::Owned(spec);
      }
      Ok(())
   }

   fn spec(&self, id: u8) -> Option<usize> {
      self
         .chunks
         .iter()
         .position(|other| is(other, &[RES_TABLE_TYPE_SPEC], id))
   }

   fn finish(self, package: Option<&str>) -> Result<Vec<u8>, Error> {
      let keys = self.keys.build()?;
      let pool = self.pool.build()?;
      let mut header = self.header.to_vec();
      if header.len() < 280 {
         return Err(Error::Truncated);
      }
      if let Some(package) = package {
         let units = package.encode_utf16().collect::<Vec<u16>>();
         if units.len() > 127 {
            return Err(Error::Resources("package name too long".into()));
         }
         for index in 0..128 {
            let unit = units.get(index).copied().unwrap_or(0);
            header[12 + 2 * index..14 + 2 * index].copy_from_slice(&unit.to_le_bytes());
         }
      }
      let header_len = header.len();
      let package_size = header_len
         + self.types.len()
         + keys.len()
         + self.chunks.iter().map(|chunk| chunk.len()).sum::<usize>();
      header[268..272].copy_from_slice(&le32(header_len)?);
      header[276..280].copy_from_slice(&le32(header_len + self.types.len())?);
      header[4..8].copy_from_slice(&le32(package_size)?);

      let total = 12 + pool.len() + package_size;
      let mut out = Vec::with_capacity(total);
      out.extend_from_slice(&RES_TABLE.to_le_bytes());
      out.extend_from_slice(&12u16.to_le_bytes());
      out.extend_from_slice(&le32(total)?);
      out.extend_from_slice(&1u32.to_le_bytes());
      out.extend_from_slice(&pool);
      out.extend_from_slice(&header);
      out.extend_from_slice(self.types);
      out.extend_from_slice(&keys);
      for chunk in &self.chunks {
         out.extend_from_slice(chunk);
      }
      Ok(out)
   }
}
