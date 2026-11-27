//! The plugin runtime the loader starts.
//!
//! [`start`] picks the target library among the loaded images, boots `QuickJS`
//! with the `phoenix` API, and evaluates every bundle in `<home>/plugins`.
//! Every gateway registry entry points at phoenix's detour from the start, and
//! a plugin's hook turns its event on.

use std::{
   path::{
      Path,
      PathBuf,
   },
   sync::{
      Mutex,
      OnceLock,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
};

use phoenix_hook::image::{
   BuildId,
   Image,
};
use phoenix_sigs::{
   LIBRARIES,
   registry::{
      self,
      GatewayRegistry,
   },
};

pub mod console;
pub mod discord;
pub mod fetch;
pub mod gateway;
pub mod highlight;
pub mod installer;
pub mod js;
pub mod local;
mod platform;
pub mod remote;
pub mod store;
pub mod update;

pub use platform::{
   Notice,
   Platform,
};

#[derive(Debug, flub::Error)]
pub enum Error {
   #[error("no loaded library is one of {LIBRARIES:?}")]
   NoTarget,
   #[error("the runtime is already running")]
   AlreadyStarted,
   #[error("the runtime has not started")]
   NotStarted,
   #[error("{path}: {source}")]
   Io {
      path:   PathBuf,
      #[source]
      source: std::io::Error,
   },
   #[error("javascript: {0}")]
   Js(String),
   #[error(transparent)]
   Registry(#[from] registry::Error),
   #[error("{event}: the registry slot held {found:#x}, the walk read {expected:#x}")]
   SwapConflict {
      event:    String,
      expected: usize,
      found:    usize,
   },
   #[error(transparent)]
   Hook(#[from] phoenix_hook::Error),
   #[error("{url}: plugins install from https:// links to a .js file, a GitHub file or a gist")]
   PluginUrl { url: String },
   #[error("{url}: this GitHub page is not a file, link the plugin's .js file")]
   PluginGitHubPage { url: String },
   #[error("{url}: the gist has {found} .js files, link one with its #file- anchor")]
   PluginGist { url: String, found: usize },
   #[error("{name}: plugin files are .js files")]
   PluginFile { name: String },
   #[error("{name}: the download is a web page, not a plugin")]
   PluginPage { name: String },
   #[error("{name}: the download is JSON data, not a plugin")]
   PluginData { name: String },
   #[error("{name}: this is an ES module, bundle it into a plain script (esbuild --format=iife)")]
   PluginModule { name: String },
   #[error("{name}: this is TypeScript source, install its bundled .js")]
   PluginTypeScript { name: String },
   #[error("{name}: {message}")]
   PluginSyntax { name: String, message: String },
   #[error("{url}: the download is not UTF-8 text")]
   NotUtf8 { url: String },
   #[error(transparent)]
   Fetch(#[from] fetch::FetchError),
   #[error("update RubyCord to {version} first, its plugins need it")]
   LoaderOutdated { version: String },
}

pub struct State {
   pub image: Image<'static>,
   pub home:  PathBuf,
   js:        Mutex<js::Engine>,
}

static STATE: OnceLock<State> = OnceLock::new();
static PLATFORM: OnceLock<&'static dyn Platform> = OnceLock::new();
/// Phoenix's Staff Settings page reads `(user.flags | flag_override) & 1`, and
/// the platforms set that bit while this is on.
static STAFF: AtomicBool = AtomicBool::new(false);

/// The running runtime, once [`start`] published it.
#[must_use]
pub fn state() -> Option<&'static State> {
   STATE.get()
}

/// The platform [`start`] runs on.
#[must_use]
pub fn platform() -> Option<&'static dyn Platform> {
   PLATFORM.get().copied()
}

/// Sets the platform ahead of [`start`], for hooks that run before it.
pub fn set_platform(platform: &'static dyn Platform) {
   let _ = PLATFORM.set(platform);
}

/// A plugin bundle, an esbuild IIFE that receives the `phoenix` API.
pub struct Plugin {
   pub name:   String,
   pub source: String,
}

/// A plugin as the settings page shows it.
#[derive(Clone, Debug)]
pub struct PluginInfo {
   pub name:      String,
   pub enabled:   bool,
   pub error:     Option<String>,
   pub installed: bool,
   pub header:    Header,
   pub settings:  Vec<Setting>,
}

/// The `// @key value` lines a bundle starts with.
#[derive(Clone, Debug, Default)]
pub struct Header {
   pub description: Option<String>,
   pub version:     Option<String>,
   pub author:      Option<String>,
   /// `// @enabled false`, for plugins that stay off until the user turns
   /// them on.
   pub default_off: bool,
}

impl From<&str> for Header {
   fn from(source: &str) -> Self {
      let mut header = Self::default();
      for line in source
         .lines()
         .map(str::trim)
         .filter(|line| !line.is_empty())
      {
         let Some(comment) = line.strip_prefix("//") else {
            break;
         };
         let Some((key, value)) = comment
            .trim()
            .strip_prefix('@')
            .and_then(|tag| tag.split_once(' '))
         else {
            continue;
         };
         let value = value.trim();
         match key {
            "description" => header.description = Some(value.to_owned()),
            "version" => header.version = Some(value.to_owned()),
            "author" => header.author = Some(value.to_owned()),
            "enabled" => header.default_off = value == "false",
            _ => {},
         }
      }
      header
   }
}

/// A plugin setting as the user currently has it.
#[derive(Clone, Debug)]
pub struct Setting {
   pub key:         String,
   pub title:       String,
   pub description: Option<String>,
   pub control:     Control,
   /// A text setting edited over several lines.
   pub multiline:   bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Control {
   Switch(bool),
   Text(String),
   /// Text shown masked, and redacted from the plugin's log lines.
   Secret(String),
   Select {
      options: Vec<Choice>,
      chosen:  String,
   },
   Number(f64),
   Button,
}

/// One option of a select setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
   pub value: String,
   pub label: String,
}

/// A value the user picked for a setting, as `plugins.json` stores it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Chosen {
   Switch(bool),
   Number(f64),
   Text(String),
}

pub fn engine() -> Option<std::sync::MutexGuard<'static, js::Engine>> {
   Some(
      STATE
         .get()?
         .js
         .lock()
         .unwrap_or_else(std::sync::PoisonError::into_inner),
   )
}

/// Runs matching `phoenix.discord.onRequest` hooks on an outgoing Discord API
/// request.
#[must_use]
pub fn intercept_request(
   matched: &[u64],
   method: &str,
   path: &str,
   body: &[u8],
) -> discord::RequestAction {
   engine().map_or(discord::RequestAction::Pass, |engine| {
      engine.intercept_request(matched, method, path, body)
   })
}

/// Runs matching `phoenix.discord.onResponse` hooks (sync or async) on an
/// incoming Discord API reply and delivers the final status and body through
/// `done`.
pub fn intercept_response(
   matched: Vec<u64>,
   method: String,
   path: String,
   status: i32,
   body: Vec<u8>,
   done: std::sync::mpsc::Sender<(i32, Vec<u8>)>,
) {
   if let Some(engine) = engine() {
      engine.intercept_response(matched, method, path, status, body, done);
   } else {
      let _ = done.send((status, body));
   }
}

/// Redraws the platform's settings pages.
pub fn refresh_settings() {
   if let Some(platform) = platform() {
      platform.refresh();
   }
}

pub fn toast(message: &str) {
   if let Some(platform) = platform() {
      platform.toast(message);
   }
}

/// Every plugin found at start, enabled or not.
#[must_use]
pub fn plugins() -> Vec<PluginInfo> {
   engine().map_or_default(|engine| engine.plugins())
}

/// Whether the staff unlock is on.
#[must_use]
pub fn staff_enabled() -> bool {
   STAFF.load(Ordering::Acquire)
}

/// Turns the staff unlock on or off and remembers the choice across restarts.
pub fn set_staff_enabled(enabled: bool) {
   STAFF.store(enabled, Ordering::Release);
   if let Some(engine) = engine() {
      engine.save_choices();
   }
}

/// Whether the localhost debug shell (`127.0.0.1:4242`) is enabled.
#[must_use]
pub fn remote_shell_enabled() -> bool {
   remote::enabled()
}

pub fn set_remote_shell_flag(enabled: bool) {
   remote::set_flag(enabled);
}

/// Turns the localhost debug shell on or off and remembers the choice.
pub fn set_remote_shell_enabled(enabled: bool) {
   remote::set_enabled(enabled);
   if let Some(engine) = engine() {
      engine.save_choices();
   }
}

/// Runs `code` in the plugin runtime's global scope, logs it, and returns its
/// formatted result or error.
pub fn evaluate_capture(code: &str) -> Result<String, String> {
   log::info!("> {code}");
   let result = engine().map(|engine| engine.evaluate(code));
   match result {
      Some(Ok(result)) => {
         log::info!("{result}");
         Ok(result)
      },
      Some(Err(err)) => {
         let message = err.to_string();
         log::error!("{message}");
         Err(message)
      },
      None => {
         let message = "the runtime is not running".to_owned();
         log::error!("{message}");
         Err(message)
      },
   }
}

/// Turns a plugin on or off and remembers the choice across restarts.
pub fn set_enabled(name: &str, enabled: bool) {
   if let Some(mut engine) = engine() {
      engine.set_enabled(name, enabled);
   }
}

/// Sets one of a plugin's settings and remembers it across restarts.
pub fn set_setting(plugin: &str, key: &str, value: Chosen) {
   if let Some(engine) = engine() {
      engine.set_setting(plugin, key, value);
   }
}

/// Runs a button setting's `onPress`.
pub fn press(plugin: &str, key: &str) {
   if let Some(engine) = engine() {
      engine.press(plugin, key);
   }
}

/// Runs `code` in the plugin runtime's global scope and logs it with its
/// result.
pub fn evaluate(code: &str) {
   log::info!("> {code}");
   let result = engine().map(|engine| engine.evaluate(code));
   match result {
      Some(Ok(result)) => log::info!("{result}"),
      Some(Err(err)) => log::error!("{err}"),
      None => log::error!("the runtime is not running"),
   }
}

/// Downloads a plugin into `home/plugins` and loads it, replacing a plugin of
/// the same name. Takes direct links, GitHub file pages and gists.
pub fn install(url: &str) -> Result<installer::Installed, Error> {
   let (download, name) = match installer::parse(url)? {
      installer::Link::File { url, name } => (url, name),
      installer::Link::Gist { id, anchor } => installer::gist(url, &id, anchor.as_deref())?,
   };
   let source = String::from_utf8(fetch::get(&download)?).map_err(|_| {
      Error::NotUtf8 {
         url: download.clone(),
      }
   })?;
   add(name, source)
}

/// Installs a bundle the user picked from the device, named by its file name.
pub fn install_file(file: &str, bytes: Vec<u8>) -> Result<installer::Installed, Error> {
   let name = installer::name_of(file, &[file], None).map_err(|err| {
      match err {
         Error::PluginUrl { .. } => {
            Error::PluginFile {
               name: file.to_owned(),
            }
         },
         other => other,
      }
   })?;
   let source = String::from_utf8(bytes).map_err(|_| {
      Error::NotUtf8 {
         url: file.to_owned(),
      }
   })?;
   add(name, source)
}

fn add(name: String, source: String) -> Result<installer::Installed, Error> {
   installer::check(&name, &source)?;
   let previous = engine().and_then(|engine| {
      engine
         .plugins()
         .into_iter()
         .find(|plugin| plugin.name == name)
   });
   let replaced = installer::Replaced::of(previous.as_ref(), &source);
   save_source(&name, source)?;
   Ok(installer::Installed { name, replaced })
}

/// Writes `source` to `home/plugins/<name>.js` and loads it, replacing the
/// plugin's code, bundled or not.
pub fn save_source(name: &str, source: String) -> Result<(), Error> {
   let state = STATE.get().ok_or(Error::NotStarted)?;
   let file = state.plugins_dir().join(format!("{name}.js"));
   let write = || {
      std::fs::create_dir_all(state.plugins_dir())?;
      std::fs::write(&file, &source)
   };
   write().map_err(io_error(&file))?;
   if let Some(mut engine) = engine() {
      engine.install(name, source);
   }
   Ok(())
}

/// A plugin's current code.
#[must_use]
pub fn source(name: &str) -> Option<String> {
   engine()?.source(name)
}

/// What an uninstall left in the plugin's place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uninstalled {
   Removed,
   /// The plugin had replaced a bundled one, which runs again as the user
   /// had it.
   RestoredBundled,
}

/// Deletes a plugin installed into `home/plugins` and unloads it, putting
/// back the bundled plugin it replaced.
pub fn uninstall(name: &str) -> Result<Uninstalled, Error> {
   let state = STATE.get().ok_or(Error::NotStarted)?;
   let file = state.plugins_dir().join(format!("{name}.js"));
   std::fs::remove_file(&file).map_err(io_error(&file))?;
   Ok(engine().map_or(Uninstalled::Removed, |mut engine| engine.uninstall(name)))
}

/// Evaluates an enabled plugin again with its hooks cleared.
pub fn reload(name: &str) {
   if let Some(mut engine) = engine() {
      engine.reload(name);
   }
}

/// Reloads every currently enabled plugin.
pub fn reload_all() {
   let names = plugins()
      .into_iter()
      .filter(|plugin| plugin.enabled)
      .map(|plugin| plugin.name)
      .collect::<Vec<_>>();
   for name in names {
      reload(&name);
   }
}

/// The first loaded library named in [`LIBRARIES`].
pub fn target() -> Result<Image<'static>, Error> {
   // SAFETY: the target is the host app's own library, which stays mapped for
   // the life of the process, and scans run before the host's threads touch its
   // data.
   let images = unsafe { Image::loaded() };
   images
      .into_iter()
      .find(|image| LIBRARIES.contains(&image.file_name()))
      .ok_or(Error::NoTarget)
}

/// Points the gateway registry's handlers at phoenix.
pub fn hook(image: &Image<'static>) -> Result<&'static GatewayRegistry, Error> {
   let walked = GatewayRegistry::locate(image)?;
   log::info!("gateway registry: {} events", walked.iter().count());
   gateway::swap(image, walked)
}

/// What [`start`] found and loaded.
pub struct Started {
   pub target:  String,
   pub build:   Option<BuildId>,
   pub plugins: Vec<String>,
}

/// Starts phoenix on `platform` against the first loaded library named in
/// [`LIBRARIES`]. `bundled` plugins load first, and a file in `home/plugins`
/// with the same name replaces one.
pub fn start(
   platform: &'static dyn Platform,
   home: PathBuf,
   bundled: Vec<Plugin>,
) -> Result<Started, Error> {
   let image = target()?;
   let registry = hook(&image)?;
   start_hooked(platform, image, registry, home, bundled)
}

/// [`start`] for a target whose registry [`hook`] already swapped.
pub fn start_hooked(
   platform: &'static dyn Platform,
   image: Image<'static>,
   registry: &'static GatewayRegistry,
   home: PathBuf,
   bundled: Vec<Plugin>,
) -> Result<Started, Error> {
   set_platform(platform);
   let engine = js::Engine::new(registry)?;
   let state = State {
      image,
      home,
      js: Mutex::new(engine),
   };
   STATE.set(state).map_err(|_| Error::AlreadyStarted)?;
   let state = STATE.get().expect("STATE was set above");

   let first_run = !state.choices_path().exists();
   platform.before_plugins(&state.home);
   let plugins = js::load_plugins(state, bundled)?;
   platform.started(first_run, &plugins);
   Ok(Started {
      target: state.image.path.clone(),
      build: state.image.build_id,
      plugins,
   })
}

impl State {
   pub fn storage_path(&self, plugin: &str) -> PathBuf {
      self.home.join("storage").join(format!("{plugin}.json"))
   }

   pub fn choices_path(&self) -> PathBuf {
      self.home.join("plugins.json")
   }

   /// The app's data directory holding both phoenix's home and Phoenix's
   /// `store/<account>/cache.sqlite`.
   pub fn files_dir(&self) -> PathBuf {
      self
         .home
         .parent()
         .map_or_else(|| self.home.clone(), Path::to_path_buf)
   }

   pub fn plugins_dir(&self) -> PathBuf {
      self.home.join("plugins")
   }
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
   move |source| {
      Error::Io {
         path: path.to_owned(),
         source,
      }
   }
}
