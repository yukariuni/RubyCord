//! Desktop UI host primitives for Phoenix (Windows / macOS / Linux).
//!
//! Desktop Phoenix renders natively in Rust with GPUI rather than Android's
//! serialized UI-op bridge (`phoenix-ui-mobile`). This crate owns the
//! desktop-side UI interactions: clipboard, opening URLs, the system tray,
//! native notifications, and the `DevTools` page served over localhost.
//!
//! Each OS implements [`DesktopHost`]; everything above it is shared here.

#[cfg(target_os = "linux")] mod linux;
#[cfg(target_os = "macos")] mod macos;
#[cfg(windows)] mod webview;
#[cfg(windows)] mod windows;

use std::{
   path::PathBuf,
   sync::{
      Arc,
      OnceLock,
   },
};

#[cfg(target_os = "linux")] use linux::Host as Os;
#[cfg(target_os = "macos")] use macos::Host as Os;
#[cfg(target_os = "macos")]
pub use macos::{
   PanelAction,
   PanelButton,
   PanelSection,
   close_panel,
   redraw_windows,
   show_panel,
};
#[cfg(windows)] use windows::Host as Os;

/// Self-contained HTML/JS/CSS companion dashboard for Desktop settings,
/// plugin management, catalog installs, OTA updates, and JS console.
pub const DASHBOARD_HTML: &str = include_str!("dashboard.html");

#[derive(Debug, flub::Error)]
pub enum Error {
   #[error("desktop ui: {message}")]
   Platform { message: String },
   #[error("desktop ui: unsupported on this target")]
   Unsupported,
}

/// Configuration and runtime callbacks for the desktop system tray.
pub struct TrayConfig {
   /// Starts the debug shell if it isn't running and returns the `DevTools`
   /// URL.
   pub devtools_url:     Arc<dyn Fn() -> String + Send + Sync>,
   pub plugins_dir:      PathBuf,
   pub on_reload_all:    Option<Arc<dyn Fn() + Send + Sync>>,
   pub on_check_updates: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// What the tray menu offers, the same on every OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
   not(any(windows, target_os = "linux", target_os = "macos")),
   allow(dead_code, reason = "no tray backend on this target")
)]
enum TrayAction {
   OpenDevtools,
   ReloadAll,
   CheckUpdates,
   CopyUrl,
   OpenPlugins,
}

impl TrayAction {
   /// The menu in order, `None` for a separator.
   #[cfg_attr(
      not(any(target_os = "linux", target_os = "macos", test)),
      expect(
         dead_code,
         reason = "the Windows menu has its own labels with hotkey hints"
      )
   )]
   const MENU: [Option<Self>; 7] = [
      Some(Self::OpenDevtools),
      None,
      Some(Self::ReloadAll),
      Some(Self::CheckUpdates),
      None,
      Some(Self::CopyUrl),
      Some(Self::OpenPlugins),
   ];

   #[cfg_attr(
      not(any(target_os = "linux", target_os = "macos", test)),
      expect(
         dead_code,
         reason = "the Windows menu has its own labels with hotkey hints"
      )
   )]
   const fn label(self) -> &'static str {
      match self {
         Self::OpenDevtools => "Open DevTools",
         Self::ReloadAll => "Reload All Plugins",
         Self::CheckUpdates => "Check for Updates",
         Self::CopyUrl => "Copy DevTools URL",
         Self::OpenPlugins => "Open Plugins Folder",
      }
   }

   #[cfg_attr(
      not(any(windows, target_os = "linux", target_os = "macos")),
      allow(dead_code, reason = "no tray backend on this target")
   )]
   fn run(self) {
      let Some(config) = config() else {
         return;
      };
      match self {
         Self::OpenDevtools => open_devtools(),
         Self::ReloadAll => {
            if let Some(reload) = &config.on_reload_all {
               reload();
            }
         },
         Self::CheckUpdates => {
            if let Some(check) = &config.on_check_updates {
               check();
            }
         },
         Self::CopyUrl => {
            let url = (config.devtools_url)();
            if copy(&url).is_ok() {
               Os::notify("Ruby", &format!("Copied {url}"));
            }
         },
         Self::OpenPlugins => {
            let _ = std::fs::create_dir_all(&config.plugins_dir);
            if let Err(err) = Os::open(&config.plugins_dir.to_string_lossy()) {
               log::warn!("tray: could not open the plugins folder: {err}");
            }
         },
      }
   }
}

/// The OS primitives a desktop backend provides. The defaults are what a
/// target without a backend gets.
trait DesktopHost {
   fn copy(_text: &str) -> Result<(), Error> {
      Err(Error::Unsupported)
   }

   /// Opens a URL or local path with the system handler. Callers have
   /// already checked the URL scheme.
   fn open(_target: &str) -> Result<(), Error> {
      Err(Error::Unsupported)
   }

   /// Asks for a `.js` file with the native picker.
   fn pick_js_path() -> Option<PathBuf> {
      None
   }

   fn notify(_title: &str, _body: &str) {}

   /// Starts the tray and hotkeys after [`config`] is set.
   fn start_tray() {}
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
struct Os;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
impl DesktopHost for Os {}

static CONFIG: OnceLock<TrayConfig> = OnceLock::new();

fn config() -> Option<&'static TrayConfig> {
   CONFIG.get()
}

/// Starts the desktop system tray icon, which opens `DevTools`.
pub fn start_tray(config: TrayConfig) {
   let _ = CONFIG.set(config);
   Os::start_tray();
}

/// The `DevTools` shortcut as Ruby's pages name it: Command on macOS, where
/// Control-Shift-I would be out of place, Control elsewhere.
pub const DEVTOOLS_SHORTCUT: &str = if cfg!(target_os = "macos") {
   "Cmd+Shift+I"
} else {
   "Ctrl+Shift+I"
};

/// Opens the Ruby `DevTools` / Runtime Console in the default browser
/// ([`DEVTOOLS_SHORTCUT`] while Discord Phoenix is focused).
pub fn open_devtools() {
   if let Some(config) = config() {
      open_page(&(config.devtools_url)());
   }
}

fn open_page(url: &str) {
   if let Err(err) = open_url(url) {
      log::warn!("desktop ui: could not open {url}: {err}");
   }
}

/// Copies `text` to the system clipboard.
pub fn copy(text: &str) -> Result<(), Error> {
   Os::copy(text)
}

/// Opens the native OS file picker for choosing a `.js` plugin bundle to
/// install (`(file_name, bytes)`).
#[must_use]
pub fn pick_js_file() -> Option<(String, Vec<u8>)> {
   let path = Os::pick_js_path()?;
   let name = path.file_name()?.to_string_lossy().into_owned();
   let bytes = std::fs::read(&path).ok()?;
   Some((name, bytes))
}

/// Opens `url` in the system's default browser / handler.
pub fn open_url(url: &str) -> Result<(), Error> {
   if !url.starts_with("https://") && !url.starts_with("http://") && !url.starts_with("discord://")
   {
      return Err(Error::Platform {
         message: format!("refusing to open non-web URL: {url}"),
      });
   }
   Os::open(url)
}

/// Shows a short desktop toast / notification message.
pub fn toast(message: &str) {
   log::info!("[toast] {message}");
   Os::notify("Ruby", message);
}

/// Shows a titled desktop notification.
pub fn notify(title: &str, body: Option<&str>) {
   match body {
      Some(body) if !body.is_empty() => {
         log::info!("[notify] {title}: {body}");
         Os::notify(title, body);
      },
      _ => {
         log::info!("[notify] {title}");
         Os::notify("Ruby", title);
      },
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn rejects_unsafe_url_schemes() {
      assert!(open_url("file:///C:/Windows/notepad.exe").is_err());
      assert!(open_url("javascript:alert(1)").is_err());
   }

   #[test]
   fn tray_menu_lists_every_action_once() {
      let actions = TrayAction::MENU.iter().flatten().collect::<Vec<_>>();
      assert_eq!(actions.len(), 5);
      for (at, action) in actions.iter().enumerate() {
         assert!(!actions[at + 1..].contains(action));
         assert_ne!(action.label(), "");
      }
   }

   #[test]
   fn dashboard_html_is_embedded() {
      assert!(DASHBOARD_HTML.contains("<title>Ruby"));
      assert!(DASHBOARD_HTML.contains("/eval"));
      assert!(DASHBOARD_HTML.contains("/logs"));
   }
}
