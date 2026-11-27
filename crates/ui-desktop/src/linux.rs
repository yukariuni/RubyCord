//! Linux desktop backend through the usual freedesktop command-line helpers,
//! so the crate links nothing beyond libc.

use std::{
   env,
   io::{
      BufRead as _,
      BufReader,
      Write as _,
   },
   path::PathBuf,
   process::{
      Command,
      Stdio,
   },
   sync::Once,
   thread,
};

use crate::{
   DesktopHost,
   Error,
   TrayAction,
};

/// The tray icon as an `AppIndicator` (`StatusNotifierItem`, which KDE shows
/// natively and GNOME with its `AppIndicator` extension), run by the system
/// Python's GTK bindings so Discord's process loads no GTK. Takes the icon,
/// then one label per [`TrayAction::MENU`] entry, empty for a separator.
/// Prints the index of each clicked entry and exits when its stdin closes,
/// which is when Discord exits.
const TRAY_PY: &str = r#"
import sys, gi
gi.require_version("Gtk", "3.0")
from gi.repository import Gtk, GLib
try:
    gi.require_version("AyatanaAppIndicator3", "0.1")
    from gi.repository import AyatanaAppIndicator3 as AI
except (ValueError, ImportError):
    gi.require_version("AppIndicator3", "0.1")
    from gi.repository import AppIndicator3 as AI
tray = AI.Indicator.new("ruby", sys.argv[1], AI.IndicatorCategory.APPLICATION_STATUS)
tray.set_title("Ruby")
menu = Gtk.Menu()
for at, label in enumerate(sys.argv[2:]):
    item = Gtk.MenuItem(label=label) if label else Gtk.SeparatorMenuItem()
    if label:
        item.connect("activate", lambda _, at=at: print(at, flush=True))
    menu.append(item)
menu.show_all()
tray.set_menu(menu)
tray.set_status(AI.IndicatorStatus.ACTIVE)
GLib.io_add_watch(0, GLib.IO_IN | GLib.IO_HUP | GLib.IO_ERR, lambda *_: Gtk.main_quit())
Gtk.main()
"#;

fn pipe_to(mut command: Command, text: &str) -> bool {
   let Ok(mut child) = command
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
   else {
      return false;
   };
   let written = child
      .stdin
      .take()
      .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
   child.wait().is_ok_and(|status| status.success()) && written
}

/// Spawns `command` and waits for it on a background thread, so it never
/// lingers as a zombie on Unix.
fn spawn_reaped(command: &mut Command) -> Result<(), Error> {
   let mut child = command.spawn().map_err(|err| {
      Error::Platform {
         message: format!("{}: {err}", command.get_program().display()),
      }
   })?;
   let _ = thread::Builder::new()
      .name("ruby-reap".to_owned())
      .spawn(move || child.wait());
   Ok(())
}

pub struct Host;

impl DesktopHost for Host {
   fn copy(text: &str) -> Result<(), Error> {
      let wayland = env::var_os("WAYLAND_DISPLAY").is_some();
      let mut tools = Vec::with_capacity(3);
      if wayland {
         tools.push(Command::new("wl-copy"));
      }
      let mut xclip = Command::new("xclip");
      xclip.args(["-selection", "clipboard"]);
      let mut xsel = Command::new("xsel");
      xsel.args(["--clipboard", "--input"]);
      tools.extend([xclip, xsel]);
      if tools.into_iter().any(|tool| pipe_to(tool, text)) {
         return Ok(());
      }
      Err(Error::Platform {
         message: "no clipboard tool worked (wl-copy, xclip, xsel)".to_owned(),
      })
   }

   fn open(target: &str) -> Result<(), Error> {
      spawn_reaped(Command::new("xdg-open").arg(target))
   }

   fn pick_js_path() -> Option<PathBuf> {
      let mut zenity = Command::new("zenity");
      zenity.args([
         "--file-selection",
         "--title=Install Ruby Plugin (.js)",
         "--file-filter=JavaScript Plugins | *.js",
         "--file-filter=All Files | *",
      ]);
      let mut kdialog = Command::new("kdialog");
      kdialog.args([
         "--title",
         "Install Ruby Plugin (.js)",
         "--getopenfilename",
         ".",
         "*.js|JavaScript Plugins",
      ]);
      // The first dialog that starts decides; a cancel there is final.
      let output = [zenity, kdialog]
         .into_iter()
         .find_map(|mut dialog| dialog.stderr(Stdio::null()).output().ok())?;
      let path = String::from_utf8(output.stdout).ok()?;
      let path = path.trim_end_matches('\n');
      (output.status.success() && !path.is_empty()).then(|| PathBuf::from(path))
   }

   fn notify(title: &str, body: &str) {
      let mut send = Command::new("notify-send");
      send.args(["--app-name=Ruby", "--", title, body]);
      if let Err(err) = spawn_reaped(&mut send) {
         log::debug!("notify: {err}");
      }
   }

   fn start_tray() {
      static STARTED: Once = Once::new();
      STARTED.call_once(|| {
         if let Err(err) = start_indicator() {
            log::info!("tray: {err}, press Ctrl+Shift+I in Discord for Ruby DevTools");
         }
      });
   }
}

/// Discord's own icon from its `AppImage` or package, else its icon name.
fn icon() -> String {
   env::current_exe()
      .ok()
      .and_then(|exe| {
         Some(
            exe.parent()?
               .parent()?
               .join("share/icons/hicolor/64x64/apps"),
         )
      })
      .map(|dir| dir.join("com.discord.libdiscord.png"))
      .filter(|png| png.is_file())
      .map_or_else(
         || "com.discord.libdiscord".to_owned(),
         |png| png.to_string_lossy().into_owned(),
      )
}

fn start_indicator() -> Result<(), Error> {
   let mut python = Command::new("python3");
   python
      .args(["-I", "-c", TRAY_PY])
      .arg(icon())
      .args(TrayAction::MENU.map(|entry| entry.map_or("", TrayAction::label)))
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::null());
   let mut child = python.spawn().map_err(|err| {
      Error::Platform {
         message: format!("python3 for the tray: {err}"),
      }
   })?;
   let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
      return Err(Error::Platform {
         message: "tray pipes missing".to_owned(),
      });
   };
   thread::Builder::new()
      .name("ruby-tray".to_owned())
      .spawn(move || {
         for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(action) = line
               .trim()
               .parse::<usize>()
               .ok()
               .and_then(|at| TrayAction::MENU.get(at).copied().flatten())
            {
               action.run();
            }
         }
         drop(stdin);
         if child.wait().is_ok_and(|status| !status.success()) {
            log::info!(
               "tray: needs python3 with GTK 3 and AppIndicator bindings; press Ctrl+Shift+I in \
                Discord for Ruby DevTools"
            );
         }
      })
      .map(drop)
      .map_err(|err| {
         Error::Platform {
            message: format!("tray thread: {err}"),
         }
      })
}
