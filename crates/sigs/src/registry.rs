use std::collections::HashMap;

use phoenix_hook::image::Image;

use crate::GATEWAY_HANDLER_FIELD;

/// Every Phoenix build has this event, so its entry seeds the walk.
const SEED: &str = "MESSAGE_CREATE";
/// Seeds the walk over the session/domain reducers (`READY_SUPPLEMENTAL`,
/// `RESUMED`, etc.) whose slice sits separately from the main registry or
/// domain table.
const DOMAIN_SEED: &str = "READY_SUPPLEMENTAL";
/// An event every client handles. macOS Phoenix keeps it, with others, in a
/// flat table apart from the domain tables, which is walked only when the
/// walks above miss it.
const SPLIT_SEED: &str = "MESSAGE_UPDATE";
const STRIDE: usize = 3 * size_of::<usize>();
/// Stride of a `libdiscord` domain reducer descriptor:
/// `{domain_name_ptr, domain_name_len, domain_id, slice_ptr, slice_len}`.
const DOMAIN_STRIDE: usize = 5 * size_of::<usize>();
const MAX_NAME: usize = 64;

#[derive(Debug, flub::Error)]
pub enum Error {
   #[error("no gateway table holds {seed}")]
   NotFound { seed: &'static str },
   #[error("{} gateway tables hold {seed}", .slots.len())]
   Ambiguous {
      seed:  &'static str,
      slots: Vec<usize>,
   },
}

/// A gateway event that exists in the loaded target. Only
/// [`GatewayRegistry`] makes these, so holding one proves the event is live.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EventId(usize);

impl EventId {
   /// Position in [`GatewayRegistry::iter`] order.
   #[must_use]
   pub const fn index(self) -> usize {
      self.0
   }
}

#[derive(Clone, Debug)]
pub struct GatewayEntry {
   pub name:    String,
   /// All registry table slots holding this event's `{name_ptr, name_len,
   /// handler}` record. Android has one slot per event; desktop builds can
   /// emit duplicate copies of the static domain table across codegen units.
   pub slots:   Vec<usize>,
   pub handler: usize,
}

/// The target's gateway registry, walked from [`SEED`].
///
/// The walk crosses either a flat `{name_ptr, name_len, handler}` table
/// (Android) or `libdiscord`'s 40-byte per-domain descriptor table (Desktop),
/// plus any separate [`DOMAIN_SEED`] slice (`READY_SUPPLEMENTAL`, etc.).
#[derive(Default)]
pub struct GatewayRegistry {
   entries: Vec<GatewayEntry>,
   by_name: HashMap<String, EventId>,
}

impl GatewayRegistry {
   pub fn locate(image: &Image<'_>) -> Result<Self, Error> {
      let mut entries = locate_primary(image)?;
      match walk_flat(image, DOMAIN_SEED) {
         Ok(domain) => merge_entries(&mut entries, domain),
         Err(Error::NotFound { .. }) => {},
         Err(err) => return Err(err),
      }

      if !entries.iter().any(|entry| entry.name == SPLIT_SEED) {
         for split in walk_split(image) {
            // macOS dispatches MESSAGE_CREATE through the split table, whose
            // handler differs from the domain table's, which never runs.
            entries.retain(|known| {
               !split
                  .iter()
                  .any(|entry| entry.name == known.name && entry.handler != known.handler)
            });
            merge_entries(&mut entries, split);
         }
      }

      let by_name = entries
         .iter()
         .enumerate()
         .map(|(index, entry)| (entry.name.clone(), EventId(index)))
         .collect();

      Ok(Self { entries, by_name })
   }

   #[must_use]
   pub fn id(&self, name: &str) -> Option<EventId> {
      self.by_name.get(name).copied()
   }

   /// # Panics
   ///
   /// When `id` came from another registry, which one process never has.
   #[must_use]
   pub fn entry(&self, id: EventId) -> &GatewayEntry {
      &self.entries[id.0]
   }

   pub fn iter(&self) -> impl Iterator<Item = (EventId, &GatewayEntry)> {
      self
         .entries
         .iter()
         .enumerate()
         .map(|(index, entry)| (EventId(index), entry))
   }
}

/// Finds the primary event table(s) containing [`SEED`]. Supports both a
/// single flat table (Android) and one or more per-domain descriptor tables
/// (Desktop `libdiscord`).
fn locate_primary(image: &Image<'_>) -> Result<Vec<GatewayEntry>, Error> {
   let mut seeds = table_entries(image, SEED, GATEWAY_HANDLER_FIELD);
   seeds.sort_unstable();
   seeds.dedup();
   if seeds.is_empty() {
      return Err(Error::NotFound { seed: SEED });
   }

   // Try walking domain descriptor tables anchored at each seed slice first.
   let mut domain_entries = Vec::new();
   let mut all_domains = true;
   for &(slot, _) in &seeds {
      let slice_start = walk_slice_start(image, slot);
      let Some(found) = walk_domains_from_slice(image, slice_start) else {
         all_domains = false;
         break;
      };
      merge_entries(&mut domain_entries, found);
   }
   if all_domains && !domain_entries.is_empty() {
      return Ok(domain_entries);
   }

   match seeds.as_slice() {
      [(slot, _)] => Ok(walk_from_slot(image, *slot)),
      _ => {
         Err(Error::Ambiguous {
            seed:  SEED,
            slots: seeds.iter().map(|(slot, _)| *slot).collect(),
         })
      },
   }
}

fn merge_entries(target: &mut Vec<GatewayEntry>, incoming: Vec<GatewayEntry>) {
   for entry in incoming {
      if let Some(existing) = target.iter_mut().find(|known| known.name == entry.name) {
         if existing.handler == entry.handler {
            for slot in entry.slots {
               if !existing.slots.contains(&slot) {
                  existing.slots.push(slot);
               }
            }
         }
      } else {
         target.push(entry);
      }
   }
}

fn entry(image: &Image<'_>, slot: usize) -> Option<GatewayEntry> {
   let ptr_size = image.pointer_width;
   let name = image.read_usize(slot)?;
   let len = image.read_usize(slot + ptr_size)?;
   let handler = image.read_usize(slot + 2 * ptr_size)?;
   if !(1..=MAX_NAME).contains(&len) || !image.is_function(handler) {
      return None;
   }
   let name = image.read(name, len)?;
   if !name
      .iter()
      .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
   {
      return None;
   }
   Some(GatewayEntry {
      name: String::from_utf8(name.to_vec()).ok()?,
      slots: vec![slot],
      handler,
   })
}

fn walk_slice_start(image: &Image<'_>, start: usize) -> usize {
   let stride = 3 * image.pointer_width;
   let mut first = start;
   while let Some(previous) = first.checked_sub(stride)
      && entry(image, previous).is_some()
   {
      first = previous;
   }
   first
}

fn walk_from_slot(image: &Image<'_>, start: usize) -> Vec<GatewayEntry> {
   let stride = 3 * image.pointer_width;
   let first = walk_slice_start(image, start);
   (0..)
      .map(|index| first + index * stride)
      .map_while(|slot| entry(image, slot))
      .collect()
}

fn walk_flat(image: &Image<'_>, seed: &'static str) -> Result<Vec<GatewayEntry>, Error> {
   let mut seeds = table_entries(image, seed, 2 * image.pointer_width);
   seeds.sort_unstable();
   seeds.dedup();
   match seeds.as_slice() {
      [(slot, _)] => Ok(walk_from_slot(image, *slot)),
      [] => Err(Error::NotFound { seed }),
      _ => {
         Err(Error::Ambiguous {
            seed,
            slots: seeds.iter().map(|(slot, _)| *slot).collect(),
         })
      },
   }
}

/// Every flat table holding [`SPLIT_SEED`]. macOS keeps two copies of it,
/// each naming the event through its own copy of the string, and dispatches
/// `MESSAGE_DELETE` through the second.
fn walk_split(image: &Image<'_>) -> Vec<Vec<GatewayEntry>> {
   let ptr_size = image.pointer_width;
   let mut slots = image
      .find_bytes_iter(SPLIT_SEED.as_bytes())
      .flat_map(|string| image.pointers_to(string))
      .filter(|&slot| {
         image.read_usize(slot + ptr_size) == Some(SPLIT_SEED.len())
            && image
               .read_usize(slot + 2 * ptr_size)
               .is_some_and(|code| image.is_function(code))
      })
      .collect::<Vec<_>>();
   slots.sort_unstable();
   slots.dedup();
   slots
      .into_iter()
      .map(|slot| walk_from_slot(image, slot))
      .collect()
}

/// Checks whether `slice_start` is referenced by a 5-word `libdiscord` domain
/// descriptor `{name_ptr, name_len, domain_id, slice_ptr, slice_len}` and, if
/// so, walks the entire contiguous domain table.
fn walk_domains_from_slice(image: &Image<'_>, slice_start: usize) -> Option<Vec<GatewayEntry>> {
   let ptr_size = image.pointer_width;
   let domain_stride = 5 * ptr_size;
   let slice_field = 3 * ptr_size;

   let anchor = image
      .pointers_to(slice_start)
      .into_iter()
      .filter_map(|ptr_slot| ptr_slot.checked_sub(slice_field))
      .find(|&desc| domain_entries(image, desc).is_some())?;

   let mut first = anchor;
   while let Some(prev) = first.checked_sub(domain_stride)
      && domain_entries(image, prev).is_some()
   {
      first = prev;
   }

   let mut out = Vec::new();
   let mut domains = 0usize;
   let mut cur = first;
   while let Some(slice_entries) = domain_entries(image, cur) {
      domains += 1;
      merge_entries(&mut out, slice_entries);
      cur += domain_stride;
   }
   (domains >= 2 && !out.is_empty()).then_some(out)
}

/// Parses one `libdiscord` domain descriptor at `addr`:
/// `{name_ptr, name_len, domain_id, slice_ptr, slice_len}`.
fn domain_entries(image: &Image<'_>, addr: usize) -> Option<Vec<GatewayEntry>> {
   let ptr_size = image.pointer_width;
   let name_ptr = image.read_usize(addr)?;
   let name_len = image.read_usize(addr + ptr_size)?;
   let domain_id = image.read_usize(addr + 2 * ptr_size)?;
   let slice_ptr = image.read_usize(addr + 3 * ptr_size)?;
   let slice_len = image.read_usize(addr + 4 * ptr_size)?;

   if !(1..=MAX_NAME).contains(&name_len) || domain_id > 1024 || slice_len > 256 {
      return None;
   }
   let name = image.read(name_ptr, name_len)?;
   if !name
      .iter()
      .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
   {
      return None;
   }
   let stride = 3 * ptr_size;
   let mut out = Vec::with_capacity(slice_len);
   for index in 0..slice_len {
      out.push(entry(image, slice_ptr + index * stride)?);
   }
   Some(out)
}

/// `(slot, code)` for every `{name ptr, name len, ...}` entry named `key` whose
/// pointer `field` bytes in starts a function.
fn table_entries(image: &Image<'_>, key: &str, field: usize) -> Vec<(usize, usize)> {
   let ptr_size = image.pointer_width;
   let field = if field == GATEWAY_HANDLER_FIELD {
      2 * ptr_size
   } else {
      field
   };
   let mut out = Vec::new();
   for string in image.find_bytes_iter(key.as_bytes()) {
      for slot in image.pointers_to(string) {
         if image.read_usize(slot + ptr_size) != Some(key.len()) {
            continue;
         }
         let Some(code) = image.read_usize(slot + field) else {
            continue;
         };
         if image.is_function(code) {
            out.push((slot, code));
         }
      }
      if !out.is_empty() {
         break;
      }
   }
   // Keep compile-time constants referenced on 64-bit targets.
   let _ = (STRIDE, DOMAIN_STRIDE);
   out
}
