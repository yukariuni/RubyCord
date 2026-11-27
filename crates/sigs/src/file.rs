//! ELF files viewed the way the dynamic linker maps them, for offline tools.

use std::{
   borrow::Cow,
   num::TryFromIntError,
};

use object::{
   Endianness,
   LittleEndian,
   Object,
   elf::{
      self,
      FileHeader32,
      FileHeader64,
   },
   macho::{
      self,
      MachHeader64,
   },
   pe::{
      self,
      IMAGE_DIRECTORY_ENTRY_EXCEPTION,
      IMAGE_FILE_MACHINE_AMD64,
      IMAGE_SCN_MEM_EXECUTE,
      IMAGE_SCN_MEM_READ,
      IMAGE_SCN_MEM_WRITE,
   },
   read::{
      elf::{
         FileHeader,
         ProgramHeader,
         SectionHeader,
         SectionTable,
      },
      macho::{
         FatArch as _,
         Fixup,
         LoadCommandVariant,
         MachHeader,
         MachOFatFile32,
         Section as _,
         Segment as _,
      },
      pe::{
         ImageNtHeaders,
         ImageOptionalHeader,
         PeFile64,
      },
   },
};
use phoenix_hook::image::{
   BuildId,
   FunctionStarts,
   Image,
   Segment,
};

/// `PT_ARM_EXIDX`, the ARM unwind table segment. The `object` crate exposes no
/// constant for it.
const PT_ARM_EXIDX: object::elf::ProgramType = object::elf::ProgramType(0x7000_0001);

#[derive(Debug, flub::Error)]
pub enum FileError {
   #[error("{path}: not an ELF file")]
   NotElf { path: String },
   #[error("{path}: {source}")]
   Parse {
      path:   String,
      #[source]
      source: object::Error,
   },
   #[error("{path}: unsupported e_machine {machine}")]
   Machine { path: String, machine: u16 },
   #[error("{path}: Mach-O cputype {cputype:#x}, only arm64 is read")]
   CpuType { path: String, cputype: u32 },
   #[error("{path}: segment or section outside the file")]
   Truncated { path: String },
   #[error("{path}: relocation at {offset:#x} outside every segment")]
   Relocation { path: String, offset: usize },
   #[error("{path}: {message}")]
   Packed {
      path:    String,
      message: &'static str,
   },
   #[error("{path}: address does not fit in usize")]
   Address {
      path:   String,
      #[source]
      source: TryFromIntError,
   },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Machine {
   Aarch64,
   X86_64,
   Arm,
}

/// A `PT_LOAD` segment's place in the file.
#[derive(Clone, Copy, Debug)]
struct FileLoad {
   vaddr:  usize,
   offset: usize,
   size:   usize,
}

pub struct ElfFile<'a> {
   /// Segments at their link addresses with `R_*_RELATIVE` relocations applied,
   /// from `.rela.dyn` plain or Android APS2-packed. RELR and REL keep the
   /// addend in place, which with a zero load base is already the final value.
   pub image:   Image<'a>,
   pub machine: Machine,
   loads:       Vec<FileLoad>,
}

impl<'a> ElfFile<'a> {
   pub fn parse(path: &str, data: &'a [u8]) -> Result<Self, FileError> {
      match data.get(4) {
         Some(2) => Self::build::<FileHeader64<Endianness>>(path, data),
         Some(1) => Self::build::<FileHeader32<Endianness>>(path, data),
         _ => {
            Err(FileError::NotElf {
               path: path.to_owned(),
            })
         },
      }
   }

   fn build<Elf>(path: &str, data: &'a [u8]) -> Result<Self, FileError>
   where
      Elf: FileHeader<Endian = Endianness>,
      Elf::Word: Into<u64>,
   {
      let parse = |source| {
         FileError::Parse {
            path: path.to_owned(),
            source,
         }
      };
      let truncated = || {
         FileError::Truncated {
            path: path.to_owned(),
         }
      };
      let address = |source| {
         FileError::Address {
            path: path.to_owned(),
            source,
         }
      };

      let header = Elf::parse(data).map_err(parse)?;
      let endian = header.endian().map_err(parse)?;
      // ARM32 keeps its `R_ARM_RELATIVE` addends in place, so only the RELA
      // targets need writing back. `relative` is the matching reloc type.
      let (machine, pointer_width, relative) = match header.e_machine(endian) {
         elf::EM_X86_64 => (Machine::X86_64, 8, Some(elf::R_X86_64_RELATIVE.0)),
         elf::EM_AARCH64 => (Machine::Aarch64, 8, Some(elf::R_AARCH64_RELATIVE.0)),
         elf::EM_ARM => (Machine::Arm, 4, None),
         other => {
            return Err(FileError::Machine {
               path:    path.to_owned(),
               machine: other.0,
            });
         },
      };

      let mut image = Image {
         path: path.to_owned(),
         build_id: None,
         segments: Vec::new(),
         eh_frame_hdr: None,
         arm_exidx: None,
         pdata: None,
         pointer_width,
         function_starts: None,
      };
      let mut loads = Vec::new();
      for segment in header.program_headers(endian, data).map_err(parse)? {
         let bytes = segment.data(endian, data).map_err(|()| truncated())?;
         let vaddr = usize::try_from(segment.p_vaddr(endian).into()).map_err(address)?;
         let flags = segment.p_flags(endian);

         match segment.p_type(endian) {
            elf::PT_LOAD => {
               image.segments.push(Segment {
                  addr:       vaddr,
                  bytes:      mapped(bytes, segment.p_memsz(endian).into()).map_err(address)?,
                  executable: flags.0 & elf::PF_X.0 != 0,
                  writable:   flags.0 & elf::PF_W.0 != 0,
               });
               loads.push(FileLoad {
                  vaddr,
                  offset: usize::try_from(segment.p_offset(endian).into()).map_err(address)?,
                  size: bytes.len(),
               });
            },
            elf::PT_NOTE if image.build_id.is_none() => image.build_id = BuildId::from_notes(bytes),
            elf::PT_GNU_EH_FRAME => image.eh_frame_hdr = Some(vaddr),
            PT_ARM_EXIDX => {
               image.arm_exidx = Some((
                  vaddr,
                  usize::try_from(segment.p_memsz(endian).into()).map_err(address)?,
               ));
            },
            _ => {},
         }
      }

      if let Some(relative) = relative {
         let sections = header.sections(endian, data).map_err(parse)?;
         apply_relocations(path, endian, data, &sections, relative, &mut image.segments)?;
      }

      Ok(Self {
         image,
         machine,
         loads,
      })
   }

   /// Where the byte at link address `vaddr` sits in the file, when it has file
   /// backing.
   #[must_use]
   pub fn file_offset(&self, vaddr: usize) -> Option<usize> {
      self
         .loads
         .iter()
         .find(|load| (load.vaddr..load.vaddr + load.size).contains(&vaddr))
         .map(|load| vaddr - load.vaddr + load.offset)
   }
}

/// A Mach-O image viewed at its link addresses with its chained fixups applied,
/// for offline tools.
pub struct MachOFile<'a> {
   pub image: Image<'a>,
   /// `(vmaddr, file offset, size)` per instruction section, to map a link
   /// address back to its byte in the file.
   code:      Vec<(usize, usize, usize)>,
}

impl MachOFile<'_> {
   /// File offset of a link address, if it falls in an instruction section.
   #[must_use]
   pub fn file_offset(&self, vaddr: usize) -> Option<usize> {
      self.code.iter().find_map(|&(addr, offset, size)| {
         (vaddr >= addr && vaddr < addr + size).then(|| offset + (vaddr - addr))
      })
   }
}

impl<'a> MachOFile<'a> {
   #[expect(
      clippy::too_many_lines,
      reason = "one pass over the load commands, then one over the fixup chains"
   )]
   pub fn parse(path: &str, data: &'a [u8]) -> Result<Self, FileError> {
      let parse = |source| {
         FileError::Parse {
            path: path.to_owned(),
            source,
         }
      };
      let truncated = || {
         FileError::Truncated {
            path: path.to_owned(),
         }
      };
      let address = |source| {
         FileError::Address {
            path: path.to_owned(),
            source,
         }
      };

      let header = MachHeader64::<Endianness>::parse(data, 0).map_err(parse)?;
      let endian = header.endian().map_err(parse)?;
      let cputype = header.cputype(endian);
      if cputype != macho::CPU_TYPE_ARM64 {
         return Err(FileError::CpuType {
            path:    path.to_owned(),
            cputype: cputype.0,
         });
      }
      let mut image = Image {
         path:            path.to_owned(),
         build_id:        None,
         segments:        Vec::new(),
         eh_frame_hdr:    None,
         arm_exidx:       None,
         pdata:           None,
         pointer_width:   8,
         function_starts: None,
      };

      // Fixup chains name segments by load-command order, `__PAGEZERO`
      // included.
      let mut by_index = Vec::new();
      let mut text = 0;
      let mut fixups = None;
      let mut starts = None;
      let mut linkedit = None;
      let mut code = Vec::new();
      for command in header.load_commands(endian, data, 0).map_err(parse)? {
         match command.map_err(parse)?.variant().map_err(parse)? {
            LoadCommandVariant::Segment64(segment, sections) => {
               let vmaddr = segment.vmaddr(endian);
               if segment.name() == b"__TEXT" {
                  text = vmaddr;
               }
               if segment.name() == b"__LINKEDIT" {
                  linkedit = Some((vmaddr, segment.fileoff(endian)));
               }
               if segment.filesize(endian) == 0 {
                  by_index.push(None);
                  continue;
               }
               // `__TEXT` maps strings and constants executable beside the
               // code, so only instruction sections count as code.
               if segment.initprot(endian).0 & macho::VM_PROT_EXECUTE.0 != 0 {
                  by_index.push(None);
                  for section in segment.sections(endian, sections).map_err(parse)? {
                     let start = section.offset(endian) as usize;
                     let length = usize::try_from(section.size(endian)).map_err(address)?;
                     let instructions =
                        macho::S_ATTR_PURE_INSTRUCTIONS.0 | macho::S_ATTR_SOME_INSTRUCTIONS.0;
                     let addr = usize::try_from(section.addr(endian)).map_err(address)?;
                     if section.flags(endian).0 & instructions != 0 {
                        code.push((addr, start, length));
                     }
                     image.segments.push(Segment {
                        addr,
                        bytes: Cow::Borrowed(
                           data.get(start..start + length).ok_or_else(truncated)?,
                        ),
                        executable: section.flags(endian).0 & instructions != 0,
                        writable: false,
                     });
                  }
                  continue;
               }
               let bytes = segment.data(endian, data).map_err(|()| truncated())?;
               let mut owned = bytes.to_vec();
               owned.resize(usize::try_from(segment.vmsize(endian)).map_err(address)?, 0);
               let initprot = segment.initprot(endian).0;
               by_index.push(Some((image.segments.len(), bytes)));
               image.segments.push(Segment {
                  addr:       usize::try_from(vmaddr).map_err(address)?,
                  bytes:      Cow::Owned(owned),
                  executable: initprot & macho::VM_PROT_EXECUTE.0 != 0,
                  writable:   initprot & macho::VM_PROT_WRITE.0 != 0,
               });
            },
            LoadCommandVariant::LinkeditData(command)
               if command.cmd.get(endian) == macho::LC_DYLD_CHAINED_FIXUPS =>
            {
               fixups = Some(command.chained_fixups(endian, data).map_err(parse)?);
            },
            LoadCommandVariant::LinkeditData(command)
               if command.cmd.get(endian) == macho::LC_FUNCTION_STARTS =>
            {
               starts = Some((
                  command.dataoff.get(endian) as usize,
                  command.datasize.get(endian) as usize,
               ));
            },
            LoadCommandVariant::Uuid(uuid) if image.build_id.is_none() => {
               image.build_id = BuildId::from_bytes(&uuid.uuid);
            },
            _ => {},
         }
      }

      // Pointers on disk are chain links. Rebases become link addresses, and
      // binds to other images become 0 since their targets are not loaded.
      for chain in fixups
         .iter()
         .flat_map(|fixups| fixups.segments(endian))
         .flatten()
      {
         let chain = chain.map_err(parse)?;
         let Some(&Some((slot, on_disk))) = by_index.get(chain.index() as usize) else {
            continue;
         };
         let Cow::Owned(bytes) = &mut image.segments[slot].bytes else {
            continue;
         };
         for link in chain.fixups(endian, text, on_disk) {
            let (offset, fixup) = link.map_err(parse)?;
            let value = match fixup {
               Fixup::Rebase(rebase) => text + rebase.target_offset,
               _ => 0,
            };
            let at = usize::try_from(offset).map_err(address)?;
            bytes
               .get_mut(at..at + 8)
               .ok_or_else(truncated)?
               .copy_from_slice(&value.to_le_bytes());
         }
      }
      if let (Some((offset, len)), Some((vmaddr, fileoff))) = (starts, linkedit)
         && let Some(into) = (offset as u64).checked_sub(fileoff)
      {
         image.function_starts = Some(FunctionStarts::new(
            usize::try_from(vmaddr + into).map_err(address)?,
            len,
            usize::try_from(text).map_err(address)?,
         ));
      }
      Ok(Self { image, code })
   }
}

/// A PE64 image mapped at its preferred image base, for offline desktop tools.
pub struct PeFile<'a> {
   pub image:   Image<'a>,
   pub machine: Machine,
}

impl<'a> PeFile<'a> {
   pub fn parse(path: &str, data: &'a [u8]) -> Result<Self, FileError> {
      let parse = |source| {
         FileError::Parse {
            path: path.to_owned(),
            source,
         }
      };
      let address = |source| {
         FileError::Address {
            path: path.to_owned(),
            source,
         }
      };

      let pe = PeFile64::parse(data).map_err(parse)?;
      let machine = match pe.nt_headers().file_header().machine.get(LittleEndian) {
         IMAGE_FILE_MACHINE_AMD64 => Machine::X86_64,
         other => {
            return Err(FileError::Machine {
               path:    path.to_owned(),
               machine: other.0,
            });
         },
      };
      let image_base =
         usize::try_from(pe.nt_headers().optional_header().image_base()).map_err(address)?;
      let pdata = pe
         .data_directory(IMAGE_DIRECTORY_ENTRY_EXCEPTION)
         .and_then(|dir| {
            let rva = usize::try_from(dir.virtual_address.get(LittleEndian)).ok()?;
            let len = usize::try_from(dir.size.get(LittleEndian)).ok()?;
            (rva != 0 && len >= 12).then_some((image_base, image_base + rva, len))
         });

      let mut segments = Vec::new();
      for section in pe.section_table().iter() {
         let chars = section.characteristics.get(LittleEndian).0;
         if chars & IMAGE_SCN_MEM_READ.0 == 0 {
            continue;
         }
         let bytes = section.pe_data(data).map_err(parse)?;
         let rva = usize::try_from(section.virtual_address.get(LittleEndian)).map_err(address)?;
         let vsize = u64::from(section.virtual_size.get(LittleEndian));
         let memsz = vsize.max(bytes.len() as u64);
         segments.push(Segment {
            addr:       image_base + rva,
            bytes:      mapped(bytes, memsz).map_err(address)?,
            executable: chars & IMAGE_SCN_MEM_EXECUTE.0 != 0,
            writable:   chars & IMAGE_SCN_MEM_WRITE.0 != 0,
         });
      }

      let build_id = pe
         .pdb_info()
         .ok()
         .flatten()
         .and_then(|info| BuildId::from_bytes(&info.guid()));

      Ok(Self {
         image: Image {
            path: path.to_owned(),
            build_id,
            segments,
            eh_frame_hdr: None,
            arm_exidx: None,
            pdata,
            pointer_width: 8,
            function_starts: None,
         },
         machine,
      })
   }
}

/// An ELF, PE64 or arm64 Mach-O binary parsed from disk, a fat Mach-O by its
/// arm64 slice.
pub enum BinaryFile<'a> {
   Elf(ElfFile<'a>),
   Pe(PeFile<'a>),
   MachO(MachOFile<'a>),
}

impl<'a> BinaryFile<'a> {
   pub fn parse(path: &str, data: &'a [u8]) -> Result<Self, FileError> {
      if data.starts_with(&pe::IMAGE_DOS_SIGNATURE.to_le_bytes()) {
         Ok(Self::Pe(PeFile::parse(path, data)?))
      } else if data.starts_with(&macho::MH_MAGIC_64.to_le_bytes()) {
         Ok(Self::MachO(MachOFile::parse(path, data)?))
      } else if data.starts_with(&macho::FAT_MAGIC.to_be_bytes()) {
         let parse = |source| {
            FileError::Parse {
               path: path.to_owned(),
               source,
            }
         };
         let arches = MachOFatFile32::parse(data).map_err(parse)?.arches();
         let arm64 = arches
            .iter()
            .find(|arch| arch.cputype() == macho::CPU_TYPE_ARM64)
            .ok_or_else(|| {
               FileError::CpuType {
                  path:    path.to_owned(),
                  cputype: arches.first().map_or(0, |arch| arch.cputype().0),
               }
            })?;
         Ok(Self::MachO(MachOFile::parse(
            path,
            arm64.data(data).map_err(parse)?,
         )?))
      } else {
         Ok(Self::Elf(ElfFile::parse(path, data)?))
      }
   }

   #[must_use]
   pub const fn machine(&self) -> Machine {
      match self {
         Self::Elf(file) => file.machine,
         Self::Pe(file) => file.machine,
         Self::MachO(_) => Machine::Aarch64,
      }
   }

   #[must_use]
   pub const fn image(&self) -> &Image<'a> {
      match self {
         Self::Elf(file) => &file.image,
         Self::Pe(file) => &file.image,
         Self::MachO(file) => &file.image,
      }
   }
}

/// A segment as loaded. Only segments with `.bss` or relocations get copied,
/// which keeps the 100 MB host library from doubling in the site's wasm.
fn mapped(bytes: &[u8], size: u64) -> Result<Cow<'_, [u8]>, TryFromIntError> {
   let length = usize::try_from(size)?;
   if length == bytes.len() {
      return Ok(Cow::Borrowed(bytes));
   }
   let mut owned = bytes.to_vec();
   owned.resize(length, 0);
   Ok(Cow::Owned(owned))
}

fn plain_rela(bytes: &[u8], relative: u32) -> Vec<(usize, u64)> {
   bytes
      .as_chunks::<24>()
      .0
      .iter()
      .filter_map(|entry| {
         let word = |at: usize| u64::from_le_bytes(entry[at..at + 8].try_into().unwrap());
         let offset = usize::try_from(word(0)).ok()?;
         (word(8) & 0xFFFF_FFFF == u64::from(relative)).then_some((offset, word(16)))
      })
      .collect()
}

/// Android's packed relocation format: `APS2` then SLEB128 groups of offset
/// deltas, info and addend deltas, each optionally shared across the group.
fn aps2(bytes: &[u8], relative: u32) -> Result<Vec<(usize, u64)>, &'static str> {
   const BY_INFO: i64 = 1;
   const BY_OFFSET_DELTA: i64 = 2;
   const BY_ADDEND: i64 = 4;
   const HAS_ADDEND: i64 = 8;

   let mut stream = bytes
      .strip_prefix(b"APS2")
      .ok_or("packed relocations without APS2")?;

   let mut next = || -> Result<i64, &'static str> {
      let (mut value, mut shift) = (0i64, 0);
      loop {
         let (&byte, rest) = stream.split_first().ok_or("truncated APS2 stream")?;
         stream = rest;
         value |= i64::from(byte & 0x7F) << shift;
         shift += 7;
         if byte & 0x80 == 0 {
            if byte & 0x40 != 0 && shift < 64 {
               value |= -1 << shift;
            }
            return Ok(value);
         }
      }
   };

   let count = next()?;
   let (mut offset, mut addend, mut done) = (next()?, 0i64, 0);
   let mut out = Vec::new();

   while done < count {
      let group_size = next()?;
      let flags = next()?;
      let offset_delta = if flags & BY_OFFSET_DELTA != 0 {
         next()?
      } else {
         0
      };

      let mut info = if flags & BY_INFO != 0 { next()? } else { 0 };
      if flags & HAS_ADDEND != 0 && flags & BY_ADDEND != 0 {
         addend += next()?;
      }

      for _ in 0..group_size {
         offset += if flags & BY_OFFSET_DELTA != 0 {
            offset_delta
         } else {
            next()?
         };

         if flags & BY_INFO == 0 {
            info = next()?;
         }
         if flags & HAS_ADDEND != 0 && flags & BY_ADDEND == 0 {
            addend += next()?;
         }
         if info & 0xFFFF_FFFF == i64::from(relative) {
            let value = if flags & HAS_ADDEND != 0 { addend } else { 0 };
            out.push((
               usize::try_from(offset).map_err(|_| "negative relocation offset")?,
               value.cast_unsigned(),
            ));
         }

         done += 1;
      }
   }
   Ok(out)
}

/// Writes the addends of `relative` relocations into the segments, as the
/// linker would at load.
fn apply_relocations<Elf>(
   path: &str,
   endian: Endianness,
   data: &[u8],
   sections: &SectionTable<'_, Elf>,
   relative: u32,
   segments: &mut [Segment<'_>],
) -> Result<(), FileError>
where
   Elf: FileHeader<Endian = Endianness>,
{
   for section in sections.iter() {
      let contents = || {
         section.data(endian, data).map_err(|source| {
            FileError::Parse {
               path: path.to_owned(),
               source,
            }
         })
      };

      let relocs = match section.sh_type(endian) {
         elf::SHT_RELA => plain_rela(contents()?, relative),
         elf::SHT_ANDROID_RELA => {
            aps2(contents()?, relative).map_err(|message| {
               FileError::Packed {
                  path: path.to_owned(),
                  message,
               }
            })?
         },
         _ => continue,
      };

      for (offset, addend) in relocs {
         let segment = segments
            .iter_mut()
            .find(|segment| segment.contains(offset))
            .ok_or_else(|| {
               FileError::Relocation {
                  path: path.to_owned(),
                  offset,
               }
            })?;
         let at = offset - segment.addr;
         segment.bytes.to_mut()[at..at + 8].copy_from_slice(&addend.to_le_bytes());
      }
   }
   Ok(())
}
