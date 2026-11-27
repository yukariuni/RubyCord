//! RubyCord themes. Discord Phoenix draws with GPUI, not a DOM, so a theme is not CSS: it is a JSON
//! file in `<home>/themes` that names what to change.
//!
//! ```json
//! { "square": true, "hide": ["composer.gift", "dm.nitro"] }
//! ```
//!
//! `square` squares the corners, and `hide` lists [`HIDES`] ids to remove from the client. Enabled
//! themes add up. What each id does is the platform's: it registers an applier with
//! [`set_applier`], which gets the [`Effects`] whenever they change.

use std::{
   collections::BTreeSet,
   fs,
   path::{Path, PathBuf},
   sync::{Mutex, Once, OnceLock, PoisonError},
   thread,
   time::{Duration, SystemTime},
};

/// The ids a theme can hide, with what each one is.
pub const HIDES: &[(&str, &str)] = &[
   ("dm.nitro", "Nitro in the DM list"),
   ("dm.quests", "Quests in the DM list"),
   ("composer.stickers", "the sticker button in the message box"),
   ("composer.apps", "the apps button in the message box"),
   ("composer.gift", "the gift button in the message box"),
];

#[derive(serde::Deserialize)]
struct File {
   #[serde(default)]
   square: bool,
   #[serde(default)]
   hide: Vec<String>,
}

struct Theme {
   name: String,
   /// The file's content, or why it is not a theme.
   file: Result<File, String>,
   enabled: bool,
}

/// What the Themes page lists.
pub struct Listed {
   pub name: String,
   pub enabled: bool,
   pub square: bool,
   /// The ids it hides, unknown ones included.
   pub hide: Vec<String>,
   /// Set when the file is not a valid theme.
   pub error: Option<String>,
}

/// What the enabled themes ask for together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Effects {
   pub square: bool,
   pub hide: BTreeSet<String>,
}

type Applier = fn(&Effects);

static APPLIER: OnceLock<Applier> = OnceLock::new();
static THEMES: Mutex<Option<Vec<Theme>>> = Mutex::new(None);

/// Where theme files live, once phoenix has started.
pub fn dir() -> Option<PathBuf> {
   crate::state().map(|state| state.home.join("themes"))
}

fn enabled_path() -> Option<PathBuf> {
   crate::state().map(|state| state.home.join("themes.json"))
}

fn is_theme(path: &Path) -> bool {
   path
      .extension()
      .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

fn parse(bytes: &[u8]) -> Result<File, String> {
   serde_json::from_slice(bytes).map_err(|err| err.to_string())
}

fn load() -> Vec<Theme> {
   let Some(dir) = dir() else {
      return Vec::new();
   };
   let on: Vec<String> = enabled_path()
      .and_then(|path| fs::read(path).ok())
      .and_then(|bytes| serde_json::from_slice(&bytes).ok())
      .unwrap_or_default();
   let mut themes: Vec<Theme> = fs::read_dir(&dir)
      .into_iter()
      .flatten()
      .filter_map(Result::ok)
      .map(|entry| entry.path())
      .filter(|path| is_theme(path))
      .filter_map(|path| {
         let name = path.file_stem()?.to_str()?.to_owned();
         let file = fs::read(&path)
            .map_err(|err| err.to_string())
            .and_then(|bytes| parse(&bytes));
         Some(Theme {
            enabled: on.contains(&name),
            name,
            file,
         })
      })
      .collect();
   themes.sort_by(|a, b| a.name.cmp(&b.name));
   themes
}

/// Every theme file in `dir` with its size and modification time, so a file added, removed or
/// edited changes it.
fn fingerprint(dir: &Path) -> Vec<(String, u64, Option<SystemTime>)> {
   let mut files: Vec<_> = fs::read_dir(dir)
      .into_iter()
      .flatten()
      .filter_map(Result::ok)
      .filter(|entry| is_theme(&entry.path()))
      .filter_map(|entry| {
         let meta = entry.metadata().ok()?;
         Some((
            entry.file_name().to_string_lossy().into_owned(),
            meta.len(),
            meta.modified().ok(),
         ))
      })
      .collect();
   files.sort_by(|a, b| a.0.cmp(&b.0));
   files
}

/// Watches the folder, so files added, deleted or edited show up in the open page and take
/// effect without a restart.
fn watch() {
   static WATCHER: Once = Once::new();
   WATCHER.call_once(|| {
      let Some(dir) = dir() else {
         return;
      };
      let spawned = thread::Builder::new()
         .name("rubycord-themes".to_owned())
         .spawn(move || {
            let mut seen = fingerprint(&dir);
            loop {
               thread::sleep(Duration::from_secs(1));
               let now = fingerprint(&dir);
               if now != seen {
                  seen = now;
                  reload();
               }
            }
         });
      if let Err(err) = spawned {
         log::warn!("themes: no folder watcher: {err}");
      }
   });
}

fn with<T>(f: impl FnOnce(&mut Vec<Theme>) -> T) -> T {
   watch();
   let mut themes = THEMES.lock().unwrap_or_else(PoisonError::into_inner);
   f(themes.get_or_insert_with(load))
}

/// Registers what turns [`Effects`] into changes in the client.
pub fn set_applier(applier: Applier) {
   let _ = APPLIER.set(applier);
}

/// Reads the enabled themes and applies them. Call once phoenix has started, so what was on at
/// the last run is on again before the first page opens.
pub fn start() {
   with(|_| ());
   apply();
}

fn apply() {
   if let Some(applier) = APPLIER.get() {
      applier(&effects());
   }
}

/// What the enabled themes ask for together.
pub fn effects() -> Effects {
   with(|themes| {
      let mut effects = Effects::default();
      for file in themes
         .iter()
         .filter(|theme| theme.enabled)
         .filter_map(|theme| theme.file.as_ref().ok())
      {
         effects.square |= file.square;
         effects.hide.extend(file.hide.iter().cloned());
      }
      effects
   })
}

pub fn list() -> Vec<Listed> {
   with(|themes| {
      themes
         .iter()
         .map(|theme| {
            let file = theme.file.as_ref();
            Listed {
               name: theme.name.clone(),
               enabled: theme.enabled,
               square: file.is_ok_and(|file| file.square),
               hide: file.map(|file| file.hide.clone()).unwrap_or_default(),
               error: file.err().cloned(),
            }
         })
         .collect()
   })
}

/// Rereads the folder and applies it. The watcher calls it on every change, and the Refresh
/// button does too.
pub fn reload() {
   *THEMES.lock().unwrap_or_else(PoisonError::into_inner) = None;
   apply();
   crate::refresh_settings();
}

pub fn set_enabled(name: &str, enabled: bool) {
   let on = with(|themes| {
      for theme in themes.iter_mut().filter(|theme| theme.name == name) {
         theme.enabled = enabled;
      }
      themes
         .iter()
         .filter(|theme| theme.enabled)
         .map(|theme| theme.name.clone())
         .collect::<Vec<_>>()
   });
   if let Some(path) = enabled_path()
      && let Err(err) = serde_json::to_vec(&on)
         .map_err(std::io::Error::other)
         .and_then(|bytes| fs::write(path, bytes))
   {
      log::warn!("themes: saving the enabled list: {err}");
   }
   apply();
   crate::refresh_settings();
}

/// Saves a picked `.json` theme into the themes folder and turns it on. Returns its name.
pub fn import(file_name: &str, bytes: &[u8]) -> Result<String, String> {
   const MAX: usize = 1 << 20;
   let name = file_name
      .strip_suffix(".json")
      .or_else(|| file_name.strip_suffix(".JSON"))
      .filter(|name| {
         !name.is_empty()
            && name
               .chars()
               .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '-' | '.'))
            && !name.starts_with('.')
      })
      .ok_or("pick a .json theme with a plain name")?;
   if bytes.len() > MAX {
      return Err("the file is over 1 MB".to_owned());
   }
   parse(bytes).map_err(|err| format!("not a theme: {err}"))?;
   let dir = dir().ok_or("RubyCord has not started")?;
   fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
   fs::write(dir.join(format!("{name}.json")), bytes).map_err(|err| err.to_string())?;
   reload();
   set_enabled(name, true);
   Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn reads_a_theme() {
      let file = parse(br#"{ "square": true, "hide": ["composer.gift", "dm.nitro"], "extra": 1 }"#)
         .unwrap();
      assert!(file.square);
      assert_eq!(file.hide, ["composer.gift", "dm.nitro"]);
      let empty = parse(b"{}").unwrap();
      assert!(!empty.square && empty.hide.is_empty());
      assert!(parse(b"not json").is_err());
      assert!(parse(br#"{ "hide": "composer.gift" }"#).is_err());
   }

   #[test]
   fn imports_only_plain_json_names() {
      // Rejected before anything is read from or written to disk.
      assert!(import("a.css", b"{}").is_err());
      assert!(import("../a.json", b"{}").is_err());
      assert!(import(".json", b"{}").is_err());
      assert!(import("a.json", b"nope").is_err());
   }

   #[test]
   fn fingerprint_follows_the_folder() {
      let dir = std::env::temp_dir().join(format!("rubycord-themes-test-{}", std::process::id()));
      fs::create_dir_all(&dir).unwrap();
      let empty = fingerprint(&dir);
      fs::write(dir.join("a.json"), "{}").unwrap();
      fs::write(dir.join("notes.txt"), "ignored").unwrap();
      let one = fingerprint(&dir);
      assert_eq!(one.len(), 1);
      assert_ne!(empty, one);
      fs::write(dir.join("a.json"), r#"{"square":true}"#).unwrap();
      assert_ne!(one, fingerprint(&dir), "an edit changes it");
      fs::remove_file(dir.join("a.json")).unwrap();
      assert_eq!(empty, fingerprint(&dir));
      fs::remove_dir_all(&dir).unwrap();
   }
}
