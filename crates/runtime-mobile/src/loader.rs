//! Updates the loader in-app. The site's plugins are built against its loader,
//! so they wait in `plugins.next/` while the new loader,
//! `plugins/libphoenix-<abi>.so`, is swapped into the installed APK with
//! [`patcher::swap_loader`], signed again and handed to Android's installer
//! through Discord's own `FileProvider`. The new loader moves the staged
//! plugins in before it loads any. An install from separate splits cannot
//! update from one APK, so it opens the site instead.

use std::{
   fs::{
      self,
      File,
   },
   io::{
      BufWriter,
      Write,
   },
   path::Path,
   thread,
};

use memchr::memmem;
use phoenix_runtime::update::{
   self,
   Update,
};

/// Where the signed update waits for Android's installer, under the app's
/// `files` directory, which Discord's `FileProvider` shares.
const UPDATE_APK: &str = "phoenix-update.apk";

const LOADER_ASSET: &str = if cfg!(target_arch = "x86_64") {
   "plugins/libphoenix-x86_64.so"
} else if cfg!(target_arch = "arm") {
   "plugins/libphoenix-armv7.so"
} else {
   "plugins/libphoenix-arm64.so"
};

/// Stages `update`'s plugins for loader `version`, then installs it.
pub fn install(version: &str, update: &Update) {
   update::set_status(Some("Downloading plugins...".to_owned()));
   stage_plugins(version, update);
   run_loader_self_update();
}

/// Runs the in-app loader self-update directly against the site's current
/// `libphoenix-*.so` (also exposed to the gated localhost debug shell).
pub fn self_update_from_site() {
   if !update::begin() {
      log::info!("self-update: already running");
      return;
   }
   thread::spawn(run_loader_self_update);
}

/// Where plugins for a loader not installed yet wait, with that loader's
/// version in [`STAGED_FOR`].
const STAGED: &str = "plugins.next";
const STAGED_FOR: &str = "loader";

/// Downloads `update`'s plugins into [`STAGED`] for loader `version`. One that
/// fails stays as it is, and the next check offers it again.
fn stage_plugins(version: &str, update: &Update) {
   let Some(state) = phoenix_runtime::state() else {
      return;
   };
   let dir = state.home.join(STAGED);
   let _ = fs::remove_dir_all(&dir);
   if let Err(err) = fs::create_dir_all(&dir) {
      log::error!("staging plugins: {}: {err}", dir.display());
      return;
   }
   for name in &update.plugins {
      let staged =
         update::download_plugin(name, update.plugin_crcs.get(name).copied()).and_then(|source| {
            fs::write(dir.join(format!("{name}.js")), source).map_err(|err| err.to_string())
         });
      match staged {
         Ok(()) => log::info!("staged plugin {name} for {version}"),
         Err(err) => log::error!("staging {name} failed: {err}"),
      }
   }
   if let Err(err) = fs::write(dir.join(STAGED_FOR), version) {
      log::error!("staging plugins: {err}");
   }
}

/// Moves the plugins staged for this loader, or an older one, into `plugins`.
/// Plugins staged for a newer loader, whose install was not finished, wait.
pub fn adopt_staged(home: &Path, plugins: &Path) {
   let dir = home.join(STAGED);
   let Ok(version) = fs::read_to_string(dir.join(STAGED_FOR)) else {
      return;
   };
   if version.trim() > update::VERSION {
      return;
   }
   if let Err(err) = fs::create_dir_all(plugins) {
      log::error!("staged plugins: {}: {err}", plugins.display());
      return;
   }
   for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
      let path = entry.path();
      if path.extension().is_some_and(|extension| extension == "js")
         && let Some(name) = path.file_name()
         && let Err(err) = fs::rename(&path, plugins.join(name))
      {
         log::error!("staged plugin {}: {err}", path.display());
      }
   }
   let _ = fs::remove_dir_all(&dir);
   log::info!("moved in the plugins staged for {}", version.trim());
}

fn run_loader_self_update() {
   update::set_status(Some("Downloading loader...".to_owned()));
   let _ = crate::picker::toast("Downloading Ruby update...");

   let url = format!("{}{LOADER_ASSET}", update::ASSETS);
   let bytes = match phoenix_runtime::fetch::get(&url) {
      Ok(bytes) => bytes,
      Err(err) => {
         fail_self_update(&format!("Download failed: {err}"));
         return;
      },
   };
   if !bytes.starts_with(b"\x7fELF") || memmem::find(&bytes, b"phoenix-version:").is_none() {
      fail_self_update("Downloaded loader is invalid");
      return;
   }

   let loader = match patcher::Library::new(patcher::LOADER, bytes) {
      Ok(loader) => loader,
      Err(err) => {
         fail_self_update(&format!("Downloaded loader is invalid: {err}"));
         return;
      },
   };
   let base = match crate::host::base_apk() {
      Ok(base) => base,
      Err(err) => {
         fail_self_update(&format!("Self-update failed: {err}"));
         return;
      },
   };
   if installed_from_splits(&base) {
      fail_self_update("Split APK install, update from the site");
      let _ = crate::picker::open(update::SITE);
      return;
   }

   update::set_status(Some("Signing the update...".to_owned()));
   let Some(state) = phoenix_runtime::state() else {
      fail_self_update("Self-update failed: phoenix has not started");
      return;
   };
   let files = crate::install::files_dir().unwrap_or_else(|| state.files_dir());
   if let Err(err) = fs::create_dir_all(&files) {
      fail_self_update(&format!("Self-update failed: {}: {err}", files.display()));
      return;
   }
   let path = files.join(UPDATE_APK);
   if let Err(err) = write_update(&base, &loader, &path) {
      fail_self_update(&format!("Self-update failed: {err}"));
      return;
   }
   update::set_status(Some("Opening Android's installer...".to_owned()));
   match crate::install::open(&path) {
      Ok(()) => update::finish(None),
      Err(err) => fail_self_update(&format!("Self-update failed: {err}")),
   }
}

/// Whether the app runs from split APKs, which sit next to `base`.
fn installed_from_splits(base: &str) -> bool {
   Path::new(base)
      .parent()
      .and_then(|dir| fs::read_dir(dir).ok())
      .is_some_and(|entries| {
         entries.flatten().any(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
               name.starts_with("split_")
                  && Path::new(name)
                     .extension()
                     .is_some_and(|extension| extension.eq_ignore_ascii_case("apk"))
            })
         })
      })
}

/// Writes `base` with `loader` swapped in and signed to `path`.
fn write_update(base: &str, loader: &patcher::Library, path: &Path) -> Result<(), String> {
   let map = crate::host::Map::open(base).map_err(|err| format!("{base}: {err}"))?;
   let apk = patcher::swap_loader(map.bytes(), loader).map_err(|err| err.to_string())?;
   let mut file =
      BufWriter::new(File::create(path).map_err(|err| format!("{}: {err}", path.display()))?);
   for part in apk.parts() {
      file.write_all(part).map_err(|err| err.to_string())?;
   }
   file.flush().map_err(|err| err.to_string())
}

/// Deletes an update Android's installer has had its chance at, since it is as
/// big as the app.
pub fn forget_update(files: &Path) {
   let _ = fs::remove_file(files.join(UPDATE_APK));
   if let Some(real) = crate::install::files_dir()
      && real != files
   {
      let _ = fs::remove_file(real.join(UPDATE_APK));
   }
}

fn fail_self_update(message: &str) {
   log::error!("self-update: {message}");
   update::finish(Some(message.to_owned()));
   let _ = crate::picker::toast(message);
}
