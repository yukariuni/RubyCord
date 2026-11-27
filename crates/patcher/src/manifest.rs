//! Binary `AndroidManifest.xml` edits for a merged APK. The split attributes
//! go and Play Core's `com.android.vending.splits.required` turns false, so it
//! installs on its own. A rename changes `package`, provider authorities,
//! `com.discord.*` permissions and task affinities so it installs next to the
//! original, and class names stay.

use std::collections::{
   HashMap,
   HashSet,
};

use crate::{
   Error,
   arsc::Pool,
   bytes::{
      Bytes,
      le32,
   },
};

pub const ORIGINAL: &str = "com.discord";
const INSTALL_PACKAGES: &str = "android.permission.REQUEST_INSTALL_PACKAGES";
const SPLITS_REQUIRED: &str = "com.android.vending.splits.required";

const RES_XML: u16 = 0x0003;
const RES_STRING_POOL: u16 = 0x0001;
const RES_XML_RESOURCE_MAP: u16 = 0x0180;
const RES_XML_START_ELEMENT: u16 = 0x0102;
const RES_XML_END_ELEMENT: u16 = 0x0103;

/// `requiredSplitTypes`, `splitTypes` and `isSplitRequired`.
const DROPPED: [u32; 3] = [0x0101_064E, 0x0101_064F, 0x0101_0591];
const ATTR_NAME: u32 = 0x0101_0003;
const ATTR_VALUE: u32 = 0x0101_0024;
const ATTR_LABEL: u32 = 0x0101_0001;
const ATTR_AUTHORITIES: u32 = 0x0101_0018;
const ATTR_TASK_AFFINITY: u32 = 0x0101_0012;

const TYPE_REFERENCE: u8 = 0x01;
const TYPE_STRING: u8 = 0x03;
const TYPE_BOOLEAN: u8 = 0x12;
const NO_STRING: u32 = 0xFFFF_FFFF;

pub struct Patched {
   pub data:             Vec<u8>,
   pub removed:          usize,
   pub flipped:          bool,
   pub renamed:          usize,
   pub added_permission: bool,
}

pub fn patch(source: &[u8], package: &str, label: Option<&str>) -> Result<Patched, Error> {
   let bytes = Bytes(source);
   if bytes.u16(0)? != RES_XML {
      return Err(Error::Manifest("not a binary XML manifest".into()));
   }
   let mut chunks = Vec::new();
   let mut at = usize::from(bytes.u16(2)?);
   while at < source.len() {
      let size = bytes.u32(at + 4)? as usize;
      if size < 8 {
         return Err(Error::Manifest(format!("chunk at {at:#x} has size {size}")));
      }
      bytes.slice(at..at + size)?;
      chunks.push(Chunk { at, size });
      at += size;
   }

   let mut plan = Plan {
      package,
      label,
      ..Plan::default()
   };
   let mut pool: Option<(usize, Pool<'_>)> = None;
   let mut resource_ids = Vec::new();
   for (index, chunk) in chunks.iter().enumerate() {
      match chunk.kind(bytes)? {
         RES_STRING_POOL => pool = Some((index, Pool::parse(chunk.bytes(bytes)?)?)),
         RES_XML_RESOURCE_MAP => {
            resource_ids = (0..(chunk.size - 8) / 4)
               .map(|id| bytes.u32(chunk.at + 8 + 4 * id))
               .collect::<Result<Vec<u32>, Error>>()?;
         },
         RES_XML_START_ELEMENT => {
            let (_, strings) = pool
               .as_ref()
               .ok_or_else(|| Error::Manifest("element before the string pool".into()))?;
            let element = Element::parse(bytes, chunk.at, strings, &resource_ids)?;
            let ends = chunks
               .get(index + 1)
               .is_some_and(|next| next.kind(bytes).ok() == Some(RES_XML_END_ELEMENT));
            plan.visit(&element, index, ends)?;
         },
         _ => {},
      }
   }
   let (pool_index, strings) =
      pool.ok_or_else(|| Error::Manifest("manifest has no string pool".into()))?;
   plan.emit(bytes, &chunks, pool_index, strings)
}

#[derive(Clone, Copy)]
struct Chunk {
   at:   usize,
   size: usize,
}

impl Chunk {
   fn kind(self, bytes: Bytes<'_>) -> Result<u16, Error> {
      bytes.u16(self.at)
   }

   fn bytes(self, bytes: Bytes<'_>) -> Result<&[u8], Error> {
      bytes.slice(self.at..self.at + self.size)
   }
}

/// A start element with its attributes' file offsets.
struct Element<'a> {
   bytes:        Bytes<'a>,
   strings:      &'a Pool<'a>,
   resource_ids: &'a [u32],
   attributes:   Vec<usize>,
   tag:          Option<String>,
}

impl<'a> Element<'a> {
   fn parse(
      bytes: Bytes<'a>,
      chunk: usize,
      strings: &'a Pool<'a>,
      resource_ids: &'a [u32],
   ) -> Result<Self, Error> {
      let mut element = Self {
         bytes,
         strings,
         resource_ids,
         attributes: attributes(bytes, chunk)?,
         tag: None,
      };
      element.tag = element.string(bytes.u32(chunk + 20)?)?;
      Ok(element)
   }

   fn string(&self, id: u32) -> Result<Option<String>, Error> {
      if id == NO_STRING {
         return Ok(None);
      }
      self.strings.string(id as usize).map(Some)
   }

   fn resource(&self, attribute: usize) -> Result<Option<u32>, Error> {
      Ok(self
         .resource_ids
         .get(self.bytes.u32(attribute + 4)? as usize)
         .copied())
   }

   fn value(&self, attribute: usize) -> Result<Option<String>, Error> {
      if self.bytes.u8(attribute + 15)? != TYPE_STRING {
         return Ok(None);
      }
      self.string(self.bytes.u32(attribute + 16)?)
   }

   fn is(&self, tag: &str) -> bool {
      self.tag.as_deref() == Some(tag)
   }

   /// Whether an `android:name` attribute holds `wanted`.
   fn names(&self, wanted: &str) -> Result<bool, Error> {
      for &attribute in &self.attributes {
         if self.resource(attribute)? == Some(ATTR_NAME)
            && self.value(attribute)?.as_deref() == Some(wanted)
         {
            return Ok(true);
         }
      }
      Ok(false)
   }
}

enum Edit {
   String(u32),
   False,
}

/// What the scan decided, applied by [`Plan::emit`]. Attributes are keyed by
/// file offset.
#[derive(Default)]
struct Plan<'a> {
   package:           &'a str,
   label:             Option<&'a str>,
   added:             Vec<String>,
   edits:             HashMap<usize, Edit>,
   drops:             HashSet<usize>,
   app_label:         Option<u32>,
   has_install:       bool,
   sample_permission: Option<usize>,
   flipped:           bool,
   renamed:           usize,
}

impl Plan<'_> {
   fn renaming(&self) -> bool {
      self.package != ORIGINAL
   }

   /// The pool index `string` will have, once the added strings follow the
   /// pool's `count` own.
   fn intern(&mut self, count: usize, string: String) -> Result<u32, Error> {
      let at = self
         .added
         .iter()
         .position(|other| *other == string)
         .unwrap_or_else(|| {
            self.added.push(string);
            self.added.len() - 1
         });
      u32::try_from(count + at).map_err(|_| Error::Overflow)
   }

   fn rename(&self, value: &str) -> String {
      match value.strip_prefix(ORIGINAL) {
         Some("") => self.package.to_owned(),
         Some(rest) if rest.starts_with('.') => format!("{}{rest}", self.package),
         _ => value.to_owned(),
      }
   }

   /// Plans the edits to one start element. `ends` says the next chunk closes
   /// it, which makes a lone `uses-permission` a template for another.
   fn visit(&mut self, element: &Element<'_>, index: usize, ends: bool) -> Result<(), Error> {
      if element.is("uses-permission") {
         self.has_install |= element.names(INSTALL_PACKAGES)?;
         if self.sample_permission.is_none()
            && ends
            && let [only] = element.attributes[..]
            && element.resource(only)? == Some(ATTR_NAME)
         {
            self.sample_permission = Some(index);
         }
      }
      for &attribute in &element.attributes {
         if element
            .resource(attribute)?
            .is_some_and(|id| DROPPED.contains(&id))
         {
            self.drops.insert(attribute);
         }
      }
      if element.is("meta-data") && element.names(SPLITS_REQUIRED)? {
         for &attribute in &element.attributes {
            if element.resource(attribute)? == Some(ATTR_VALUE) {
               self.edits.insert(attribute, Edit::False);
               self.flipped = true;
               break;
            }
         }
      }
      if !self.renaming() && self.label.is_none() {
         return Ok(());
      }
      for &attribute in &element.attributes {
         self.visit_attribute(element, attribute)?;
      }
      Ok(())
   }

   fn visit_attribute(&mut self, element: &Element<'_>, attribute: usize) -> Result<(), Error> {
      let count = element.strings.raws.len();
      let id = element.resource(attribute)?;
      let current = element
         .value(attribute)?
         .filter(|current| !current.is_empty());
      let renaming = self.renaming();
      let replacement = match &current {
         Some(_)
            if renaming
               && element.is("manifest")
               && element
                  .string(element.bytes.u32(attribute + 4)?)?
                  .as_deref()
                  == Some("package") =>
         {
            Some(self.package.to_owned())
         },
         Some(value) if renaming && id == Some(ATTR_AUTHORITIES) => {
            Some(
               value
                  .split(';')
                  .map(|part| self.rename(part))
                  .collect::<Vec<String>>()
                  .join(";"),
            )
         },
         Some(value)
            if renaming
               && id == Some(ATTR_NAME)
               && (element.is("permission") || element.is("uses-permission")) =>
         {
            Some(self.rename(value))
         },
         Some(value) if renaming && id == Some(ATTR_TASK_AFFINITY) => Some(self.rename(value)),
         _ => None,
      };
      if let Some(replacement) = replacement {
         if Some(&replacement) != current.as_ref() {
            let string = self.intern(count, replacement)?;
            self.edits.insert(attribute, Edit::String(string));
            self.renamed += 1;
         }
         return Ok(());
      }

      let Some(label) = self.label.filter(|_| id == Some(ATTR_LABEL)) else {
         return Ok(());
      };
      let is_reference = element.bytes.u8(attribute + 15)? == TYPE_REFERENCE;
      let reference = element.bytes.u32(attribute + 16)?;
      let is_application = element.is("application");
      if is_application && is_reference {
         self.app_label = Some(reference);
      }
      if is_reference && (is_application || Some(reference) == self.app_label) {
         let string = self.intern(count, label.to_owned())?;
         self.edits.insert(attribute, Edit::String(string));
         self.renamed += 1;
      }
      Ok(())
   }

   fn emit(
      mut self,
      bytes: Bytes<'_>,
      chunks: &[Chunk],
      pool_index: usize,
      mut pool: Pool<'_>,
   ) -> Result<Patched, Error> {
      let install = match self.sample_permission {
         Some(sample) if !self.has_install => {
            Some((
               sample,
               self.intern(pool.raws.len(), INSTALL_PACKAGES.to_owned())?,
            ))
         },
         _ => None,
      };
      let has_new_strings = !self.added.is_empty();
      for string in &self.added {
         pool.push(string)?;
      }

      let mut removed = 0;
      let mut out = bytes.slice(0..usize::from(bytes.u16(2)?))?.to_vec();
      for (index, chunk) in chunks.iter().enumerate() {
         let whole = chunk.bytes(bytes)?;
         if index == pool_index {
            if has_new_strings {
               out.extend_from_slice(&pool.build()?);
            } else {
               out.extend_from_slice(whole);
            }
            continue;
         }
         if chunk.kind(bytes)? != RES_XML_START_ELEMENT {
            out.extend_from_slice(whole);
            if let Some((sample, string)) = install
               && index == sample + 1
            {
               let mut copy = chunks[sample].bytes(bytes)?.to_vec();
               let first = 16 + usize::from(Bytes(&copy).u16(24)?);
               Bytes(&copy).slice(first..first + 20)?;
               set_string(&mut copy, first, string);
               out.extend_from_slice(&copy);
               out.extend_from_slice(whole);
            }
            continue;
         }
         removed += self.emit_element(bytes, *chunk, &mut out)?;
      }

      let total = le32(out.len())?;
      out[4..8].copy_from_slice(&total);
      Ok(Patched {
         data: out,
         removed,
         flipped: self.flipped,
         renamed: self.renamed,
         added_permission: install.is_some(),
      })
   }

   /// Writes a start element without its dropped attributes and with its
   /// edits, and returns how many it dropped.
   fn emit_element(
      &self,
      bytes: Bytes<'_>,
      chunk: Chunk,
      out: &mut Vec<u8>,
   ) -> Result<usize, Error> {
      let whole = chunk.bytes(bytes)?;
      let attributes = attributes(bytes, chunk.at)?;
      if !attributes
         .iter()
         .any(|attribute| self.drops.contains(attribute) || self.edits.contains_key(attribute))
      {
         out.extend_from_slice(whole);
         return Ok(0);
      }
      let kept = attributes
         .iter()
         .copied()
         .filter(|attribute| !self.drops.contains(attribute))
         .collect::<Vec<usize>>();

      let width = usize::from(bytes.u16(chunk.at + 26)?);
      let first = chunk.at + 16 + usize::from(bytes.u16(chunk.at + 24)?);
      let mut head = bytes.slice(chunk.at..first)?.to_vec();
      let tail = bytes.slice(first + attributes.len() * width..chunk.at + chunk.size)?;
      let length = le32(head.len() + kept.len() * width + tail.len())?;
      head[4..8].copy_from_slice(&length);
      let kept_count = u16::try_from(kept.len()).map_err(|_| Error::Overflow)?;
      head[28..30].copy_from_slice(&kept_count.to_le_bytes());
      // ResXMLTree_attrExt idIndex, classIndex and styleIndex, 1-based.
      for field in [30, 32, 34] {
         let position = usize::from(Bytes(&head).u16(field)?);
         if position == 0 {
            continue;
         }
         let moved = attributes
            .get(position - 1)
            .and_then(|attribute| kept.iter().position(|other| other == attribute))
            .map_or(0, |at| at + 1);
         let position_kept = u16::try_from(moved).map_err(|_| Error::Overflow)?;
         head[field..field + 2].copy_from_slice(&position_kept.to_le_bytes());
      }

      let start = out.len();
      out.extend_from_slice(&head);
      for &attribute in &kept {
         let at = out.len() - start;
         out.extend_from_slice(bytes.slice(attribute..attribute + width)?);
         let element = &mut out[start..];
         match self.edits.get(&attribute) {
            Some(Edit::String(id)) => set_string(element, at, *id),
            Some(Edit::False) => {
               element[at + 8..at + 12].copy_from_slice(&NO_STRING.to_le_bytes());
               element[at + 15] = TYPE_BOOLEAN;
               element[at + 16..at + 20].copy_from_slice(&0u32.to_le_bytes());
            },
            None => {},
         }
      }
      out.extend_from_slice(tail);
      Ok(attributes.len() - kept.len())
   }
}

fn set_string(out: &mut [u8], at: usize, id: u32) {
   out[at + 8..at + 12].copy_from_slice(&id.to_le_bytes());
   out[at + 12..at + 14].copy_from_slice(&8u16.to_le_bytes());
   out[at + 14] = 0;
   out[at + 15] = TYPE_STRING;
   out[at + 16..at + 20].copy_from_slice(&id.to_le_bytes());
}

/// File offsets of a start element's attributes.
fn attributes(bytes: Bytes<'_>, chunk: usize) -> Result<Vec<usize>, Error> {
   let first = chunk + 16 + usize::from(bytes.u16(chunk + 24)?);
   let width = usize::from(bytes.u16(chunk + 26)?);
   let count = usize::from(bytes.u16(chunk + 28)?);
   if width < 20 {
      return Err(Error::Manifest(format!("attribute width {width}")));
   }
   bytes.slice(first..first + count * width)?;
   Ok((0..count).map(|index| first + index * width).collect())
}
