//! End-to-end demo against the stand-in.
//!
//! `stand-in-demo` builds the stand-in and the loader in release, bundles the
//! example plugin with esbuild, then runs the scenario below in this process.
//!
//! `stand-in-demo run <libstand_in.so> <libphoenix.so> <home>` runs the
//! scenario alone, which is how it runs on a phone over adb. It loads the
//! stand-in the way Android loads Phoenix, loads the loader after it the way a
//! `DT_NEEDED` entry would, and feeds the stand-in two gateway frames.

use std::{
   env,
   ffi::{
      CStr,
      CString,
      c_void,
   },
   fs,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use misstep::{
   OptionExt,
   ResultExt,
   bail,
   report,
};

type Receive = unsafe extern "C" fn(*const u8, usize) -> bool;

const FRAMES: &[&str] = &[
   r#"{"t":"MESSAGE_CREATE","d":{"id":"1","channel_id":"general","content":"hello from the gateway","author":{"id":"7","username":"wumpus"},"timestamp":"2026-11-22T00:00:00Z"}}"#,
   r#"{"t":"MESSAGE_CREATE","d":{"id":"2","channel_id":"general","content":"second message","author":{"id":"8","username":"nelly"},"timestamp":"2026-11-22T00:00:01Z"}}"#,
];

fn main() -> misstep::Result<()> {
   misstep::install_panic_hook();
   let args = env::args().skip(1).collect::<Vec<String>>();
   match args.as_slice() {
      [] => build_and_run(),
      [run, stand_in, loader, home] if run == "run" => {
         scenario(Path::new(stand_in), Path::new(loader), Path::new(home))
      },
      _ => bail!("usage: stand-in-demo [run <libstand_in.so> <libphoenix.so> <home>]"),
   }
}

fn build_and_run() -> misstep::Result<()> {
   let root = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("../..")
      .canonicalize()?;
   let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
   let status = Command::new(&cargo)
      .current_dir(&root)
      .args([
         "build",
         "--release",
         "-p",
         "stand-in",
         "-p",
         "phoenix-loader",
      ])
      .status()
      .context("running cargo")?;
   if !status.success() {
      bail!("cargo build failed with {status}");
   }

   let target = root.join("target");
   let home = target.join("phoenix-home");
   match fs::remove_dir_all(&home) {
      Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
         return Err(report!(err).context(format!("clearing {}", home.display())));
      },
      _ => {},
   }
   let bundle = home.join("plugins/example.js");
   let status = Command::new("esbuild")
      .current_dir(root.join("plugins/example"))
      .args([
         "src/index.ts",
         "--bundle",
         "--format=iife",
         "--target=es2023",
         "--log-level=warning",
      ])
      .arg(format!("--outfile={}", bundle.display()))
      .status()
      .context("running esbuild, enter the devshell with `nix develop`")?;
   if !status.success() {
      bail!("esbuild failed with {status}");
   }

   let release = target.join("release");
   scenario(
      &release.join("libstand_in.so"),
      &release.join("libphoenix.so"),
      &home,
   )
}

fn scenario(stand_in: &Path, loader: &Path, home: &Path) -> misstep::Result<()> {
   // SAFETY: no other threads exist yet.
   unsafe { env::set_var("PHOENIX_HOME", home) };

   let host = open(stand_in)?;
   // SAFETY: the symbol is the stand-in's exported receive function with this
   // signature.
   let receive: Receive = unsafe {
      let symbol = libc::dlsym(host, c"stand_in_gateway_receive".as_ptr());
      std::mem::transmute::<*mut c_void, Receive>(
         (!symbol.is_null())
            .then_some(symbol)
            .context("stand_in_gateway_receive is missing")?,
      )
   };

   println!("loading {}", loader.display());
   open(loader)?;

   for frame in FRAMES {
      // SAFETY: the frame is a live byte slice.
      if !unsafe { receive(frame.as_ptr(), frame.len()) } {
         bail!("the stand-in rejected {frame}");
      }
   }
   let storage = fs::read_to_string(home.join("storage/example.json")).unwrap_or_default();
   println!("example plugin storage: {storage}");
   Ok(())
}

fn open(path: &Path) -> misstep::Result<*mut c_void> {
   let path: PathBuf = path
      .canonicalize()
      .with_context(|| format!("{}", path.display()))?;
   let name = CString::new(path.as_os_str().as_encoded_bytes())?;
   // SAFETY: plain FFI call with a NUL-terminated path.
   let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
   if handle.is_null() {
      // SAFETY: dlerror returns a NUL-terminated message after a failed dlopen.
      let message = unsafe { CStr::from_ptr(libc::dlerror()) }.to_string_lossy();
      return Err(report!("dlopen {}: {message}", path.display()));
   }
   Ok(handle)
}
