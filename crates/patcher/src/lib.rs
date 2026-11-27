//! Patches Discord's split APKs so Phoenix starts with phoenix inside it, for
//! the command line and, built to wasm, the site.
//!
//! - The base split's launcher always picks Phoenix.
//! - The host library's access gate returns true, and its `DT_NEEDED`
//!   `liblog.so` becomes the loader.
//! - The loader, the libraries it needs and the plugin bundle land next to the
//!   host library in its ABI split.
//! - Output is signed with phoenix's key, either split by split for `adb
//!   install-multiple` or merged into one APK, which is what a browser download
//!   can install.

mod arsc;
mod bytes;
mod dex;
pub mod frida;
mod host;
pub mod ios;
mod manifest;
mod sign;
pub mod zip;

use std::{
   borrow::Cow,
   collections::{
      BTreeMap,
      HashSet,
   },
   fmt::{
      self,
      Write,
   },
   io::Read,
   str::FromStr,
};

use flate2::read::GzDecoder;
use phoenix_sigs::{
   PLUGIN_BUNDLE,
   file::{
      FileError,
      Machine,
   },
};

use crate::zip::{
   Entry,
   Method,
   Record,
};

/// The numbers Phoenix's host name table gives `names` in `apk`.
///
/// `apk` is the base APK or one merged from the splits, and a name the build
/// lacks gets `None`. Host calls, events and their fields go by these numbers,
/// which a Discord build can renumber.
pub fn host_names(apk: &[u8], names: &[&str]) -> Result<Vec<Option<u16>>, Error> {
   let mut numbers = vec![None; names.len()];
   for record in zip::read(apk)? {
      let is_dex = record
         .name
         .strip_prefix("classes")
         .and_then(|rest| rest.strip_suffix(".dex"))
         .is_some();
      if is_dex {
         dex::host_numbers(record.entry(apk).data()?.into_owned(), names, &mut numbers)?;
      }
   }
   Ok(numbers)
}

/// The loader's name in the APK.
pub const LOADER: &str = host::INJECTED;

#[derive(Debug, flub::Error)]
pub enum Error {
   #[error("input truncated")]
   Truncated,
   #[error("a size or offset passes 4 GiB")]
   Overflow,
   #[error("{0}")]
   Zip(String),
   #[error("{name}: {source}")]
   Inflate {
      name:   String,
      source: std::io::Error,
   },
   #[error("dex: {0}")]
   Dex(String),
   #[error("dex: {0}")]
   Parse(pokedex::error::Error),
   #[error("{0}")]
   Host(String),
   #[error(transparent)]
   Elf(FileError),
   #[error("resources.arsc: {0}")]
   Resources(String),
   #[error("AndroidManifest.xml: {0}")]
   Manifest(String),
   #[error("signing key: {0}")]
   Key(String),
   #[error("{name}: {source}")]
   Apk { name: String, source: Box<Self> },
   #[error(
      "incomplete, launcher patched: {dex}, host library patched: {host}. Pass base.apk and the \
       ABI split."
   )]
   Incomplete { dex: bool, host: bool },
   #[error("{0}")]
   Input(String),
}

/// A library to add next to the host library of the same machine.
pub struct Library {
   name:    String,
   machine: Machine,
   bytes:   Vec<u8>,
}

impl Library {
   pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> Result<Self, Error> {
      let name = name.into();
      Ok(Self {
         machine: host::machine(&name, &bytes)?,
         name,
         bytes,
      })
   }
}

/// RubyCord's plugin runtime: the loader, the libraries it needs and the
/// plugins to bundle, by name.
pub struct Runtime {
   pub libraries: Vec<Library>,
   pub plugins:   BTreeMap<String, String>,
}

pub struct Frida {
   gadget: Library,
   script: String,
}

impl Frida {
   /// `gadget` may be gzipped, the way the site ships it. `script` comes from
   /// [`frida::script`].
   pub fn new(gadget: Vec<u8>, script: String) -> Result<Self, Error> {
      let gadget = if gadget.starts_with(&[0x1F, 0x8B]) {
         let mut unpacked = Vec::new();
         GzDecoder::new(&*gadget)
            .read_to_end(&mut unpacked)
            .map_err(|source| {
               Error::Inflate {
                  name: frida::GADGET.to_owned(),
                  source,
               }
            })?;
         unpacked
      } else {
         gadget
      };
      Ok(Self {
         gadget: Library::new(frida::GADGET, gadget)?,
         script,
      })
   }
}

/// What goes into the APK beyond the access patches.
#[derive(Default)]
pub struct Payload {
   pub runtime: Option<Runtime>,
   pub frida:   Option<Frida>,
}

/// An Android package name, which also has to fit the 127 UTF-16 units of a
/// resource table's package name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageName(String);

impl PackageName {
   fn is_original(&self) -> bool {
      self.0 == manifest::ORIGINAL
   }
}

impl Default for PackageName {
   fn default() -> Self {
      Self(manifest::ORIGINAL.to_owned())
   }
}

impl FromStr for PackageName {
   type Err = Error;

   fn from_str(name: &str) -> Result<Self, Error> {
      let part = |part: &str| {
         let mut chars = part.chars();
         chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && chars.all(|char| char.is_ascii_alphanumeric() || char == '_')
      };
      if name.len() > 127 || name.split('.').count() < 2 || !name.split('.').all(part) {
         return Err(Error::Input(format!(
            "invalid package name {name}, it must look like com.example.app"
         )));
      }
      Ok(Self(name.to_owned()))
   }
}

impl fmt::Display for PackageName {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str(&self.0)
   }
}

/// A [`Payload`] ready to add: the loader loads the gadget when both ride
/// along, and the plugins are serialized once. Output borrows its libraries.
pub struct Prepared {
   needed:    Option<&'static str>,
   libraries: Vec<Library>,
   bundle:    Option<Vec<u8>>,
   plugins:   Option<Vec<String>>,
   frida:     Option<Frida>,
}

impl Payload {
   pub fn prepare(self) -> Result<Prepared, Error> {
      let Self { runtime, frida } = self;
      let Some(Runtime {
         mut libraries,
         plugins,
      }) = runtime
      else {
         return Ok(Prepared {
            needed: frida.as_ref().map(|_| frida::GADGET),
            libraries: Vec::new(),
            bundle: None,
            plugins: None,
            frida,
         });
      };
      if frida.is_some() {
         for library in libraries
            .iter_mut()
            .filter(|library| library.name == LOADER)
         {
            host::rename_needed(&library.name, &mut library.bytes, frida::GADGET)?;
         }
      }
      let bundle = serde_json::to_vec(&plugins)
         .map_err(|source| Error::Input(format!("plugin bundle: {source}")))?;
      Ok(Prepared {
         needed: Some(LOADER),
         libraries,
         bundle: Some(bundle),
         plugins: Some(plugins.into_keys().collect()),
         frida,
      })
   }
}

#[derive(Default)]
struct Progress {
   dex:  bool,
   host: Vec<Machine>,
}

impl Progress {
   const fn check(&self) -> Result<(), Error> {
      if self.dex && !self.host.is_empty() {
         return Ok(());
      }
      Err(Error::Incomplete {
         dex:  self.dex,
         host: !self.host.is_empty(),
      })
   }
}

/// JAR signature files and the source stamp, both stale once patched.
fn is_stale(name: &str) -> bool {
   if name == "stamp-cert-sha256" {
      return true;
   }
   let Some(file) = name.strip_prefix("META-INF/") else {
      return false;
   };
   !file.contains('/')
      && (file == "MANIFEST.MF"
         || [".SF", ".RSA", ".DSA", ".EC"]
            .iter()
            .any(|suffix| file.ends_with(suffix)))
}

/// One split's entries with the launcher forced in the first dex that holds
/// it, every host library patched and the payload added next to each. A stored
/// host library is patched in `apk` itself, which saves a 100 MB copy.
fn patch_split<'a>(
   apk: &'a mut [u8],
   prepared: &'a Prepared,
   progress: &mut Progress,
   log: &mut dyn FnMut(&str),
) -> Result<Vec<Entry<'a>>, Error> {
   let records = zip::read(apk)?;
   let mut replaced: Vec<Option<Option<Vec<u8>>>> = vec![None; records.len()];
   let mut dirs = Vec::new();

   for (index, record) in records.iter().enumerate() {
      if is_stale(&record.name) {
         continue;
      }
      let name = &record.name;
      if let Some(dir) = name
         .strip_suffix(host::LIBRARY)
         .filter(|dir| dir.starts_with("lib/"))
      {
         let (patched, owned) = if record.method == Method::Stored {
            let data = &mut apk[record.data.clone()];
            (host::patch(name, data, prepared.needed)?, None)
         } else {
            let mut data = record.entry(apk).data()?.into_owned();
            (host::patch(name, &mut data, prepared.needed)?, Some(data))
         };
         let renamed = prepared
            .needed
            .map_or_else(String::new, |needed| format!(", liblog.so -> {needed}"));
         log(&format!(
            "{name}: access evaluator {:#x} returns true{renamed}",
            patched.access
         ));
         dirs.push((dir.to_owned(), patched.machine));
         progress.host.push(patched.machine);
         replaced[index] = Some(owned);
      } else if !progress.dex
         && name
            .strip_prefix("classes")
            .and_then(|rest| rest.strip_suffix(".dex"))
            .is_some()
         && let Some(data) = dex::patch(record.entry(apk).data()?.into_owned())?
      {
         log(&format!("{name}: launcher returns true"));
         progress.dex = true;
         replaced[index] = Some(Some(data));
      }
   }

   let apk: &'a [u8] = apk;
   let mut out = Vec::with_capacity(records.len() + 8);
   for (record, replacement) in records.iter().zip(replaced) {
      if is_stale(&record.name) {
         continue;
      }
      out.push(match replacement {
         None => record.entry(apk),
         Some(None) => Entry::stored(record.name.clone(), &apk[record.data.clone()])?,
         Some(Some(data)) => Entry::stored(record.name.clone(), data)?,
      });
   }

   let names = records
      .iter()
      .map(|record| record.name.as_str())
      .collect::<HashSet<&str>>();
   for (dir, machine) in dirs {
      let mut add = |name: &str, data: Cow<'a, [u8]>| -> Result<(), Error> {
         let path = format!("{dir}{name}");
         if names.contains(path.as_str()) {
            return Err(Error::Input(format!("{path} is already in the APK")));
         }
         log(&format!("added {path}"));
         out.push(Entry::stored(path, data)?);
         Ok(())
      };
      if let Some(bundle) = &prepared.bundle {
         let libraries = prepared
            .libraries
            .iter()
            .filter(|library| library.machine == machine);
         if !libraries.clone().any(|library| library.name == LOADER) {
            return Err(Error::Input(format!(
               "no {LOADER} (loader) built for {dir}"
            )));
         }
         for library in libraries {
            add(&library.name, Cow::Borrowed(&library.bytes))?;
         }
         add(PLUGIN_BUNDLE, Cow::Borrowed(bundle))?;
      }
      if let Some(frida) = &prepared.frida {
         if frida.gadget.machine != machine {
            return Err(Error::Input(format!("frida-gadget is not built for {dir}")));
         }
         for (name, data) in frida::files(&frida.gadget.bytes, &frida.script) {
            add(name, data)?;
         }
      }
   }
   Ok(out)
}

/// A signed APK as parts that borrow the inputs and the [`Prepared`] payload,
/// in file order.
pub struct Apk<'a>(Vec<Cow<'a, [u8]>>);

impl<'a> Apk<'a> {
   pub fn parts(&self) -> impl Iterator<Item = &[u8]> {
      self.0.iter().map(|part| &**part)
   }

   /// The parts, borrowed where they come from the inputs or the payload.
   #[must_use]
   pub fn into_parts(self) -> Vec<Cow<'a, [u8]>> {
      self.0
   }
}

/// Swaps the loader of an APK this patcher made, one merged from the splits,
/// for `loader` and signs it again, so the install updates in place.
pub fn swap_loader<'a>(apk: &'a [u8], loader: &'a Library) -> Result<Apk<'a>, Error> {
   let mut swapped = false;
   let mut entries = Vec::new();
   for record in zip::read(apk)? {
      let entry = record.entry(apk);
      let is_loader = record
         .name
         .strip_suffix(LOADER)
         .is_some_and(|dir| dir.starts_with("lib/") && dir.ends_with('/'));
      if is_loader && host::machine(&record.name, &entry.data()?)? == loader.machine {
         entries.push(Entry::stored(record.name, &loader.bytes[..])?);
         swapped = true;
      } else {
         entries.push(entry);
      }
   }
   if !swapped {
      return Err(Error::Input(format!(
         "no {LOADER} for this machine in the APK"
      )));
   }
   signed(entries)
}

fn signed(entries: Vec<Entry<'_>>) -> Result<Apk<'_>, Error> {
   let mut archive = zip::write(entries)?;
   sign::sign(&mut archive)?;
   Ok(Apk(archive.parts))
}

/// Patches and signs every split on its own, for `adb install-multiple`.
/// Returns the signed splits by name, in input order.
pub fn splits<'a>(
   apks: &'a mut [(String, Vec<u8>)],
   prepared: &'a Prepared,
   log: &mut dyn FnMut(&str),
) -> Result<Vec<(&'a str, Apk<'a>)>, Error> {
   let mut progress = Progress::default();
   let patched = apks
      .iter_mut()
      .map(|(name, apk)| {
         patch_split(apk, prepared, &mut progress, log)
            .map(|entries| (name.as_str(), entries))
            .map_err(|source| {
               Error::Apk {
                  name:   name.clone(),
                  source: Box::new(source),
               }
            })
      })
      .collect::<Result<Vec<_>, Error>>()?;
   progress.check()?;

   patched
      .into_iter()
      .map(|(name, entries)| {
         let apk = signed(entries)?;
         log(&format!("signed {name}"));
         Ok((name, apk))
      })
      .collect()
}

enum Role {
   Base,
   Abi,
   Config,
   Other,
}

fn role(records: &[Record]) -> Role {
   let has = |wanted: &dyn Fn(&str) -> bool| records.iter().any(|record| wanted(&record.name));
   if has(&|name| name == "classes.dex") {
      Role::Base
   } else if has(&|name| name.starts_with("lib/") && name.ends_with(host::LIBRARY)) {
      Role::Abi
   } else if has(&|name| name == "resources.arsc") {
      Role::Config
   } else {
      Role::Other
   }
}

/// Patches base and the ABI split into one signed APK with the config splits'
/// resources.
///
/// It installs as `package` and, with `label`, shows under that name. The
/// splits come in any order, told apart by their contents.
pub fn merge<'a>(
   apks: &'a mut [Vec<u8>],
   prepared: &'a Prepared,
   package: &PackageName,
   label: Option<&str>,
   log: &mut dyn FnMut(&str),
) -> Result<Apk<'a>, Error> {
   let (mut base, mut abi, mut configs) = (None, None, Vec::new());
   for apk in apks.iter_mut() {
      match role(&zip::read(apk)?) {
         Role::Base if base.is_none() => base = Some(apk),
         Role::Abi if abi.is_none() => abi = Some(apk),
         Role::Base | Role::Abi => {
            return Err(Error::Input("pass one base and one ABI split".into()));
         },
         Role::Config => configs.push(&**apk),
         Role::Other => log("skipped a file that is not a Discord split"),
      }
   }
   let (Some(base), Some(abi)) = (base, abi) else {
      return Err(Error::Input("need base.apk and the ABI split".into()));
   };

   log(&match &prepared.plugins {
      None => "plugin runtime: off".to_owned(),
      Some(names) if names.is_empty() => "plugins: none".to_owned(),
      Some(names) => format!("plugins: {}", names.join(", ")),
   });
   let mut progress = Progress::default();
   let base_entries = patch_split(base, prepared, &mut progress, log)?;
   let abi_entries = patch_split(abi, prepared, &mut progress, log)?;
   progress.check()?;

   let config_entries = configs
      .iter()
      .map(|config| {
         Ok(zip::read(config)?
            .iter()
            .map(|record| record.entry(config))
            .collect())
      })
      .collect::<Result<Vec<Vec<Entry<'_>>>, Error>>()?;
   let mut names = base_entries
      .iter()
      .map(|entry| entry.name.clone())
      .collect::<HashSet<String>>();

   let mut out = Vec::with_capacity(base_entries.len() + abi_entries.len());
   for entry in base_entries {
      match entry.name.as_str() {
         "AndroidManifest.xml" => {
            let patched = manifest::patch(&entry.data()?, &package.0, label)?;
            let mut line = format!(
               "manifest: removed {} split attribute(s), splits.required=false: {}",
               patched.removed, patched.flipped
            );
            if patched.added_permission {
               line.push_str(", +REQUEST_INSTALL_PACKAGES");
            }
            if patched.renamed > 0 {
               let _ = write!(line, ", {} value(s) renamed to {package}", patched.renamed);
               if let Some(shown) = label {
                  let _ = write!(line, " / \"{shown}\"");
               }
            }
            log(&line);
            out.push(Entry::stored(entry.name, patched.data)?);
         },
         "resources.arsc" => {
            let tables = config_entries
               .iter()
               .filter_map(|entries| entries.iter().find(|entry| entry.name == "resources.arsc"))
               .map(Entry::data)
               .collect::<Result<Vec<Cow<'_, [u8]>>, Error>>()?;
            let rename = (!package.is_original()).then_some(package.0.as_str());
            let merged = arsc::merge(&entry.data()?, &tables, rename)?;
            log(&format!(
               "resources: merged {} config split table(s)",
               tables.len()
            ));
            out.push(Entry::stored(entry.name, merged)?);
         },
         _ => out.push(entry),
      }
   }
   out.extend(
      abi_entries
         .into_iter()
         .filter(|entry| entry.name.starts_with("lib/") && !names.contains(&entry.name)),
   );
   let mut added = 0;
   for entry in config_entries.into_iter().flatten() {
      if entry.name.starts_with("res/") && names.insert(entry.name.clone()) {
         out.push(entry);
         added += 1;
      }
   }
   log(&format!(
      "resources: added {added} file(s) from config splits"
   ));

   log("signing");
   signed(out)
}
