//! Tells the user when the site ships a newer RubyCord: a newer loader,
//! or plugin bundles that differ from the installed ones. The site publishes
//! both in `version.json` (`web/tools/bundle-plugins.ts`).
//!
//! Plugin updates download each changed `plugins/<name>.js`, verify its CRC-32
//! against `version.json` and hot-reload it. A loader update is the platform's
//! ([`crate::Platform::update_loader`]).

use std::{
   collections::BTreeMap,
   sync::{
      Mutex,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
   thread,
};

use crate::Notice;
/// This build's version, `YEAR.MAJOR.MINOR` from the workspace `Cargo.toml` (`build.rs`).
pub const VERSION: &str = env!("PHOENIX_VERSION");
/// RubyCord hosts no update server. While `ASSETS` is empty, checks and the plugin list report
/// that and stop; set both to a server serving `version.json` and `plugins/` to turn them on.
pub const SITE: &str = "";
pub const ASSETS: &str = "";

fn site_get(path: &str) -> Result<Vec<u8>, String> {
   if ASSETS.is_empty() {
      return Err("no update server configured".to_owned());
   }
   crate::fetch::get(&format!("{ASSETS}{path}")).map_err(|err| err.to_string())
}

/// Where `bundle-plugins.ts` reads the version from the loader the site ships.
#[used]
static MARKER: &str = concat!("phoenix-version:", env!("PHOENIX_VERSION"), ";");

#[derive(serde::Deserialize)]
struct Site {
   loader:  String,
   plugins: BTreeMap<String, u32>,
}

/// What the site has that this install lacks.
#[derive(Clone)]
pub struct Update {
   /// The site's version, when it is newer than [`VERSION`].
   pub loader:      Option<String>,
   /// Installed plugins whose code differs from the site's.
   pub plugins:     Vec<String>,
   /// Expected CRC-32 for each plugin in [`Self::plugins`].
   pub plugin_crcs: BTreeMap<String, u32>,
}

/// A plugin the site offers, as `plugins/index.json` lists it.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct Listed {
   pub name:        String,
   pub description: String,
   #[serde(default)]
   pub warning:     Option<String>,
}

/// The site's plugin list, fetched when the install page first opens.
#[derive(Clone)]
pub enum Catalog {
   Unfetched,
   Loading,
   Loaded(Vec<Listed>),
   Failed(String),
}

static CATALOG: Mutex<Catalog> = Mutex::new(Catalog::Unfetched);
static FOUND: Mutex<Option<Update>> = Mutex::new(None);
static STATUS: Mutex<Option<String>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

/// The update the last check found, if any.
pub fn found() -> Option<Update> {
   FOUND.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Human-readable status of the current or last update attempt, if any.
pub fn status() -> Option<String> {
   STATUS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
}

/// Whether an update download/install is currently running.
pub fn busy() -> bool {
   BUSY.load(Ordering::Relaxed)
}

pub fn set_status(message: Option<String>) {
   *STATUS.lock().unwrap_or_else(PoisonError::into_inner) = message;
   crate::refresh_settings();
}

pub fn catalog() -> Catalog {
   CATALOG
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
}

/// Fetches the site's plugin list in the background, unless it is loaded or
/// on its way. A failed fetch is retried on the next call.
pub fn fetch_catalog() {
   {
      let mut catalog = CATALOG.lock().unwrap_or_else(PoisonError::into_inner);
      if matches!(*catalog, Catalog::Loading | Catalog::Loaded(_)) {
         return;
      }
      *catalog = Catalog::Loading;
   }
   thread::spawn(|| {
      let fetched = site_get("plugins/index.json").and_then(|body| {
            serde_json::from_slice::<Vec<Listed>>(&body).map_err(|err| err.to_string())
         });
      *CATALOG.lock().unwrap_or_else(PoisonError::into_inner) = match fetched {
         Ok(listed) => Catalog::Loaded(listed),
         Err(err) => {
            log::info!("plugin list: {err}");
            Catalog::Failed(err)
         },
      };
      crate::refresh_settings();
   });
}

/// Installs a plugin the site lists. Site plugins can call API an older loader
/// lacks, so platforms with an in-app loader updater wait until the loader is
/// current.
pub fn install_listed(name: &str) -> Result<crate::installer::Installed, crate::Error> {
   let updates_loader = crate::platform().is_some_and(<dyn crate::Platform>::updates_loader);
   if updates_loader && let Some(version) = found().and_then(|update| update.loader) {
      return Err(crate::Error::LoaderOutdated { version });
   }
   crate::install(&format!("{ASSETS}plugins/{name}.js"))
}

/// Asks the site what this install lacks, and remembers it for [`found`].
/// `None` means up to date.
fn query() -> Result<Option<Update>, String> {
   let body = site_get("version.json")?;
   let site =
      serde_json::from_slice::<Site>(&body).map_err(|err| format!("version.json: {err}"))?;
   let loader = (site.loader.as_str() > VERSION).then_some(site.loader);
   // Plugins come along with a loader update, so one tap brings both.
   let mut plugins = Vec::new();
   let mut plugin_crcs = BTreeMap::new();
   for plugin in crate::plugins() {
      if let Some(&crc) = site.plugins.get(&plugin.name)
         && crate::source(&plugin.name)
            .is_some_and(|source| crc32fast::hash(source.as_bytes()) != crc)
      {
         plugin_crcs.insert(plugin.name.clone(), crc);
         plugins.push(plugin.name);
      }
   }
   let update = (loader.is_some() || !plugins.is_empty()).then_some(Update {
      loader,
      plugins,
      plugin_crcs,
   });
   FOUND
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone_from(&update);
   crate::refresh_settings();
   Ok(update)
}

/// Asks the site in the background. Failures (offline) are only logged.
pub fn check() {
   thread::spawn(|| {
      let update = match query() {
         Ok(Some(update)) => update,
         Ok(None) => {
            log::info!("up to date ({VERSION})");
            return;
         },
         Err(err) => {
            log::info!("update check: {err}");
            return;
         },
      };
      log::info!(
         "update available: loader {:?}, plugins {:?}",
         update.loader,
         update.plugins
      );
      if let Some(platform) = crate::platform() {
         platform.notify(Notice::UpdateAvailable(&update));
      }
   });
}

/// Checks the site on demand for a user click, updating status and showing a
/// toast on completion.
pub fn check_now() {
   if !begin() {
      log::info!("update check: already running");
      crate::toast("Update check already in progress...");
      return;
   }
   set_status(Some("Checking for updates...".to_owned()));
   thread::spawn(|| {
      match query() {
         Ok(Some(update)) => {
            log::info!(
               "update available: loader {:?}, plugins {:?}",
               update.loader,
               update.plugins
            );
            finish(None);
            if let Some(platform) = crate::platform() {
               platform.notify(Notice::UpdateAvailable(&update));
            }
         },
         Ok(None) => {
            log::info!("up to date ({VERSION})");
            finish(Some(format!("RubyCord {VERSION} is up to date.")));
            crate::toast("RubyCord is up to date");
         },
         Err(err) => {
            log::info!("update check: {err}");
            let msg = format!("Update check failed: {err}");
            finish(Some(msg.clone()));
            crate::toast(&msg);
         },
      }
   });
}

/// Checks the site again and applies what this install lacks.
///
/// Updates changed plugins over-the-air first (hot-reloading them live), then
/// runs the in-app APK self-updater if a newer loader is available.
pub fn apply() {
   if BUSY.swap(true, Ordering::AcqRel) {
      log::info!("update apply: already running");
      crate::toast("Update already in progress...");
      return;
   }
   thread::spawn(|| {
      // The last check ran when Discord started, and the site may have shipped
      // since, so what it found can be out of date by the time this runs.
      let update = query().unwrap_or_else(|err| {
         log::info!("update apply: {err}, using the last check");
         found()
      });
      let Some(update) = update else {
         log::info!("update apply: up to date ({VERSION})");
         // `finish` also clears the last status line and redraws the page;
         // releasing only the slot left "Updating..." on screen for good.
         finish(None);
         crate::toast("RubyCord is up to date");
         return;
      };
      // The site's plugins are built against its loader and can call API this
      // one lacks, so with a loader update they wait in `plugins.next/` for it.
      if let Some(version) = &update.loader
         && let Some(platform) = crate::platform()
      {
         if platform.updates_loader() {
            platform.update_loader(version, &update);
            return;
         }
         if update.plugins.is_empty() {
            let msg = format!("RubyCord {version} is available — restart RubyCord to update.");
            finish(Some(msg.clone()));
            crate::toast(&msg);
            return;
         }
      }
      let mut updated_plugins = Vec::new();
      let mut failed = Vec::new();
      if !update.plugins.is_empty() {
         set_status(Some(format!(
            "Updating {} plugin{}...",
            update.plugins.len(),
            if update.plugins.len() == 1 { "" } else { "s" }
         )));
         for name in &update.plugins {
            let expected_crc = update.plugin_crcs.get(name).copied();
            match update_plugin(name, expected_crc) {
               Ok(()) => {
                  log::info!("ota updated plugin {name}");
                  updated_plugins.push(name.clone());
               },
               Err(err) => {
                  log::error!("ota update {name} failed: {err}");
                  failed.push(format!("{name}: {err}"));
               },
            }
         }
         // Drop successfully updated plugins from FOUND.
         {
            let mut guard = FOUND.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(pending) = guard.as_mut() {
               pending
                  .plugins
                  .retain(|name| !updated_plugins.contains(name));
               pending
                  .plugin_crcs
                  .retain(|name, _| !updated_plugins.contains(name));
               if pending.loader.is_none() && pending.plugins.is_empty() {
                  *guard = None;
               }
            }
         }
      }

      // A failed plugin used to go unreported whenever the loader update below
      // still had to run, since only `update.loader.is_none()` printed it.
      // The slot is released either way, and the failure keeps its line.
      BUSY.store(false, Ordering::Release);
      if failed.is_empty() {
         set_status(None);
      } else {
         let msg = format!("Plugin update failed: {}", failed.join("; "));
         log::error!("{msg}");
         set_status(Some(msg.clone()));
         crate::toast(&msg);
      }
      if !updated_plugins.is_empty()
         && let Some(platform) = crate::platform()
      {
         platform.notify(Notice::PluginsUpdated(&updated_plugins));
      }
   });
}

fn update_plugin(name: &str, expected_crc: Option<u32>) -> Result<(), String> {
   let source = download_plugin(name, expected_crc)?;
   crate::save_source(name, source).map_err(|err| err.to_string())
}

/// The site's `plugins/<name>.js`, checked against its CRC-32.
pub fn download_plugin(name: &str, expected_crc: Option<u32>) -> Result<String, String> {
   let bytes = site_get(&format!("plugins/{name}.js")).map_err(|err| format!("fetch: {err}"))?;
   if let Some(expected) = expected_crc {
      let actual = crc32fast::hash(&bytes);
      if actual != expected {
         return Err(format!(
            "CRC mismatch (got {actual:#x}, expected {expected:#x})"
         ));
      }
   }
   String::from_utf8(bytes).map_err(|err| format!("invalid UTF-8: {err}"))
}

/// Claims the update slot. False when an update already runs.
pub fn begin() -> bool {
   !BUSY.swap(true, Ordering::AcqRel)
}

/// Releases the update slot with `status` shown.
pub fn finish(status: Option<String>) {
   BUSY.store(false, Ordering::Release);
   set_status(status);
}
