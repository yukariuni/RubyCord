//! In-place injection into `Discord.exe`, the way Vencord/Equicord patch Electron Discord: add
//! one import to the executable so Windows loads `rubycord.dll` (the loader, dropped next to
//! `Discord.exe`) on every launch, from a shortcut, a `discord://` link or anything else. No
//! launcher and no shortcut changes.
//!
//! The patch is three edits to the PE32+ file:
//! 1. `DependentLoadFlags` gains `LOAD_LIBRARY_SEARCH_APPLICATION_DIR` (`0x200`), so the added
//!    import is found beside the exe and not only in System32.
//! 2. A new read/write section (`.rubyc`) holds a copy of the import descriptors plus one for
//!    `rubycord.dll`, and the import data directory points at it. The original descriptors are
//!    copied verbatim, so their thunks and names keep working.
//! 3. The certificate directory is cleared, since editing the file voids the signature anyway.
//!
//! Every edit but the flag sits in the first `SizeOfHeaders` bytes, so [`restore`] puts those
//! bytes back, restores the flag and truncates the appended section — the exact original file.
//! The offsets are read from the PE, not hard-coded, so a Discord update that moves things still
//! patches. It never writes while Discord runs (the caller checks), and never patches twice.

use std::{
   fs,
   io::{self, Read, Seek, SeekFrom, Write},
   path::Path,
};

/// The section the patch adds, also how [`is_patched`] recognises its own work.
const SECTION_NAME: &[u8; 8] = b".rubyc\0\0";
/// The DLL the import names, dropped next to `Discord.exe`.
const DLL_NAME: &[u8] = b"rubycord.dll\0";
/// The export bound to force the load; the loader never has it called.
const IMPORT_NAME: &[u8] = b"RubyCordAttach\0";
/// `LOAD_LIBRARY_SEARCH_APPLICATION_DIR`, added to `DependentLoadFlags`.
const SEARCH_APPLICATION_DIR: u16 = 0x0200;
/// `CNT_INITIALIZED_DATA | MEM_READ | MEM_WRITE`: the IAT is written at load.
const SECTION_CHARS: u32 = 0xC000_0040;

#[derive(Debug)]
pub enum Error {
   Io(io::Error),
   /// The file is not a PE32+ AMD64 image the patcher understands.
   NotSupported(String),
   /// No `.rubyc` undo record, or it does not match this file.
   NothingToRestore,
}

impl std::fmt::Display for Error {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      match self {
         Self::Io(err) => write!(f, "{err}"),
         Self::NotSupported(why) => {
            write!(f, "this Discord.exe is not one RubyCord can patch: {why}")
         },
         Self::NothingToRestore => write!(f, "no record of a RubyCord patch to undo"),
      }
   }
}

impl From<io::Error> for Error {
   fn from(err: io::Error) -> Self {
      Self::Io(err)
   }
}

type Result<T> = std::result::Result<T, Error>;

fn u16(data: &[u8], at: usize) -> u16 {
   u16::from_le_bytes(data[at..at + 2].try_into().unwrap())
}

fn u32(data: &[u8], at: usize) -> u32 {
   u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn align_up(value: u64, to: u64) -> u64 {
   value.div_ceil(to) * to
}

/// The parts of the PE headers the patch reads and writes.
struct Pe {
   /// The first `size_of_headers` bytes.
   header: Vec<u8>,
   coff: usize,
   optional: usize,
   data_dirs: usize,
   section_table: usize,
   sections: usize,
   section_align: u64,
   file_align: u64,
   size_of_headers: u64,
   /// File offset of `DependentLoadFlags`, if the load config directory is present.
   dependent_flags: Option<usize>,
}

impl Pe {
   fn parse(file: &mut fs::File) -> Result<Self> {
      let mut probe = vec![0u8; 0x1000];
      let read = file.read(&mut probe)?;
      probe.truncate(read);
      let bad = |why: &str| Error::NotSupported(why.to_owned());
      if probe.len() < 0x40 || &probe[..2] != b"MZ" {
         return Err(bad("not an executable"));
      }
      let coff = u32(&probe, 0x3C) as usize;
      if coff + 24 > probe.len() || &probe[coff..coff + 4] != b"PE\0\0" {
         return Err(bad("no PE header"));
      }
      let coff = coff + 4;
      if u16(&probe, coff) != 0x8664 {
         return Err(bad("not a 64-bit image"));
      }
      let optional = coff + 20;
      if u16(&probe, optional) != 0x20B {
         return Err(bad("not PE32+"));
      }
      let opt_size = u16(&probe, coff + 16) as usize;
      let rva_count = u32(&probe, optional + 108) as usize;
      let data_dirs = optional + 112;
      let section_table = optional + opt_size;
      let size_of_headers = u32(&probe, optional + 60) as u64;

      let mut file_header = vec![0u8; size_of_headers as usize];
      file.seek(SeekFrom::Start(0))?;
      file.read_exact(&mut file_header)?;

      let mut pe = Self {
         section_align: u32(&file_header, optional + 32) as u64,
         file_align: u32(&file_header, optional + 36) as u64,
         sections: u16(&file_header, coff + 2) as usize,
         header: file_header,
         coff,
         optional,
         data_dirs,
         section_table,
         size_of_headers,
         dependent_flags: None,
      };
      // The load config directory (index 10) holds DependentLoadFlags at offset 0x4E.
      if rva_count > 10 {
         let rva = u32(&pe.header, data_dirs + 10 * 8);
         if rva != 0 {
            pe.dependent_flags = pe.rva_to_offset(rva).map(|off| off + 0x4E);
         }
      }
      Ok(pe)
   }

   fn section(&self, index: usize) -> &[u8] {
      let at = self.section_table + index * 40;
      &self.header[at..at + 40]
   }

   fn rva_to_offset(&self, rva: u32) -> Option<usize> {
      for index in 0..self.sections {
         let section = self.section(index);
         let va = u32(section, 12);
         let vsize = u32(section, 8);
         let raw = u32(section, 20);
         let raw_size = u32(section, 16);
         if rva >= va && u64::from(rva) < u64::from(va) + u64::from(vsize.max(raw_size)) {
            return Some((raw + (rva - va)) as usize);
         }
      }
      None
   }

   fn data_dir(&self, index: usize) -> (u32, u32) {
      let at = self.data_dirs + index * 8;
      (u32(&self.header, at), u32(&self.header, at + 8))
   }
}

/// Whether `exe` already carries the patch, by its `.rubyc` section.
pub fn is_patched(exe: &Path) -> Result<bool> {
   let mut file = fs::File::open(exe)?;
   let pe = Pe::parse(&mut file)?;
   Ok((0..pe.sections).any(|index| pe.section(index)[..8] == SECTION_NAME[..]))
}

/// The bytes the original import descriptors occupy (up to, not including, the null one).
fn read_descriptors(file: &mut fs::File, pe: &Pe) -> Result<Vec<u8>> {
   let (rva, _) = pe.data_dir(1);
   let offset = pe
      .rva_to_offset(rva)
      .ok_or_else(|| Error::NotSupported("import table is unmapped".to_owned()))?;
   file.seek(SeekFrom::Start(offset as u64))?;
   let mut descriptors = Vec::new();
   let mut one = [0u8; 20];
   loop {
      file.read_exact(&mut one)?;
      if one == [0u8; 20] {
         return Ok(descriptors);
      }
      descriptors.extend_from_slice(&one);
   }
}

/// The `.rubyc` section's bytes: the descriptor table, then the thunks, the imported name and the
/// DLL name. `base` is the section's RVA, so the fields can point at themselves.
fn build_section(base: u32, original: &[u8]) -> (Vec<u8>, u32) {
   let count = original.len() / 20;
   // descriptors: the originals, ours, then the null terminator.
   let descriptors_len = (count + 2) * 20;
   let aligned = |len: usize| (len + 7) & !7;
   let ilt_off = aligned(descriptors_len);
   let iat_off = ilt_off + 16;
   let ibn_off = iat_off + 16;
   let name_off = ibn_off + 2 + IMPORT_NAME.len();
   let total = name_off + DLL_NAME.len();

   let rva = |off: usize| base + off as u32;
   let mut data = vec![0u8; total];
   data[..original.len()].copy_from_slice(original);
   // Our descriptor, right after the originals.
   let ours = count * 20;
   data[ours..ours + 4].copy_from_slice(&rva(ilt_off).to_le_bytes()); // OriginalFirstThunk
   data[ours + 12..ours + 16].copy_from_slice(&rva(name_off).to_le_bytes()); // Name
   data[ours + 16..ours + 20].copy_from_slice(&rva(iat_off).to_le_bytes()); // FirstThunk
   // The import lookup and address tables: one by-name entry, then a null.
   data[ilt_off..ilt_off + 8].copy_from_slice(&u64::from(rva(ibn_off)).to_le_bytes());
   data[iat_off..iat_off + 8].copy_from_slice(&u64::from(rva(ibn_off)).to_le_bytes());
   // IMAGE_IMPORT_BY_NAME: a zero hint, then the name.
   data[ibn_off + 2..ibn_off + 2 + IMPORT_NAME.len()].copy_from_slice(IMPORT_NAME);
   data[name_off..].copy_from_slice(DLL_NAME);
   (data, descriptors_len as u32)
}

/// Patches `exe` so it loads `rubycord.dll`, writing an undo record to `undo`. The caller has
/// already made sure Discord is not running and the DLL is in place.
pub fn inject(exe: &Path, undo: &Path) -> Result<()> {
   let mut file = fs::OpenOptions::new().read(true).write(true).open(exe)?;
   let mut pe = Pe::parse(&mut file)?;
   if (0..pe.sections).any(|index| pe.section(index)[..8] == SECTION_NAME[..]) {
      return Ok(()); // already patched
   }
   let dependent_flags = pe
      .dependent_flags
      .ok_or_else(|| Error::NotSupported("no load config to adjust the DLL search".to_owned()))?;

   // The undo record first, with the live flag value read here.
   let original_len = file.seek(SeekFrom::End(0))?;
   let original_flag = {
      file.seek(SeekFrom::Start(dependent_flags as u64))?;
      let mut two = [0u8; 2];
      file.read_exact(&mut two)?;
      u16::from_le_bytes(two)
   };
   {
      let mut record = Vec::new();
      record.extend_from_slice(b"RUBYUNDO1");
      record.extend_from_slice(&original_len.to_le_bytes());
      record.extend_from_slice(&(dependent_flags as u64).to_le_bytes());
      record.extend_from_slice(&original_flag.to_le_bytes());
      record.extend_from_slice(&(pe.header.len() as u64).to_le_bytes());
      record.extend_from_slice(&pe.header);
      fs::write(undo, record)?;
   }

   if pe.section_table + (pe.sections + 1) * 40 > pe.size_of_headers as usize {
      return Err(Error::NotSupported(
         "no room in the header for another section".to_owned(),
      ));
   }

   let original = read_descriptors(&mut file, &pe)?;
   let new_rva = {
      let last = pe.section(pe.sections - 1);
      align_up(
         u64::from(u32(last, 12)) + u64::from(u32(last, 8)),
         pe.section_align,
      )
   };
   let (section_data, import_size) = build_section(new_rva as u32, &original);
   let raw_ptr = align_up(original_len, pe.file_align);
   let raw_size = align_up(section_data.len() as u64, pe.file_align);

   // Write the new section header into the header buffer.
   let slot = pe.section_table + pe.sections * 40;
   let header = &mut pe.header;
   header[slot..slot + 8].copy_from_slice(SECTION_NAME);
   header[slot + 8..slot + 12].copy_from_slice(&(section_data.len() as u32).to_le_bytes());
   header[slot + 12..slot + 16].copy_from_slice(&(new_rva as u32).to_le_bytes());
   header[slot + 16..slot + 20].copy_from_slice(&(raw_size as u32).to_le_bytes());
   header[slot + 20..slot + 24].copy_from_slice(&(raw_ptr as u32).to_le_bytes());
   header[slot + 24..slot + 36].fill(0);
   header[slot + 36..slot + 40].copy_from_slice(&SECTION_CHARS.to_le_bytes());
   // NumberOfSections += 1.
   let next = pe.sections as u16 + 1;
   header[pe.coff + 2..pe.coff + 4].copy_from_slice(&next.to_le_bytes());
   // SizeOfImage covers the new section.
   let size_of_image = new_rva + align_up(section_data.len() as u64, pe.section_align);
   header[pe.optional + 56..pe.optional + 60]
      .copy_from_slice(&(size_of_image as u32).to_le_bytes());
   // Import directory -> our descriptor table at the section's start.
   header[pe.data_dirs + 8..pe.data_dirs + 12].copy_from_slice(&(new_rva as u32).to_le_bytes());
   header[pe.data_dirs + 12..pe.data_dirs + 16].copy_from_slice(&import_size.to_le_bytes());
   // Clear the now-void certificate directory (index 4).
   header[pe.data_dirs + 4 * 8..pe.data_dirs + 4 * 8 + 8].fill(0);

   // Apply: the header, the flag, then the appended section.
   file.seek(SeekFrom::Start(0))?;
   file.write_all(header)?;
   file.seek(SeekFrom::Start(dependent_flags as u64))?;
   file.write_all(&(original_flag | SEARCH_APPLICATION_DIR).to_le_bytes())?;
   file.seek(SeekFrom::Start(original_len))?;
   if raw_ptr > original_len {
      file.write_all(&vec![0u8; (raw_ptr - original_len) as usize])?;
   }
   file.write_all(&section_data)?;
   let pad = raw_size - section_data.len() as u64;
   if pad > 0 {
      file.write_all(&vec![0u8; pad as usize])?;
   }
   file.flush()?;
   Ok(())
}

/// Puts `exe` back to the exact bytes it had before [`inject`], from the undo record. Returns
/// `false` and changes nothing when the file is not the one that was patched (a Discord update
/// replaced it), so the caller can treat that as already clean.
pub fn restore(exe: &Path, undo: &Path) -> Result<bool> {
   let record = match fs::read(undo) {
      Ok(record) => record,
      Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(Error::NothingToRestore),
      Err(err) => return Err(err.into()),
   };
   if record.len() < 35 || &record[..9] != b"RUBYUNDO1" {
      return Err(Error::NothingToRestore);
   }
   let original_len = u64::from_le_bytes(record[9..17].try_into().unwrap());
   let flag_off = u64::from_le_bytes(record[17..25].try_into().unwrap());
   let flag = u16::from_le_bytes(record[25..27].try_into().unwrap());
   let header_len = u64::from_le_bytes(record[27..35].try_into().unwrap()) as usize;
   if record.len() != 35 + header_len {
      return Err(Error::NothingToRestore);
   }
   let header = &record[35..];

   let mut file = fs::OpenOptions::new().read(true).write(true).open(exe)?;
   // Only restore the file this record describes: our section must still be there.
   if !Pe::parse(&mut file)
      .is_ok_and(|pe| (0..pe.sections).any(|index| pe.section(index)[..8] == SECTION_NAME[..]))
   {
      let _ = fs::remove_file(undo);
      return Ok(false);
   }
   file.seek(SeekFrom::Start(0))?;
   file.write_all(header)?;
   if flag_off != 0 {
      file.seek(SeekFrom::Start(flag_off))?;
      file.write_all(&flag.to_le_bytes())?;
   }
   file.set_len(original_len)?;
   file.flush()?;
   drop(file);
   let _ = fs::remove_file(undo);
   Ok(true)
}

#[cfg(test)]
mod tests {
   use super::*;

   /// A minimal but real PE32+ AMD64 file: DOS header, PE/COFF, optional header with data
   /// directories, an `.rdata` section holding one import descriptor and a load config with
   /// `DependentLoadFlags`, and a `.reloc` section. Enough for the patcher to work on.
   fn build_pe() -> Vec<u8> {
      const HEADERS: usize = 0x200;
      const RDATA_RAW: usize = 0x200;
      const RELOC_RAW: usize = 0x400;
      let coff = 0x40;
      let optional = coff + 4 + 20;
      let data_dirs = optional + 112;
      let section_table = optional + 240;

      let mut file = vec![0u8; RELOC_RAW + 0x200];
      file[..2].copy_from_slice(b"MZ");
      file[0x3C..0x40].copy_from_slice(&(coff as u32).to_le_bytes());
      file[coff..coff + 4].copy_from_slice(b"PE\0\0");
      let c = coff + 4;
      file[c..c + 2].copy_from_slice(&0x8664u16.to_le_bytes()); // machine
      file[c + 2..c + 4].copy_from_slice(&2u16.to_le_bytes()); // sections
      file[c + 16..c + 18].copy_from_slice(&240u16.to_le_bytes()); // optional size

      file[optional..optional + 2].copy_from_slice(&0x20Bu16.to_le_bytes()); // magic
      file[optional + 32..optional + 36].copy_from_slice(&0x1000u32.to_le_bytes()); // section align
      file[optional + 36..optional + 40].copy_from_slice(&0x200u32.to_le_bytes()); // file align
      file[optional + 56..optional + 60].copy_from_slice(&0x3000u32.to_le_bytes()); // size of image
      file[optional + 60..optional + 64].copy_from_slice(&(HEADERS as u32).to_le_bytes());
      file[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes()); // rva count

      // .rdata: import descriptor (one real + null) at rva 0x1000, load config after it.
      let import_rva = 0x1000u32;
      let name_rva = 0x1040u32;
      let lcfg_rva = 0x1080u32;
      file[data_dirs + 8..data_dirs + 12].copy_from_slice(&import_rva.to_le_bytes());
      file[data_dirs + 12..data_dirs + 16].copy_from_slice(&40u32.to_le_bytes());
      file[data_dirs + 4 * 8..data_dirs + 4 * 8 + 8].copy_from_slice(&0x1234_5678u64.to_le_bytes());
      file[data_dirs + 10 * 8..data_dirs + 10 * 8 + 4].copy_from_slice(&lcfg_rva.to_le_bytes());
      file[data_dirs + 10 * 8 + 4..data_dirs + 10 * 8 + 8].copy_from_slice(&0x140u32.to_le_bytes());

      let sect = |file: &mut Vec<u8>, i: usize, name: &[u8], rva: u32, raw: u32| {
         let at = section_table + i * 40;
         file[at..at + name.len()].copy_from_slice(name);
         file[at + 8..at + 12].copy_from_slice(&0x200u32.to_le_bytes()); // vsize
         file[at + 12..at + 16].copy_from_slice(&rva.to_le_bytes());
         file[at + 16..at + 20].copy_from_slice(&0x200u32.to_le_bytes()); // raw size
         file[at + 20..at + 24].copy_from_slice(&raw.to_le_bytes());
         file[at + 36..at + 40].copy_from_slice(&0x4000_0040u32.to_le_bytes());
      };
      sect(&mut file, 0, b".rdata", 0x1000, RDATA_RAW as u32);
      sect(&mut file, 1, b".reloc", 0x2000, RELOC_RAW as u32);

      // One import descriptor naming "K.dll" at name_rva, then the null descriptor.
      file[RDATA_RAW + 12..RDATA_RAW + 16].copy_from_slice(&name_rva.to_le_bytes());
      let name_off = RDATA_RAW + (name_rva - 0x1000) as usize;
      file[name_off..name_off + 6].copy_from_slice(b"K.dll\0");
      // Load config: Size, then DependentLoadFlags = 0x0800 at +0x4E.
      let lcfg_off = RDATA_RAW + (lcfg_rva - 0x1000) as usize;
      file[lcfg_off..lcfg_off + 4].copy_from_slice(&0x140u32.to_le_bytes());
      file[lcfg_off + 0x4E..lcfg_off + 0x50].copy_from_slice(&0x0800u16.to_le_bytes());
      file
   }

   fn write_temp(name: &str, data: &[u8]) -> std::path::PathBuf {
      let path = std::env::temp_dir().join(format!("rubycord-patch-{}-{name}", std::process::id()));
      fs::write(&path, data).unwrap();
      path
   }

   #[test]
   fn inject_then_restore_is_the_original_file() {
      let original = build_pe();
      let exe = write_temp("exe", &original);
      let undo = write_temp("undo", b"");

      assert!(!is_patched(&exe).unwrap());
      inject(&exe, &undo).unwrap();
      assert!(is_patched(&exe).unwrap());

      // The import now names our DLL and the flag gained the app-dir bit.
      let patched = fs::read(&exe).unwrap();
      assert!(find(&patched, DLL_NAME), "rubycord.dll is imported");
      assert!(find(&patched, IMPORT_NAME), "the export is imported");
      let mut file = fs::File::open(&exe).unwrap();
      let pe = Pe::parse(&mut file).unwrap();
      let flag_off = pe.dependent_flags.unwrap();
      assert_eq!(u16(&fs::read(&exe).unwrap(), flag_off), 0x0A00);

      // A second inject is a no-op, not a double patch.
      inject(&exe, &undo).unwrap();

      assert!(restore(&exe, &undo).unwrap());
      assert_eq!(fs::read(&exe).unwrap(), original, "restored byte for byte");
      assert!(!undo.exists(), "the undo record is consumed");
      assert!(!is_patched(&exe).unwrap());
      fs::remove_file(&exe).ok();
   }

   #[test]
   fn restore_leaves_a_replaced_exe_alone() {
      let original = build_pe();
      let exe = write_temp("exe2", &original);
      let undo = write_temp("undo2", b"");
      inject(&exe, &undo).unwrap();
      // A Discord update: the exe is replaced by a fresh, unpatched one.
      fs::write(&exe, &original).unwrap();
      assert!(!restore(&exe, &undo).unwrap(), "nothing to restore");
      assert_eq!(fs::read(&exe).unwrap(), original);
      fs::remove_file(&exe).ok();
   }

   fn find(haystack: &[u8], needle: &[u8]) -> bool {
      haystack
         .windows(needle.len())
         .any(|window| window == needle)
   }
}
