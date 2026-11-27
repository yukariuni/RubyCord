//! `patcher --out <dir> [--loader <libphoenix.so> [--lib
//! [<name>=]<extra.so>]... [--plugin <name.js>]...] [--frida <gadget.so[.gz]>
//! --frida-bootstrap <bootstrap.js> [--frida-script <x.js>]...] <split.apk>...`
//!
//! Writes the signed splits to `<dir>`, for `adb install-multiple <dir>/*.apk`.
//! With `--merge`, `--out` names one APK merged from the splits instead, which
//! `--package` and `--label` install next to Discord. Without `--loader` only
//! the access patches go in.
//!
//! `patcher --swap --loader <libphoenix.so> --out <file.apk> <patched.apk>`
//! replaces the loader in an APK this patcher merged, such as the one installed
//! on a phone (`adb shell pm path com.discord`), and signs it again, so a new
//! loader installs over it without the splits it was made from.

use std::{
   collections::BTreeMap,
   env,
   fs::{
      self,
      File,
   },
   io::{
      BufWriter,
      Write,
   },
   path::{
      Path,
      PathBuf,
   },
};

use misstep::{
   OptionExt,
   ResultExt,
   bail,
   ensure,
};
use patcher::{
   Apk,
   Frida,
   LOADER,
   Library,
   PackageName,
   Payload,
   Runtime,
};

const USAGE: &str = "usage: patcher --out <dir or file.apk> [--merge [--package <name>] [--label \
                     <name>]] [--loader <libphoenix.so> [--lib [<name>=]<extra.so>]... [--plugin \
                     <name.js>]...] [--frida <gadget.so[.gz]> --frida-bootstrap <bootstrap.js> \
                     [--frida-script <x.js>]...] <split.apk>...\n       patcher --swap --loader \
                     <libphoenix.so> --out <file.apk> <patched.apk>";

#[derive(Default)]
struct Args {
   out:       Option<PathBuf>,
   merge:     bool,
   swap:      bool,
   package:   Option<String>,
   label:     Option<String>,
   loader:    Option<PathBuf>,
   libraries: Vec<PathBuf>,
   plugins:   Vec<PathBuf>,
   gadget:    Option<PathBuf>,
   bootstrap: Option<PathBuf>,
   scripts:   Vec<PathBuf>,
   apks:      Vec<PathBuf>,
}

impl Args {
   fn parse() -> misstep::Result<Self> {
      let mut parsed = Self::default();
      let mut args = env::args_os().skip(1);
      while let Some(arg) = args.next() {
         let mut value = || args.next().map(PathBuf::from).context(USAGE);
         match arg.to_str() {
            Some("--out") => parsed.out = Some(value()?),
            Some("--merge") => parsed.merge = true,
            Some("--swap") => parsed.swap = true,
            Some("--package") => parsed.package = Some(text(value()?)?),
            Some("--label") => parsed.label = Some(text(value()?)?),
            Some("--loader") => parsed.loader = Some(value()?),
            Some("--lib") => parsed.libraries.push(value()?),
            Some("--plugin") => parsed.plugins.push(value()?),
            Some("--frida") => parsed.gadget = Some(value()?),
            Some("--frida-bootstrap") => parsed.bootstrap = Some(value()?),
            Some("--frida-script") => parsed.scripts.push(value()?),
            Some(flag) if flag.starts_with("--") => bail!("unknown flag {flag}\n{USAGE}"),
            _ => parsed.apks.push(PathBuf::from(arg)),
         }
      }
      ensure!(!parsed.apks.is_empty() && parsed.out.is_some(), "{USAGE}");
      ensure!(
         parsed.merge || (parsed.package.is_none() && parsed.label.is_none()),
         "--package and --label need --merge"
      );
      ensure!(
         !parsed.swap || (parsed.loader.is_some() && parsed.apks.len() == 1 && !parsed.merge),
         "--swap takes --loader and one patched APK\n{USAGE}"
      );
      Ok(parsed)
   }

   fn payload(&self) -> misstep::Result<Payload> {
      let runtime = self
         .loader
         .as_ref()
         .map(|loader| -> misstep::Result<Runtime> {
            let mut libraries = vec![Library::new(LOADER, read(loader)?)?];
            for arg in &self.libraries {
               let text = arg.to_str().context("--lib is not UTF-8")?;
               let (name, path) = match text.split_once('=') {
                  Some((name, path)) => (name.to_owned(), Path::new(path)),
                  None => (file_name(arg)?, arg.as_path()),
               };
               libraries.push(Library::new(name, read(path)?)?);
            }
            let mut plugins = BTreeMap::new();
            for path in &self.plugins {
               let name = path
                  .file_stem()
                  .and_then(|stem| stem.to_str())
                  .context("plugin path has no UTF-8 file name")?;
               plugins.insert(name.to_owned(), read_text(path)?);
            }
            Ok(Runtime { libraries, plugins })
         })
         .transpose()?;

      let frida = self
         .gadget
         .as_ref()
         .map(|gadget| -> misstep::Result<Frida> {
            let bootstrap = self
               .bootstrap
               .as_ref()
               .context("--frida needs --frida-bootstrap")?;
            let scripts = self
               .scripts
               .iter()
               .map(|path| Ok((file_name(path)?, read_text(path)?)))
               .collect::<misstep::Result<Vec<(String, String)>>>()?;
            let script = patcher::frida::script(&scripts, &read_text(bootstrap)?);
            Ok(Frida::new(read(gadget)?, script)?)
         })
         .transpose()?;
      Ok(Payload { runtime, frida })
   }
}

fn text(path: PathBuf) -> misstep::Result<String> {
   path
      .into_os_string()
      .into_string()
      .ok()
      .context("argument is not UTF-8")
}

fn file_name(path: &Path) -> misstep::Result<String> {
   Ok(path
      .file_name()
      .and_then(|name| name.to_str())
      .context("path has no UTF-8 file name")?
      .to_owned())
}

fn read(path: &Path) -> misstep::Result<Vec<u8>> {
   Ok(fs::read(path).with_context(|| format!("reading {}", path.display()))?)
}

fn read_text(path: &Path) -> misstep::Result<String> {
   Ok(fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?)
}

fn main() -> misstep::Result<()> {
   misstep::install_panic_hook();
   let args = Args::parse()?;
   let out = args.out.as_deref().context(USAGE)?;
   if args.swap {
      let loader = Library::new(LOADER, read(args.loader.as_deref().context(USAGE)?)?)?;
      let apk = read(&args.apks[0])?;
      save(out, &patcher::swap_loader(&apk, &loader)?)?;
      println!("wrote {}", out.display());
      return Ok(());
   }
   let prepared = args.payload()?.prepare()?;
   let mut log = |line: &str| println!("{line}");

   if args.merge {
      let package = match &args.package {
         Some(name) => name.parse::<PackageName>()?,
         None => PackageName::default(),
      };
      let mut apks = args
         .apks
         .iter()
         .map(|path| read(path))
         .collect::<misstep::Result<Vec<Vec<u8>>>>()?;
      let apk = patcher::merge(
         &mut apks,
         &prepared,
         &package,
         args.label.as_deref(),
         &mut log,
      )?;
      save(out, &apk)?;
      println!("wrote {}", out.display());
      return Ok(());
   }

   let mut apks = args
      .apks
      .iter()
      .map(|path| Ok((file_name(path)?, read(path)?)))
      .collect::<misstep::Result<Vec<(String, Vec<u8>)>>>()?;
   let signed = patcher::splits(&mut apks, &prepared, &mut log)?;
   fs::create_dir_all(out)?;
   for (name, apk) in signed {
      save(&out.join(name), &apk)?;
   }
   Ok(())
}

fn save(path: &Path, apk: &Apk<'_>) -> misstep::Result<()> {
   let mut file =
      BufWriter::new(File::create(path).with_context(|| format!("writing {}", path.display()))?);
   for part in apk.parts() {
      file.write_all(part)?;
   }
   file.flush()?;
   Ok(())
}
