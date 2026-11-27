//! `libphoenix.so` / `phoenix.dll`, injected through `DT_NEEDED` on Android,
//! as a dylib on iOS, or loaded into Desktop Phoenix on Windows and, through
//! `LD_PRELOAD` on Linux and `DYLD_INSERT_LIBRARIES` on macOS.
//!
//! On Unix the constructor runs once the host library is mapped and relocated.
//! Desktop hooks Discord synchronously, from `DllMain` or the constructor, then
//! starts on a thread so startup never runs under the OS loader lock. Plugins
//! come from the bundle stored beside this library (when present) plus
//! `$PHOENIX_HOME/plugins`.

use std::{
   env,
   panic::{
      self,
      AssertUnwindSafe,
   },
   path::PathBuf,
};

use phoenix_runtime::{
   Error,
   Plugin,
   Started,
   console::Tee,
};

#[cfg(unix)]
#[used]
#[cfg_attr(not(target_vendor = "apple"), unsafe(link_section = ".init_array"))]
#[cfg_attr(
   target_vendor = "apple",
   unsafe(link_section = "__DATA,__mod_init_func")
)]
static CONSTRUCTOR: extern "C" fn() = init;

/// # Safety
///
/// Only the Windows loader calls this, under its loader lock.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(
   module: *mut std::ffi::c_void,
   reason: u32,
   _reserved: *mut std::ffi::c_void,
) -> i32 {
   const DLL_PROCESS_ATTACH: u32 = 1;

   #[link(name = "kernel32")]
   unsafe extern "system" {
      fn DisableThreadLibraryCalls(module: *mut std::ffi::c_void) -> i32;
   }

   if reason == DLL_PROCESS_ATTACH {
      // SAFETY: `module` is the HINSTANCE handed to DllMain.
      unsafe { DisableThreadLibraryCalls(module) };
      desktop();
   }
   1
}

#[cfg(unix)]
extern "C" fn init() {
   #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
   if linux::claim() {
      desktop();
   }
   #[cfg(target_os = "macos")]
   if macos::claim() {
      desktop();
   }
   #[cfg(target_os = "ios")]
   {
      logging();
      ios::start_with_phoenix();
   }
   #[cfg(not(any(
      target_os = "ios",
      target_os = "macos",
      all(target_os = "linux", target_arch = "x86_64")
   )))]
   {
      logging();
      run();
   }
}

/// Hooks Discord before its own code runs, since Desktop Phoenix settles on
/// the access gate and reads its HTTP vtables in its first frames, then starts
/// phoenix on a thread.
#[cfg(any(
   windows,
   target_os = "macos",
   all(target_os = "linux", target_arch = "x86_64")
))]
fn desktop() {
   logging();
   if env::var_os("RUBY_GATE_ONLY").is_some() {
      match phoenix_runtime_desktop::open_gate_only() {
         Ok(()) => log::info!("RUBY_GATE_ONLY: only the access gate is open"),
         Err(err) => log::error!("phoenix could not open the access gate: {err}"),
      }
      return;
   }
   let started = std::time::Instant::now();
   if let Err(err) = phoenix_runtime_desktop::prepare() {
      log::error!("phoenix could not hook Discord: {err}");
   }
   log::info!("hooked Discord in {:?}", started.elapsed());
   let _ = std::thread::Builder::new()
      .name("ruby-init".to_owned())
      .spawn(run);
}

fn run() {
   // Unwinding out of a constructor aborts the host, so a bug here costs
   // plugins, not the app.
   match panic::catch_unwind(AssertUnwindSafe(|| start(home(), bundled()))) {
      Ok(Ok(started)) => {
         log::info!(
            "phoenix started on {} ({}), plugins: {:?}",
            started.target,
            started
               .build
               .map_or_else(|| "no build ID".to_owned(), |id| id.to_string()),
            started.plugins,
         );
      },
      Ok(Err(err)) => log::error!("phoenix did not start: {err}"),
      Err(_) => log::error!("phoenix panicked while starting"),
   }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn start(home: PathBuf, bundled: Vec<Plugin>) -> Result<Started, Error> {
   phoenix_runtime_mobile::start(home, bundled)
}

#[cfg(any(
   windows,
   target_os = "macos",
   all(target_os = "linux", target_arch = "x86_64")
))]
fn start(home: PathBuf, bundled: Vec<Plugin>) -> Result<Started, Error> {
   phoenix_runtime_desktop::start(home, bundled)
}

#[cfg(not(any(
   target_os = "android",
   target_os = "ios",
   target_os = "macos",
   windows,
   all(target_os = "linux", target_arch = "x86_64")
)))]
fn start(_home: PathBuf, _bundled: Vec<Plugin>) -> Result<Started, Error> {
   Err(Error::NoTarget)
}

fn home() -> PathBuf {
   if let Some(home) = env::var_os("PHOENIX_HOME") {
      return PathBuf::from(home);
   }
   #[cfg(target_os = "ios")]
   {
      // Beside Phoenix's `store`, which `files_dir` expects to be the parent.
      env::var_os("HOME")
         .map_or_else(env::temp_dir, PathBuf::from)
         .join("Library/Application Support/phoenix")
   }
   #[cfg(target_os = "linux")]
   {
      // Sits directly under Linux Phoenix's data directory, which holds
      // `app.lock`, `modules/` and `store/<account>/cache.sqlite`, so its
      // parent is the `store` root.
      let data = env::var_os("LIBDISCORD_DATA_DIR").map_or_else(
         || {
            env::var_os("XDG_DATA_HOME")
               .filter(|dir| !dir.is_empty())
               .map(PathBuf::from)
               .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
               .unwrap_or_else(env::temp_dir)
               .join("discord-phoenix")
         },
         PathBuf::from,
      );
      data.join("ruby")
   }
   #[cfg(target_os = "macos")]
   {
      // Sits directly under macOS Phoenix's data directory, which holds
      // `app.lock`, `modules/` and `store/<account>/cache.sqlite`, so its
      // parent is the `store` root.
      let data = env::var_os("LIBDISCORD_DATA_DIR").map_or_else(
         || {
            env::var_os("HOME")
               .map_or_else(env::temp_dir, PathBuf::from)
               .join("Library/Application Support/libdiscord")
         },
         PathBuf::from,
      );
      data.join("ruby")
   }
   #[cfg(all(
      unix,
      not(any(target_os = "ios", target_os = "macos", target_os = "linux"))
   ))]
   {
      // /proc/self/cmdline starts with the package name, plus `:process` for
      // secondary processes.
      let package = std::fs::read("/proc/self/cmdline")
         .ok()
         .and_then(|cmdline| {
            let name = cmdline.split(|byte| *byte == 0).next()?.to_vec();
            String::from_utf8(name).ok()
         })
         .map_or_default(|name| name.split(':').next().unwrap_or_default().to_owned());
      // SAFETY: `getuid` has no preconditions.
      let user = unsafe { libc::getuid() } / 100_000;
      let lib_path = unix::library_path().and_then(|path| path.to_str().ok());
      android_files_dir(&package, user, lib_path).join("phoenix")
   }
   #[cfg(windows)]
   {
      // Sits directly under `%LOCALAPPDATA%\DiscordPhoenix\ruby` so its
      // parent matches Phoenix's `store/<account>/cache.sqlite` root.
      let base = env::var_os("LOCALAPPDATA").map_or_else(env::temp_dir, PathBuf::from);
      base.join("DiscordPhoenix").join("ruby")
   }
}

#[cfg_attr(
   not(unix),
   allow(
      clippy::missing_const_for_fn,
      reason = "only the unix body reads a bundle"
   )
)]
fn bundled() -> Vec<Plugin> {
   #[cfg(unix)]
   {
      match unix::read_bundle() {
         Ok(plugins) => plugins,
         Err(err) => {
            log::error!("plugin bundle: {err}");
            Vec::new()
         },
      }
   }
   #[cfg(not(unix))]
   {
      Vec::new()
   }
}

#[cfg(unix)]
mod unix {
   use std::{
      collections::BTreeMap,
      ffi::{
         CStr,
         c_void,
      },
      fs::{
         self,
         File,
      },
      io::{
         ErrorKind,
         Read,
      },
      path::Path,
   };

   use misstep::ResultExt;
   use phoenix_runtime::Plugin;
   use phoenix_sigs::PLUGIN_BUNDLE;
   use zip::{
      ZipArchive,
      result::ZipError,
   };

   /// This library's path as the dynamic loader opened it.
   pub fn library_path() -> Option<&'static CStr> {
      let mut info = libc::Dl_info {
         dli_fname: std::ptr::null(),
         dli_fbase: std::ptr::null_mut(),
         dli_sname: std::ptr::null(),
         dli_saddr: std::ptr::null_mut(),
      };
      // SAFETY: dladdr fills `info` for an address inside a loaded module.
      if unsafe { libc::dladdr(super::init as *const c_void, &raw mut info) } == 0
         || info.dli_fname.is_null()
      {
         return None;
      }
      // SAFETY: dli_fname is NUL-terminated and lives as long as the module.
      Some(unsafe { CStr::from_ptr(info.dli_fname) })
   }

   /// Reads [`PLUGIN_BUNDLE`] from beside this library. Android maps native
   /// libraries straight out of the APK, so `dladdr` names
   /// `<apk>!/lib/<abi>/...` and the bundle is a stored entry in the same
   /// split.
   pub fn read_bundle() -> misstep::Result<Vec<Plugin>> {
      let Some(path) = library_path() else {
         return Ok(Vec::new());
      };
      let path = path.to_str()?;

      let bytes = if let Some((apk, inner)) = path.split_once("!/") {
         let dir = inner.rsplit_once('/').map_or("", |(dir, _)| dir);
         let mut archive =
            ZipArchive::new(File::open(apk).with_context(|| format!("opening {apk}"))?)?;
         let mut entry = match archive.by_name(&format!("{dir}/{PLUGIN_BUNDLE}")) {
            Ok(entry) => entry,
            Err(ZipError::FileNotFound) => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
         };
         let mut bytes = Vec::new();
         entry.read_to_end(&mut bytes)?;
         bytes
      } else {
         match fs::read(Path::new(path).with_file_name(PLUGIN_BUNDLE)) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
         }
      };
      let plugins: BTreeMap<String, String> = serde_json::from_slice(&bytes)?;
      Ok(plugins
         .into_iter()
         .map(|(name, source)| Plugin { name, source })
         .collect())
   }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod linux {
   use std::{
      env,
      ffi::OsStr,
      os::unix::ffi::OsStrExt,
      path::Path,
   };

   /// The Linux client's executable. `AppRun` is a shell script, so the
   /// preload reaches it and every process it starts.
   const CLIENT: &str = "discord-phoenix";

   /// Whether this process is the client. If so, takes this library out of
   /// `LD_PRELOAD` so the client's children (`discord_media`, the CEF helper,
   /// `xdg-open`) never load it.
   pub fn claim() -> bool {
      let client = std::fs::read_link("/proc/self/exe")
         .is_ok_and(|exe| exe.file_name() == Some(OsStr::new(CLIENT)));
      if client && let Some(path) = super::unix::library_path() {
         unpreload(Path::new(OsStr::from_bytes(path.to_bytes())));
      }
      client
   }

   fn unpreload(ours: &Path) {
      let Some(preload) = env::var_os("LD_PRELOAD") else {
         return;
      };
      let kept = preload
         .as_bytes()
         .split(|byte| matches!(byte, b':' | b' '))
         .filter(|entry| {
            !entry.is_empty() && Path::new(OsStr::from_bytes(entry)).file_name() != ours.file_name()
         })
         .collect::<Vec<_>>()
         .join(&b':');
      // SAFETY: constructors run before `main`, while the loader is the only
      // thread.
      unsafe {
         if kept.is_empty() {
            env::remove_var("LD_PRELOAD");
         } else {
            env::set_var("LD_PRELOAD", OsStr::from_bytes(&kept));
         }
      }
   }
}

#[cfg(target_os = "macos")]
mod macos {
   use std::{
      env,
      ffi::OsStr,
      os::unix::ffi::OsStrExt,
      path::Path,
   };

   /// macOS Phoenix's executable, `Discord.app/Contents/MacOS/Discord`. The
   /// Electron client's has the same name, and its registry walk then fails.
   const CLIENT: &str = "Discord";

   /// Whether this process is the client, after its re-exec. If so, takes
   /// this library out of `DYLD_INSERT_LIBRARIES` so the client's children
   /// (the CEF helpers, `osascript`) never load it. dyld kills an arm64e
   /// child that is handed this arm64 library when SIP is off.
   pub fn claim() -> bool {
      let client = env::current_exe().is_ok_and(|exe| {
         exe.file_name() == Some(OsStr::new(CLIENT))
            && exe
               .parent()
               .is_some_and(|dir| dir.ends_with("Contents/MacOS"))
      });
      // Discord re-execs itself unless its malloc settings are set, which
      // replaces this image before its own code runs; the new one needs the
      // library still inserted. Launch Services sets them, so nothing re-execs.
      let last_image = env::var_os(phoenix_sigs::MACOS_REEXEC).is_some()
         || env::var_os(phoenix_sigs::MACOS_MALLOC).is_some();
      if !client || !last_image {
         return false;
      }
      if let Some(path) = super::unix::library_path() {
         uninsert(Path::new(OsStr::from_bytes(path.to_bytes())));
      }
      true
   }

   fn uninsert(ours: &Path) {
      let Some(inserted) = env::var_os("DYLD_INSERT_LIBRARIES") else {
         return;
      };
      let kept = inserted
         .as_bytes()
         .split(|&byte| byte == b':')
         .filter(|entry| {
            !entry.is_empty() && Path::new(OsStr::from_bytes(entry)).file_name() != ours.file_name()
         })
         .collect::<Vec<_>>()
         .join(&b':');
      // SAFETY: constructors run before `main`, while dyld is the only
      // thread.
      unsafe {
         if kept.is_empty() {
            env::remove_var("DYLD_INSERT_LIBRARIES");
         } else {
            env::set_var("DYLD_INSERT_LIBRARIES", OsStr::from_bytes(&kept));
         }
      }
   }
}

#[cfg(target_os = "ios")]
mod ios {
   use std::{
      ffi::{
         CStr,
         c_void,
      },
      sync::atomic::{
         AtomicBool,
         Ordering,
      },
   };

   unsafe extern "C" {
      fn _dyld_register_func_for_add_image(added: extern "C" fn(*const c_void, isize));
   }

   static STARTED: AtomicBool = AtomicBool::new(false);

   /// The loader is a dependency of the app's executable, so its constructor
   /// can run before dyld maps `PhoenixFatClient`. dyld calls `added` for
   /// every image already mapped and for each one mapped later.
   pub fn start_with_phoenix() {
      // SAFETY: `added` lives as long as the process.
      unsafe { _dyld_register_func_for_add_image(added) };
   }

   extern "C" fn added(header: *const c_void, _slide: isize) {
      let mut info = libc::Dl_info {
         dli_fname: std::ptr::null(),
         dli_fbase: std::ptr::null_mut(),
         dli_sname: std::ptr::null(),
         dli_saddr: std::ptr::null_mut(),
      };
      // SAFETY: `header` is the Mach-O header of an image dyld just added.
      if unsafe { libc::dladdr(header, &raw mut info) } == 0 || info.dli_fname.is_null() {
         return;
      }
      // SAFETY: dli_fname is NUL-terminated and lives as long as the image.
      let path = unsafe { CStr::from_ptr(info.dli_fname) };
      if !path.to_bytes().ends_with(b"/PhoenixFatClient") || STARTED.swap(true, Ordering::AcqRel) {
         return;
      }
      // dyld holds its lock while it runs this, and starting walks its image
      // list.
      if let Err(err) = std::thread::Builder::new()
         .name("phoenix-start".to_owned())
         .spawn(super::run)
      {
         log::error!("phoenix could not start a thread: {err}");
      }
   }
}

/// To the unified log, where `log stream --predicate 'process == "Discord"'`
/// shows it, since a launched app's stderr goes nowhere.
#[cfg(target_vendor = "apple")]
fn logging() {
   struct Syslog;

   impl log::Log for Syslog {
      fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
         metadata.level() <= log::Level::Info
      }

      fn log(&self, record: &log::Record<'_>) {
         if !self.enabled(record.metadata()) {
            return;
         }
         let priority = match record.level() {
            log::Level::Error => libc::LOG_ERR,
            log::Level::Warn => libc::LOG_WARNING,
            _ => libc::LOG_NOTICE,
         };
         let line = format!("phoenix: {}", record.args()).replace('\0', " ");
         let Ok(line) = std::ffi::CString::new(line) else {
            return;
         };
         // SAFETY: a `%s` format with one NUL-terminated argument.
         unsafe { libc::syslog(priority, c"%s".as_ptr(), line.as_ptr()) };
      }

      fn flush(&self) {}
   }

   if log::set_logger(&Tee(Syslog)).is_ok() {
      log::set_max_level(log::LevelFilter::Info);
   }
}

#[cfg(target_os = "android")]
fn logging() {
   let config = android_logger::Config::default()
      .with_tag("phoenix")
      .with_max_level(log::LevelFilter::Info);
   let logger = Tee(android_logger::AndroidLogger::new(config));
   if log::set_logger(Box::leak(Box::new(logger))).is_ok() {
      log::set_max_level(log::LevelFilter::Info);
   }
}

#[cfg(not(any(target_os = "android", target_vendor = "apple")))]
fn logging() {
   use std::io::Write as _;

   struct Stderr;

   impl log::Log for Stderr {
      fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
         metadata.level() <= log::Level::Info
      }

      fn log(&self, record: &log::Record<'_>) {
         if self.enabled(record.metadata()) {
            // `eprintln!` panics when stderr is gone, a closed terminal or
            // a broken pipe, and a panic here takes Discord down.
            let _ = writeln!(
               std::io::stderr(),
               "[phoenix {}] {}",
               record.level(),
               record.args()
            );
         }
      }

      fn flush(&self) {}
   }

   if log::set_logger(&Tee(Stderr)).is_ok() {
      log::set_max_level(log::LevelFilter::Info);
   }
}

/// Resolves Android's app `files` directory before `Application` exists:
/// adopted SD card (`/mnt/expand/<uuid>/user/<uid>/<pkg>/files`, taken from
/// this library's own path because `/mnt/expand` is not listable), then
/// secondary-user `/data/user/<uid>/<pkg>/files`, falling back to
/// `/data/data/<pkg>/files`.
#[cfg(all(unix, not(any(target_os = "ios", target_os = "linux"))))]
fn android_files_dir(package: &str, user: u32, lib_path: Option<&str>) -> PathBuf {
   android_files_dir_with(package, user, lib_path, std::path::Path::is_dir)
}

#[cfg(any(test, all(unix, not(any(target_os = "ios", target_os = "linux")))))]
fn android_files_dir_with(
   package: &str,
   user: u32,
   lib_path: Option<&str>,
   is_dir: impl Fn(&std::path::Path) -> bool,
) -> PathBuf {
   if let Some(uuid) = lib_path
      .and_then(|path| path.strip_prefix("/mnt/expand/"))
      .and_then(|rest| rest.split('/').next())
      .filter(|uuid| !uuid.is_empty() && *uuid != "." && *uuid != "..")
   {
      let data = PathBuf::from(format!("/mnt/expand/{uuid}/user/{user}/{package}"));
      if is_dir(&data) {
         return data.join("files");
      }
   }
   if user != 0 {
      let data = PathBuf::from(format!("/data/user/{user}/{package}"));
      if is_dir(&data) {
         return data.join("files");
      }
   }
   PathBuf::from(format!("/data/data/{package}/files"))
}

#[cfg(test)]
mod tests {
   use std::path::{
      Path,
      PathBuf,
   };

   use super::android_files_dir_with;

   #[test]
   fn resolves_adopted_sd_card_and_work_profile_paths() {
      let sd_lib = Some("/mnt/expand/abc-123/app/~~hash/com.discord.phoenix-1/lib/arm64/libplm.so");
      let sd = android_files_dir_with("com.discord.phoenix", 0, sd_lib, |path| {
         path == Path::new("/mnt/expand/abc-123/user/0/com.discord.phoenix")
      });
      assert_eq!(
         sd,
         PathBuf::from("/mnt/expand/abc-123/user/0/com.discord.phoenix/files")
      );

      let work = android_files_dir_with("com.discord.phoenix", 10, None, |path| {
         path == Path::new("/data/user/10/com.discord.phoenix")
      });
      assert_eq!(
         work,
         PathBuf::from("/data/user/10/com.discord.phoenix/files")
      );

      let internal = android_files_dir_with("com.discord.phoenix", 0, None, |_| false);
      assert_eq!(
         internal,
         PathBuf::from("/data/data/com.discord.phoenix/files")
      );
   }
}
