//! Links a Windows `VERSIONINFO`, and optionally a manifest, into the binary
//! being built. Defender scores unversioned binaries that inject into another
//! process as droppers. Shared by the loader's and the launcher's build
//! scripts.

use std::{
   env,
   fs,
   path::PathBuf,
   process::Command,
};

#[derive(Clone, Copy)]
#[allow(dead_code, reason = "each build script uses one kind")]
pub enum Kind {
   App,
   Dll,
}

const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;

/// Does nothing off Windows. GNU `ld` takes resources only as a COFF object,
/// so a GNU build without `windres` on `PATH` goes without them.
pub fn embed(kind: Kind, file_name: &str, description: &str, manifest: Option<&str>) {
   println!("cargo:rerun-if-changed=build.rs");
   println!("cargo:rerun-if-changed=../loader/winres.rs");
   if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
      return;
   }
   let version = env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
   let mut parts = [0u16; 4];
   for (part, text) in parts.iter_mut().zip(version.split('.')) {
      *part = text.parse().unwrap_or(0);
   }
   let version = format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], parts[3]);
   let info = version_info(kind, parts, &[
      ("CompanyName", "Ruby"),
      ("FileDescription", description),
      ("FileVersion", &version),
      ("OriginalFilename", file_name),
      ("ProductName", "Ruby"),
      ("ProductVersion", &version),
   ]);

   // The empty entry every `.res` file starts with.
   let mut res: Vec<u8> = [0u32, 32, 0xFFFF, 0xFFFF]
      .iter()
      .flat_map(|word| word.to_le_bytes())
      .collect();
   res.resize(32, 0);
   resource(&mut res, RT_VERSION, &info);
   if let Some(manifest) = manifest {
      resource(&mut res, RT_MANIFEST, manifest.as_bytes());
   }

   let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
   let res_path = out.join("resources.res");
   fs::write(&res_path, &res).expect("writing resources.res");
   let link = match kind {
      Kind::App => "cargo:rustc-link-arg-bins",
      Kind::Dll => "cargo:rustc-link-arg-cdylib",
   };
   if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
      println!("{link}={}", res_path.display());
      return;
   }
   let obj_path = out.join("resources.o");
   let converted = ["x86_64-w64-mingw32-windres", "windres"]
      .iter()
      .any(|tool| {
         Command::new(tool)
            .arg("-O")
            .arg("coff")
            .arg(&res_path)
            .arg(&obj_path)
            .status()
            .is_ok_and(|status| status.success())
      });
   if converted {
      println!("{link}={}", obj_path.display());
   } else {
      println!("cargo:warning=no windres on PATH, {file_name} goes without version info");
   }
}

fn pad(bytes: &mut Vec<u8>) {
   bytes.resize(bytes.len().next_multiple_of(4), 0);
}

/// A `.res` entry with ID 1 and language en-US.
fn resource(res: &mut Vec<u8>, kind: u16, data: &[u8]) {
   let size = u32::try_from(data.len()).expect("a resource under 4 GiB");
   let header = [
      size,
      32,
      0xFFFF | u32::from(kind) << 16,
      0xFFFF | 1 << 16,
      0,
      0x0409_0030,
      0,
      0,
   ];
   res.extend(header.iter().flat_map(|word| word.to_le_bytes()));
   res.extend_from_slice(data);
   pad(res);
}

fn utf16(text: &str) -> Vec<u8> {
   text
      .encode_utf16()
      .chain([0])
      .flat_map(u16::to_le_bytes)
      .collect()
}

/// A `VERSIONINFO` block, whose length the first field holds.
fn node(key: &str, value_len: usize, text: bool, value: &[u8], children: &[Vec<u8>]) -> Vec<u8> {
   let value_len = u16::try_from(value_len).expect("a short value");
   let mut out: Vec<u8> = [0, value_len, u16::from(text)]
      .iter()
      .flat_map(|word| word.to_le_bytes())
      .collect();
   out.extend(utf16(key));
   pad(&mut out);
   out.extend_from_slice(value);
   for child in children {
      pad(&mut out);
      out.extend_from_slice(child);
   }
   let len = u16::try_from(out.len()).expect("a block under 64 KiB");
   out[..2].copy_from_slice(&len.to_le_bytes());
   out
}

fn version_info(kind: Kind, parts: [u16; 4], strings: &[(&str, &str)]) -> Vec<u8> {
   let high = u32::from(parts[0]) << 16 | u32::from(parts[1]);
   let low = u32::from(parts[2]) << 16 | u32::from(parts[3]);
   let file_type = match kind {
      Kind::App => 1,
      Kind::Dll => 2,
   };
   // `VS_FIXEDFILEINFO`: signature, struct version, file and product versions,
   // flags mask and flags, `VOS_NT_WINDOWS32`, type, subtype and date.
   let fixed: Vec<u8> = [
      0xFEEF_04BD,
      0x0001_0000,
      high,
      low,
      high,
      low,
      0x3F,
      0,
      0x0004_0004,
      file_type,
      0,
      0,
      0,
   ]
   .iter()
   .flat_map(|word: &u32| word.to_le_bytes())
   .collect();
   let strings: Vec<_> = strings
      .iter()
      .map(|(key, value)| {
         let value = utf16(value);
         node(key, value.len() / 2, true, &value, &[])
      })
      .collect();
   let table = node("040904B0", 0, true, &[], &strings);
   let translation = node("Translation", 4, false, &[0x09, 0x04, 0xB0, 0x04], &[]);
   node("VS_VERSION_INFO", fixed.len(), false, &fixed, &[
      node("StringFileInfo", 0, true, &[], &[table]),
      node("VarFileInfo", 0, true, &[], &[translation]),
   ])
}
