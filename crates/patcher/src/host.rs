//! Patches to `liblibdiscord_wasm_host.so`, located from anchors so a new build
//! only needs a re-run while the code keeps its shape.

use std::ops::Range;

use memchr::memmem;
use object::{
   Object,
   ObjectSection,
   elf,
};
use phoenix_sigs::file::{
   ElfFile,
   Machine,
};

use crate::{
   Error,
   bytes::Bytes,
};

pub const LIBRARY: &str = "liblibdiscord_wasm_host.so";
/// The loader's name inside the APK. It replaces `liblog.so` in the host's
/// `DT_NEEDED` list, so it must be the same length to fit in `.dynstr`. The
/// loader itself needs `liblog.so`, which keeps the host's log imports bound.
pub const INJECTED: &str = "libplm.so";
const REPLACED: &str = "liblog.so";
const _: () = assert!(INJECTED.len() == REPLACED.len());

/// The client-access evaluator is the only function that references the
/// experiment table whose first entry is `{"2026-11-phoenix-app-access", 26}`.
const EXPERIMENT: &str = "2026-11-phoenix-app-access";

pub struct Patched {
   pub machine: Machine,
   pub access:  usize,
}

/// `e_machine` of an ELF file, without mapping it.
pub fn machine(path: &str, data: &[u8]) -> Result<Machine, Error> {
   let bytes = Bytes(data);
   if bytes.array::<4>(0).ok() != Some(*b"\x7fELF") {
      return Err(Error::Host(format!("{path}: not an ELF file")));
   }
   let class = data.get(4).copied();
   match (class, bytes.u16(18)?) {
      (Some(2), machine) if machine == elf::EM_AARCH64.0 => Ok(Machine::Aarch64),
      (Some(2), machine) if machine == elf::EM_X86_64.0 => Ok(Machine::X86_64),
      (Some(1), machine) if machine == elf::EM_ARM.0 => Ok(Machine::Arm),
      (_, other) => {
         Err(Error::Host(format!(
            "{path}: unsupported e_machine {other}"
         )))
      },
   }
}

/// Makes the access evaluator return true and, with `needed`, renames the
/// host's `DT_NEEDED` `liblog.so` to it.
pub fn patch(path: &str, data: &mut [u8], needed: Option<&str>) -> Result<Patched, Error> {
   let (machine, name, access, access_at) = {
      let file = ElfFile::parse(path, data).map_err(Error::Elf)?;
      let name = needed.map(|_| needed_name(path, data)).transpose()?;
      let access = access_evaluator(path, data, &file)?;
      let access_at = file
         .file_offset(access)
         .ok_or_else(|| Error::Host(format!("{path}: access evaluator has no file backing")))?;
      (file.machine, name, access, access_at)
   };

   if let (Some(at), Some(needed)) = (name, needed) {
      data[at..at + needed.len()].copy_from_slice(needed.as_bytes());
   }
   // Returns Some(true), an Option<bool> in the low byte of the return
   // register. ARM is Thumb-2, the evaluator is a Thumb function.
   let return_true: &[u8] = match machine {
      Machine::Aarch64 => &[0x20, 0x00, 0x80, 0x52, 0xC0, 0x03, 0x5F, 0xD6],
      Machine::X86_64 => &[0xB0, 0x01, 0xC3],
      Machine::Arm => &[0x01, 0x20, 0x70, 0x47],
   };
   data[access_at..access_at + return_true.len()].copy_from_slice(return_true);
   Ok(Patched { machine, access })
}

/// Renames a library's `DT_NEEDED` `liblog.so` to `needed`, the same length.
pub fn rename_needed(path: &str, data: &mut [u8], needed: &str) -> Result<(), Error> {
   if needed.len() != REPLACED.len() {
      return Err(Error::Host(format!(
         "{needed} must be as long as {REPLACED}"
      )));
   }
   let at = needed_name(path, data)?;
   data[at..at + needed.len()].copy_from_slice(needed.as_bytes());
   Ok(())
}

/// File offset of the `liblog.so` string a `DT_NEEDED` entry names.
fn needed_name(path: &str, data: &[u8]) -> Result<usize, Error> {
   const DT_NEEDED: u64 = 1;

   let file = parse(path, data)?;
   let range = |name: &str| -> Result<Range<usize>, Error> {
      let (start, size) = section(path, &file, name)?
         .file_range()
         .ok_or(Error::Truncated)?;
      let start = usize::try_from(start).map_err(|_| Error::Truncated)?;
      Ok(start..start + usize::try_from(size).map_err(|_| Error::Truncated)?)
   };
   let dynamic = Bytes(data).slice(range(".dynamic")?)?;
   let strings = range(".dynstr")?;

   // `Dyn` is two words, 8 bytes each in ELF64 and 4 in ELF32.
   let word = if file.is_64() { 8 } else { 4 };
   let read = |bytes: &[u8]| -> u64 {
      let mut buf = [0u8; 8];
      buf[..bytes.len()].copy_from_slice(bytes);
      u64::from_le_bytes(buf)
   };
   let hits = dynamic
      .chunks_exact(2 * word)
      .map(|entry| (read(&entry[..word]), read(&entry[word..])))
      .filter(|&(tag, _)| tag == DT_NEEDED)
      .filter_map(|(_, value)| strings.start.checked_add(usize::try_from(value).ok()?))
      .filter(|&at| {
         at < strings.end && data.get(at..at + REPLACED.len() + 1) == Some(b"liblog.so\0")
      })
      .collect::<Vec<usize>>();

   match hits[..] {
      [at] => Ok(at),
      _ => {
         Err(Error::Host(format!(
            "{path}: expected one DT_NEEDED {REPLACED}, found {}",
            hits.len()
         )))
      },
   }
}

fn access_evaluator(path: &str, data: &[u8], file: &ElfFile<'_>) -> Result<usize, Error> {
   let image = &file.image;
   let text = text_range(path, data)?;
   let width = image.pointer_width;
   let slots = image
      .find_bytes(EXPERIMENT.as_bytes())
      .into_iter()
      .flat_map(|string| {
         let target = (string as u64).to_le_bytes();
         image
            .segments
            .iter()
            .filter(|segment| !segment.executable)
            .flat_map(|segment| {
               memmem::find_iter(&segment.bytes[..], &target[..width])
                  .map(|at| segment.addr + at)
                  .collect::<Vec<usize>>()
            })
            .collect::<Vec<usize>>()
      })
      .filter(|slot| slot % width == 0 && image.read_usize(slot + width) == Some(EXPERIMENT.len()))
      .collect::<Vec<usize>>();

   let code = image
      .read(text.start, text.len())
      .ok_or_else(|| Error::Host(format!("{path}: .text outside every segment")))?;

   let mut functions = references(file.machine, text.start, code)
      .filter(|(_, target)| slots.contains(target))
      .filter_map(|(at, _)| image.function_containing(at))
      .collect::<Vec<usize>>();
   functions.sort_unstable();
   functions.dedup();
   match functions[..] {
      [function] => Ok(function),
      _ => {
         Err(Error::Host(format!(
            "{path}: access evaluator: expected one function referencing {EXPERIMENT}, found \
             {functions:x?}"
         )))
      },
   }
}

fn text_range(path: &str, data: &[u8]) -> Result<Range<usize>, Error> {
   let file = parse(path, data)?;
   let text = section(path, &file, ".text")?;
   let start = usize::try_from(text.address()).map_err(|_| Error::Truncated)?;
   Ok(start..start + usize::try_from(text.size()).map_err(|_| Error::Truncated)?)
}

fn parse<'data>(path: &str, data: &'data [u8]) -> Result<object::File<'data>, Error> {
   object::File::parse(data).map_err(|source| Error::Host(format!("{path}: {source}")))
}

fn section<'data, 'file>(
   path: &str,
   file: &'file object::File<'data>,
   name: &str,
) -> Result<object::Section<'data, 'file>, Error> {
   file
      .section_by_name(name)
      .ok_or_else(|| Error::Host(format!("{path}: no {name} section")))
}

/// `(instruction, target)` for every PC-relative address the code forms. On
/// arm64 that is `adrp` followed within four instructions by an `add` or 64-bit
/// `ldr` on the same register. On `x86_64` it is a REX.W `lea` or `mov` from
/// `[rip + disp32]`. ARM is [`thumb_references`].
fn references(
   machine: Machine,
   base: usize,
   code: &[u8],
) -> Box<dyn Iterator<Item = (usize, usize)> + '_> {
   match machine {
      Machine::Aarch64 => {
         let words = code
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect::<Vec<u32>>();

         Box::new((0..words.len()).flat_map(move |index| {
            let word = words[index];
            let pc = base + 4 * index;
            let is_adrp = word & 0x9F00_0000 == 0x9000_0000;
            let page = is_adrp
               .then(|| {
                  let imm = i64::from((word >> 29) & 3 | ((word >> 5) & 0x7_FFFF) << 2);
                  let imm = if imm & (1 << 20) != 0 {
                     imm - (1 << 21)
                  } else {
                     imm
                  };
                  isize::try_from(imm << 12).ok()
               })
               .flatten()
               .map(|offset| (pc & !0xFFF).wrapping_add_signed(offset));
            let register = word & 0x1F;

            let partners = words
               .get(index + 1..(index + 5).min(words.len()))
               .unwrap_or_default();
            partners
               .iter()
               .filter_map(move |&next| {
                  let page = page?;
                  let same = (next >> 5) & 0x1F == register;
                  let imm12 = ((next >> 10) & 0xFFF) as usize;
                  match next & 0xFFC0_0000 {
                     0x9100_0000 if same => Some((pc, page + imm12)),
                     0xF940_0000 if same => Some((pc, page + imm12 * 8)),
                     _ => None,
                  }
               })
               .collect::<Vec<_>>()
         }))
      },

      Machine::X86_64 => {
         Box::new((0..code.len().saturating_sub(7)).filter_map(move |at| {
            let &[rex, opcode, modrm, ..] = &code[at..] else {
               return None;
            };
            let rip_relative =
               matches!(rex, 0x48 | 0x4C) && matches!(opcode, 0x8B | 0x8D) && modrm & 0xC7 == 0x05;
            if !rip_relative {
               return None;
            }
            let disp = i32::from_le_bytes(code[at + 3..at + 7].try_into().ok()?);
            Some((
               base + at,
               (base + at + 7).wrapping_add_signed(disp as isize),
            ))
         }))
      },

      Machine::Arm => Box::new(thumb_references(base, code).into_iter()),
   }
}

/// Thumb-2 forms a `.data.rel.ro` address as `ldr Rn, [pc, #x]` or its `ldr.w`
/// form, loading `target - (pc + 4)` from the literal pool, then `add Rn, pc`.
/// The target itself never appears in the file.
fn thumb_references(base: usize, code: &[u8]) -> Vec<(usize, usize)> {
   let half = |at: usize| Some(u16::from_le_bytes(code.get(at..at + 2)?.try_into().ok()?));
   let word = |at: usize| Some(i32::from_le_bytes(code.get(at..at + 4)?.try_into().ok()?));
   let mut loaded = [None; 16];
   let mut found = Vec::new();
   let mut at = 0;
   while let Some(first) = half(at) {
      let pool = (at + 4) & !3;
      if matches!(first >> 11, 0b11101..=0b11111) {
         if first & 0xFF7F == 0xF85F
            && let Some(second) = half(at + 2)
         {
            let imm = usize::from(second & 0xFFF);
            let literal = if first & 0x80 == 0 {
               pool.checked_sub(imm)
            } else {
               pool.checked_add(imm)
            };
            loaded[usize::from(second >> 12)] = literal.and_then(word);
         }
         at += 4;
         continue;
      }
      if first & 0xF800 == 0x4800 {
         loaded[usize::from((first >> 8) & 7)] = word(pool + usize::from(first & 0xFF) * 4);
      } else if first & 0xFF78 == 0x4478 {
         let rn = usize::from((((first >> 7) & 1) << 3) | (first & 7));
         if let Some(offset) = loaded[rn] {
            let pc = base + at + 4;
            found.push((base + at, pc.wrapping_add_signed(offset as isize)));
         }
      }
      at += 2;
   }
   found
}
