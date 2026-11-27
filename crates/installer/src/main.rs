//! `rubycord-installer.exe`: a window with two buttons, Inject and Un-Inject.
//!
//! Inject puts the launcher (which carries the loader) in `%LOCALAPPDATA%\RubyCord` and points
//! the Discord Phoenix shortcuts at it, so opening Discord Phoenix the usual way loads RubyCord.
//! Un-Inject points them back and deletes what Inject made. Discord's own files are never
//! touched, and neither button needs administrator rights.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
   eprintln!("rubycord-installer only runs on Windows");
}

#[cfg(windows)]
fn main() -> eframe::Result {
   app::run()
}

#[cfg(windows)]
mod app {
   use std::{
      env, fs,
      os::windows::process::CommandExt,
      path::PathBuf,
      process::Command,
      sync::{Arc, Mutex, PoisonError},
      thread,
   };

   use eframe::egui::{
      self, Color32, CornerRadius, FontId, RichText, Stroke, StrokeKind, TextureOptions, vec2,
   };

   const BACKGROUND: Color32 = Color32::from_rgb(0x14, 0x15, 0x18);
   const CARD: Color32 = Color32::from_rgb(0x1D, 0x1E, 0x22);
   const BORDER: Color32 = Color32::from_rgb(0x2B, 0x2D, 0x33);
   const TEXT: Color32 = Color32::from_rgb(0xE8, 0xE9, 0xEC);
   const MUTED: Color32 = Color32::from_rgb(0x8B, 0x8F, 0x98);
   const RUBY: Color32 = Color32::from_rgb(0xE1, 0x1D, 0x48);
   const RUBY_HOVER: Color32 = Color32::from_rgb(0xF4, 0x3F, 0x5E);
   const OK: Color32 = Color32::from_rgb(0x3C, 0xCF, 0x7A);
   const WARN: Color32 = Color32::from_rgb(0xE8, 0xB3, 0x39);
   const ERROR: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x6B);

   const CREATE_NO_WINDOW: u32 = 0x0800_0000;

   /// The launcher CI built, which carries the loader. Empty in a plain build.
   static LAUNCHER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rubycord.exe"));

   /// What the last button press ended in.
   #[derive(Clone)]
   enum Work {
      Idle,
      Busy(&'static str),
      Done(Result<String, String>),
   }

   struct App {
      logo: Option<egui::TextureHandle>,
      work: Arc<Mutex<Work>>,
      phoenix: bool,
      injected: bool,
      /// Whether the screen has read the disk since the last job finished.
      settled: bool,
   }

   pub fn run() -> eframe::Result {
      let icon = icon_data(64);
      let options = eframe::NativeOptions {
         viewport: egui::ViewportBuilder::default()
            .with_title("RubyCord Installer")
            .with_inner_size([460.0, 600.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_icon(Arc::new(icon)),
         centered: true,
         ..Default::default()
      };
      eframe::run_native(
         "RubyCord Installer",
         options,
         Box::new(|cc| {
            style(&cc.egui_ctx);
            Ok(Box::new(App::new(&cc.egui_ctx)))
         }),
      )
   }

   fn icon_data(size: usize) -> egui::IconData {
      let rgba = phoenix_ui_desktop::logo::pixels(size)
         .into_iter()
         .flat_map(|argb| {
            let [a, r, g, b] = argb.to_be_bytes();
            [r, g, b, a]
         })
         .collect();
      egui::IconData {
         rgba,
         width: size as u32,
         height: size as u32,
      }
   }

   fn style(ctx: &egui::Context) {
      let mut visuals = egui::Visuals::dark();
      visuals.panel_fill = BACKGROUND;
      visuals.override_text_color = Some(TEXT);
      ctx.set_visuals(visuals);
   }

   fn local() -> PathBuf {
      PathBuf::from(env::var_os("LOCALAPPDATA").unwrap_or_default())
   }

   fn home() -> PathBuf {
      local().join("RubyCord")
   }

   fn launcher() -> PathBuf {
      home().join("rubycord.exe")
   }

   impl App {
      fn new(ctx: &egui::Context) -> Self {
         let icon = icon_data(128);
         let image = egui::ColorImage::from_rgba_unmultiplied(
            [icon.width as usize, icon.height as usize],
            &icon.rgba,
         );
         let mut app = Self {
            logo: Some(ctx.load_texture("logo", image, TextureOptions::LINEAR)),
            work: Arc::new(Mutex::new(Work::Idle)),
            phoenix: false,
            injected: false,
            settled: true,
         };
         app.refresh();
         app
      }

      fn refresh(&mut self) {
         self.phoenix = local().join(r"DiscordPhoenix\app\Discord.exe").is_file();
         self.injected = launcher().is_file();
      }

      fn state(&self) -> Work {
         self
            .work
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
      }

      /// Runs `job` off the UI thread, with `label` on screen meanwhile.
      fn start(
         &mut self,
         ctx: &egui::Context,
         label: &'static str,
         job: fn() -> Result<String, String>,
      ) {
         self.settled = false;
         *self.work.lock().unwrap_or_else(PoisonError::into_inner) = Work::Busy(label);
         let (work, ctx) = (self.work.clone(), ctx.clone());
         thread::spawn(move || {
            let result = job();
            *work.lock().unwrap_or_else(PoisonError::into_inner) = Work::Done(result);
            ctx.request_repaint();
         });
      }
   }

   /// Puts the launcher and its loader in place and points Discord Phoenix's shortcuts at it.
   fn inject() -> Result<String, String> {
      if LAUNCHER.is_empty() {
         return Err(
            "This copy was built without the launcher. Download the installer from the releases."
               .to_owned(),
         );
      }
      fs::create_dir_all(home())
         .map_err(|err| format!("Could not create {}: {err}", home().display()))?;
      let launcher = launcher();
      // A launcher left running keeps its file, which Windows lets us rename but not overwrite.
      let old = launcher.with_extension("exe.old");
      let _ = fs::remove_file(&old);
      if launcher.is_file() {
         let _ = fs::rename(&launcher, &old);
      }
      fs::write(&launcher, LAUNCHER)
         .map_err(|err| format!("Could not write the launcher: {err}"))?;
      let _ = fs::remove_file(&old);
      let status = Command::new(&launcher)
         .arg("--install")
         .creation_flags(CREATE_NO_WINDOW)
         .status()
         .map_err(|err| format!("Could not run the launcher: {err}"))?;
      match status.code() {
         Some(0) => {
            Ok("Injected. Open Discord Phoenix as usual and RubyCord loads with it.".to_owned())
         },
         Some(2) => Err(
            "Discord Phoenix is not installed. Install it first, then press Inject again."
               .to_owned(),
         ),
         _ => {
            let detail = fs::read_to_string(home().join("last-error.txt")).unwrap_or_default();
            Err(format!(
               "The launcher failed. {}",
               detail.lines().next().unwrap_or("")
            ))
         },
      }
   }

   /// Points the shortcuts back at Discord Phoenix and deletes what Inject made.
   fn uninject() -> Result<String, String> {
      let launcher = launcher();
      if launcher.is_file() {
         // The launcher restores the shortcuts, but it cannot delete the file it runs from.
         let _ = Command::new(&launcher)
            .args(["--uninstall", "--quiet"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
      }
      match fs::remove_dir_all(home()) {
         Ok(()) => Ok("Un-injected. Discord Phoenix opens as it did before.".to_owned()),
         Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok("Nothing to remove: RubyCord was not injected.".to_owned())
         },
         Err(_) => Ok(
            "Shortcuts restored. Discord is still using some files: close it, then press Un-Inject again to delete them."
               .to_owned(),
         ),
      }
   }

   /// Opens Discord Phoenix through the launcher, which also offers to restart a running one.
   fn open_discord() {
      let _ = Command::new(launcher()).spawn();
   }

   impl eframe::App for App {
      fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
         let ctx = ui.ctx().clone();
         let state = self.state();
         let busy = matches!(state, Work::Busy(_));
         if matches!(state, Work::Done(_)) && !self.settled {
            self.refresh();
            self.settled = true;
         }
         egui::CentralPanel::default()
            .frame(
               egui::Frame::new()
                  .fill(BACKGROUND)
                  .inner_margin(egui::Margin::symmetric(32, 28)),
            )
            .show(ui, |ui| {
               ui.vertical_centered(|ui| {
                  if let Some(logo) = &self.logo {
                     ui.add(egui::Image::new(logo).fit_to_exact_size(vec2(104.0, 104.0)));
                  }
                  ui.add_space(8.0);
                  ui.label(
                     RichText::new("RubyCord")
                        .font(FontId::proportional(32.0))
                        .strong(),
                  );
                  ui.label(
                     RichText::new("Plugins, themes and tweaks for Discord Phoenix").color(MUTED),
                  );
               });
               ui.add_space(22.0);
               self.status_card(ui);
               ui.add_space(22.0);

               let width = ui.available_width();
               let inject_ready = self.phoenix && !busy;
               if big_button(ui, width, "Inject", true, inject_ready).clicked() {
                  self.start(&ctx, "Injecting…", inject);
               }
               ui.add_space(10.0);
               let uninject_ready = self.injected && !busy;
               if big_button(ui, width, "Un-Inject", false, uninject_ready).clicked() {
                  self.start(&ctx, "Removing…", uninject);
               }
               ui.add_space(18.0);
               self.message(ui, &state);
               ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                  ui.label(
                     RichText::new(format!("RubyCord {}", env!("CARGO_PKG_VERSION")))
                        .color(MUTED)
                        .size(12.0),
                  );
               });
            });
      }
   }

   impl App {
      fn status_card(&self, ui: &mut egui::Ui) {
         egui::Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(18, 14))
            .show(ui, |ui| {
               ui.set_width(ui.available_width());
               row(
                  ui,
                  "Discord Phoenix",
                  if self.phoenix {
                     "found"
                  } else {
                     "not installed"
                  },
                  if self.phoenix { OK } else { WARN },
               );
               ui.add_space(8.0);
               row(
                  ui,
                  "RubyCord",
                  if self.injected {
                     "injected"
                  } else {
                     "not injected"
                  },
                  if self.injected { OK } else { MUTED },
               );
            });
      }

      fn message(&self, ui: &mut egui::Ui, state: &Work) {
         match state {
            Work::Idle => {
               if !self.phoenix {
                  ui.label(
                     RichText::new("Discord Phoenix was not found. Install it first.").color(WARN),
                  );
               }
            },
            Work::Busy(label) => {
               ui.horizontal(|ui| {
                  ui.spinner();
                  ui.label(RichText::new(*label).color(MUTED));
               });
            },
            Work::Done(Ok(text)) => {
               ui.label(RichText::new(text).color(OK));
               if self.injected && ui.link("Open Discord Phoenix now").clicked() {
                  open_discord();
               }
            },
            Work::Done(Err(text)) => {
               ui.label(RichText::new(text).color(ERROR));
            },
         }
      }
   }

   fn row(ui: &mut egui::Ui, name: &str, value: &str, color: Color32) {
      ui.horizontal(|ui| {
         ui.label(RichText::new(name).size(15.0));
         ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).color(color).size(15.0));
            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 4.0, color);
         });
      });
   }

   /// A full-width rounded button: filled in ruby red for the main action, outlined otherwise.
   fn big_button(
      ui: &mut egui::Ui,
      width: f32,
      label: &str,
      primary: bool,
      enabled: bool,
   ) -> egui::Response {
      let sense = if enabled {
         egui::Sense::click()
      } else {
         egui::Sense::hover()
      };
      let (rect, response) = ui.allocate_exact_size(vec2(width, 54.0), sense);
      let hovered = enabled && response.hovered();
      let (fill, text) = match (primary, enabled) {
         (true, true) => (if hovered { RUBY_HOVER } else { RUBY }, Color32::WHITE),
         (true, false) => (Color32::from_rgb(0x4A, 0x1B, 0x26), MUTED),
         (false, true) => (if hovered { CARD } else { BACKGROUND }, TEXT),
         (false, false) => (BACKGROUND, Color32::from_rgb(0x55, 0x58, 0x60)),
      };
      let radius = CornerRadius::same(12);
      ui.painter().rect_filled(rect, radius, fill);
      if !primary {
         let edge = if hovered { MUTED } else { BORDER };
         ui.painter()
            .rect_stroke(rect, radius, Stroke::new(1.5, edge), StrokeKind::Inside);
      }
      ui.painter().text(
         rect.center(),
         egui::Align2::CENTER_CENTER,
         label,
         FontId::proportional(18.0),
         text,
      );
      if enabled {
         response.on_hover_cursor(egui::CursorIcon::PointingHand)
      } else {
         response
      }
   }
}
