//! The zip subset APKs use.
//!
//! Unchanged entries keep their compressed bytes. Stored entries are aligned
//! through zero-filled local extra fields, 16 KiB for native libraries so the
//! linker can map them on 16 KiB page kernels.

use std::{
   borrow::Cow,
   io::{
      self,
      Read,
   },
   ops::Range,
};

use flate2::read::DeflateDecoder;

use crate::{
   Error,
   bytes::Bytes,
};

const LOCAL: u32 = 0x0403_4B50;
const CENTRAL: u32 = 0x0201_4B50;
const END: u32 = 0x0605_4B50;
/// 1981-01-01 00:00, so output does not depend on the clock.
const DOS_TIME: u32 = 0x0021_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
   Stored,
   Deflated,
}

impl Method {
   const fn id(self) -> u16 {
      match self {
         Self::Stored => 0,
         Self::Deflated => 8,
      }
   }

   const fn version(self) -> u16 {
      match self {
         Self::Stored => 10,
         Self::Deflated => 20,
      }
   }
}

/// Random access to an archive, so a large one is read in pieces and never
/// held whole, like the 230 MB IPA the site patches in a phone's browser.
pub trait Source {
   fn size(&self) -> usize;
   fn read(&self, range: Range<usize>) -> Result<Cow<'_, [u8]>, Error>;
}

impl Source for [u8] {
   fn size(&self) -> usize {
      self.len()
   }

   fn read(&self, range: Range<usize>) -> Result<Cow<'_, [u8]>, Error> {
      self.get(range).map(Cow::Borrowed).ok_or(Error::Truncated)
   }
}

/// An entry as the central directory lists it, with its compressed bytes as a
/// range of the archive.
pub struct Record {
   pub name:   String,
   pub method: Method,
   pub crc:    u32,
   pub size:   u32,
   pub data:   Range<usize>,
}

impl Record {
   #[must_use]
   pub fn entry<'a>(&self, archive: &'a [u8]) -> Entry<'a> {
      Entry {
         name:   self.name.clone(),
         method: self.method,
         crc:    self.crc,
         size:   self.size,
         raw:    Cow::Borrowed(&archive[self.data.clone()]),
      }
   }

   /// The uncompressed bytes, read from `archive` a mebibyte at a time.
   pub fn contents<S: Source + ?Sized>(&self, archive: &S) -> Result<Vec<u8>, Error> {
      let inflate = |source| {
         Error::Inflate {
            name: self.name.clone(),
            source,
         }
      };
      match self.method {
         Method::Stored => Ok(archive.read(self.data.clone())?.into_owned()),
         Method::Deflated => {
            let mut out = Vec::with_capacity(self.size as usize);
            let chunks = Chunks {
               archive,
               rest: self.data.clone(),
            };
            DeflateDecoder::new_with_buf(chunks, vec![0; CHUNK])
               .read_to_end(&mut out)
               .map_err(inflate)?;
            Ok(out)
         },
      }
   }
}

const CHUNK: usize = 1 << 20;

struct Chunks<'a, S: ?Sized> {
   archive: &'a S,
   rest:    Range<usize>,
}

impl<S: Source + ?Sized> Read for Chunks<'_, S> {
   fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
      let end = self.rest.end.min(self.rest.start + buf.len());
      let chunk = self
         .archive
         .read(self.rest.start..end)
         .map_err(|err| io::Error::other(err.to_string()))?;
      buf[..chunk.len()].copy_from_slice(&chunk);
      self.rest.start = end;
      Ok(chunk.len())
   }
}

pub struct Entry<'a> {
   pub name:   String,
   pub method: Method,
   pub crc:    u32,
   pub size:   u32,
   /// The bytes as they sit in the archive, compressed when `method` says so.
   pub raw:    Cow<'a, [u8]>,
}

impl<'a> Entry<'a> {
   pub fn stored(name: String, data: impl Into<Cow<'a, [u8]>>) -> Result<Self, Error> {
      let raw = data.into();
      Ok(Self {
         name,
         method: Method::Stored,
         crc: crc32fast::hash(&raw),
         size: u32::try_from(raw.len()).map_err(|_| Error::Zip("entry over 4 GiB".into()))?,
         raw,
      })
   }

   pub fn data(&self) -> Result<Cow<'_, [u8]>, Error> {
      match self.method {
         Method::Stored => Ok(Cow::Borrowed(&self.raw)),
         Method::Deflated => {
            let mut out = Vec::with_capacity(self.size as usize);
            DeflateDecoder::new(&*self.raw)
               .read_to_end(&mut out)
               .map_err(|source| {
                  Error::Inflate {
                     name: self.name.clone(),
                     source,
                  }
               })?;
            Ok(Cow::Owned(out))
         },
      }
   }
}

const MAX_COMMENT: usize = 0xFFFF;

pub fn read(archive: &[u8]) -> Result<Vec<Record>, Error> {
   read_from(archive)
}

pub fn read_from<S: Source + ?Sized>(archive: &S) -> Result<Vec<Record>, Error> {
   let size = archive.size();
   let tail = archive.read(size.saturating_sub(22 + MAX_COMMENT)..size)?;
   let tail = Bytes(&tail);
   let end = (0..=tail.0.len().saturating_sub(22))
      .rev()
      .find(|&at| tail.u32(at).is_ok_and(|magic| magic == END))
      .ok_or_else(|| Error::Zip("not a zip, no end of central directory".into()))?;
   let count = tail.u16(end + 10)?;
   let start = tail.u32(end + 16)? as usize;
   let directory = archive.read(start..start.saturating_add(tail.u32(end + 12)? as usize))?;
   let bytes = Bytes(&directory);
   let mut at = 0;

   let mut records = Vec::with_capacity(usize::from(count));
   for _ in 0..count {
      if bytes.u32(at)? != CENTRAL {
         return Err(Error::Zip("bad central directory".into()));
      }
      let name_len = usize::from(bytes.u16(at + 28)?);
      let name = String::from_utf8(bytes.slice(at + 46..at + 46 + name_len)?.to_vec())
         .map_err(|_| Error::Zip("entry name is not UTF-8".into()))?;
      let method = match bytes.u16(at + 10)? {
         0 => Method::Stored,
         8 => Method::Deflated,
         other => return Err(Error::Zip(format!("{name}: unsupported method {other}"))),
      };
      let local = bytes.u32(at + 42)? as usize;
      let header = archive.read(local..local.saturating_add(30))?;
      let header = Bytes(&header);
      if header.u32(0)? != LOCAL {
         return Err(Error::Zip(format!("{name}: bad local header")));
      }
      let start = local + 30 + usize::from(header.u16(26)?) + usize::from(header.u16(28)?);
      let data = start..start.saturating_add(bytes.u32(at + 20)? as usize);
      if data.end > size {
         return Err(Error::Truncated);
      }

      records.push(Record {
         name,
         method,
         crc: bytes.u32(at + 16)?,
         size: bytes.u32(at + 24)?,
         data,
      });
      at += 46 + name_len + usize::from(bytes.u16(at + 30)?) + usize::from(bytes.u16(at + 32)?);
   }
   Ok(records)
}

/// A written archive as parts, entry data borrowed from the inputs, so a
/// 280 MB APK costs no second copy.
pub struct Archive<'a> {
   pub parts:     Vec<Cow<'a, [u8]>>,
   /// The part holding the central directory, followed by the end record.
   pub directory: usize,
}

pub fn write(entries: Vec<Entry<'_>>) -> Result<Archive<'_>, Error> {
   let mut writer = Writer::default();
   let mut parts = Vec::with_capacity(2 * entries.len() + 2);
   for entry in entries {
      let local = writer.local(
         &entry.name,
         entry.method,
         entry.crc,
         entry.size,
         entry.raw.len(),
      )?;
      parts.push(Cow::Owned(local));
      parts.push(entry.raw);
   }

   let directory = parts.len();
   let [central, end] = writer.finish()?;
   parts.push(Cow::Owned(central));
   parts.push(Cow::Owned(end));
   Ok(Archive { parts, directory })
}

/// Lays entries out one after another and collects the central directory, so
/// a caller can hand each entry's bytes on as soon as it has them.
#[derive(Default)]
pub struct Writer {
   central: Vec<u8>,
   offset:  usize,
   count:   usize,
}

impl Writer {
   /// The local header of an entry whose `raw` bytes, compressed by `method`,
   /// come right after it.
   pub fn local(
      &mut self,
      name: &str,
      method: Method,
      crc: u32,
      size: u32,
      raw: usize,
   ) -> Result<Vec<u8>, Error> {
      let too_big = || Error::Zip("zip64 is not supported".into());
      let align = match method {
         Method::Deflated => 1,
         Method::Stored if name.as_bytes().ends_with(b".so") => 16384,
         Method::Stored => 4,
      };
      let pad = (align - (self.offset + 30 + name.len()) % align) % align;
      let compressed = u32::try_from(raw).map_err(|_| too_big())?;
      let name_len = u16::try_from(name.len()).map_err(|_| too_big())?;

      let mut local = Vec::with_capacity(30 + name.len() + pad);
      put32(&mut local, LOCAL);
      put16(&mut local, method.version());
      put16(&mut local, 0);
      put16(&mut local, method.id());
      put32(&mut local, DOS_TIME);
      put32(&mut local, crc);
      put32(&mut local, compressed);
      put32(&mut local, size);
      put16(&mut local, name_len);
      put16(&mut local, u16::try_from(pad).map_err(|_| too_big())?);
      local.extend_from_slice(name.as_bytes());
      local.resize(local.len() + pad, 0);

      let central = &mut self.central;
      put32(central, CENTRAL);
      put16(central, 20);
      put16(central, method.version());
      put16(central, 0);
      put16(central, method.id());
      put32(central, DOS_TIME);
      put32(central, crc);
      put32(central, compressed);
      put32(central, size);
      put16(central, name_len);
      central.extend_from_slice(&[0; 12]);
      put32(central, u32::try_from(self.offset).map_err(|_| too_big())?);
      central.extend_from_slice(name.as_bytes());

      self.offset += local.len() + raw;
      self.count += 1;
      Ok(local)
   }

   pub fn finish(self) -> Result<[Vec<u8>; 2], Error> {
      let too_big = || Error::Zip("zip64 is not supported".into());
      let count = u16::try_from(self.count).map_err(|_| too_big())?;
      let mut end = Vec::with_capacity(22);
      put32(&mut end, END);
      put32(&mut end, 0);
      put16(&mut end, count);
      put16(&mut end, count);
      put32(
         &mut end,
         u32::try_from(self.central.len()).map_err(|_| too_big())?,
      );
      put32(&mut end, u32::try_from(self.offset).map_err(|_| too_big())?);
      put16(&mut end, 0);
      if self
         .offset
         .checked_add(self.central.len())
         .is_none_or(|size| size > u32::MAX as usize)
      {
         return Err(too_big());
      }
      Ok([self.central, end])
   }
}

fn put16(out: &mut Vec<u8>, value: u16) {
   out.extend_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, value: u32) {
   out.extend_from_slice(&value.to_le_bytes());
}
