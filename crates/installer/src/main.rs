//! `rubycord-installer.exe`: a window with two buttons, Inject and Un-Inject, and a small terminal
//! that lists every step as it happens.
//!
//! Inject puts the launcher (which carries the loader) in `%LOCALAPPDATA%\RubyCord` and points
//! the user's Discord Phoenix shortcuts at it, so opening Discord Phoenix the usual way loads
//! RubyCord. Un-Inject points them back and deletes what Inject made. Discord's own files are
//! never touched, and neither button needs administrator rights.

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
      io::{BufRead, BufReader},
      os::windows::process::CommandExt,
      path::PathBuf,
      process::{Command, Stdio},
      sync::{Arc, Mutex, PoisonError},
      thread,
      time::Duration,
   };

   use eframe::egui::{
      self, Color32, CornerRadius, FontId, Margin, PointerButton, RichText, Stroke, StrokeKind,
      TextureOptions, ViewportCommand, pos2, vec2,
   };

   const BACKGROUND: Color32 = Color32::from_rgb(0x14, 0x15, 0x18);
   const CARD: Color32 = Color32::from_rgb(0x1D, 0x1E, 0x22);
   const TERMINAL: Color32 = Color32::from_rgb(0x0B, 0x0C, 0x0E);
   const BORDER: Color32 = Color32::from_rgb(0x2B, 0x2D, 0x33);
   const TEXT: Color32 = Color32::from_rgb(0xE8, 0xE9, 0xEC);
   const OUTPUT: Color32 = Color32::from_rgb(0xB4, 0xB8, 0xC2);
   const MUTED: Color32 = Color32::from_rgb(0x8B, 0x8F, 0x98);
   const RUBY: Color32 = Color32::from_rgb(0xE1, 0x1D, 0x48);
   const RUBY_HOVER: Color32 = Color32::from_rgb(0xF4, 0x3F, 0x5E);
   const CLOSE_HOVER: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
   const OK: Color32 = Color32::from_rgb(0x3C, 0xCF, 0x7A);
   const WARN: Color32 = Color32::from_rgb(0xE8, 0xB3, 0x39);
   const ERROR: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x6B);

   const CREATE_NO_WINDOW: u32 = 0x0800_0000;
   const TITLE_BAR_HEIGHT: f32 = 38.0;
   const TERMINAL_HEIGHT: f32 = 176.0;

   /// The launcher CI built, which carries the loader. Empty in a plain build.
   static LAUNCHER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rubycord.exe"));

   /// What a line in the terminal is.
   #[derive(Clone, Copy)]
   enum Kind {
      /// A command, after a prompt.
      Cmd,
      Info,
      Ok,
      Warn,
      Error,
   }

   struct Line {
      kind: Kind,
      text: String,
   }

   /// Where a job writes its lines, and wakes the window to show them.
   #[derive(Clone)]
   struct Sink {
      lines: Arc<Mutex<Vec<Line>>>,
      ctx: egui::Context,
   }

   impl Sink {
      fn say(&self, kind: Kind, text: impl Into<String>) {
         let mut text = text.into();
         let local = local().to_string_lossy().into_owned();
         if !local.is_empty() {
            text = text.replace(&local, "%LOCALAPPDATA%");
         }
         self
            .lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Line { kind, text });
         self.ctx.request_repaint();
      }

      /// A line the launcher printed: `ok ` and `warn ` mark the ones to colour.
      fn launcher_line(&self, line: &str) {
         if let Some(text) = line.strip_prefix("ok ") {
            self.say(Kind::Ok, text);
         } else if let Some(text) = line.strip_prefix("warn ") {
            self.say(Kind::Warn, text);
         } else if !line.trim().is_empty() {
            self.say(Kind::Info, line.trim_end());
         }
      }
   }

   /// How the last job ended.
   #[derive(Clone, Copy, PartialEq)]
   enum Work {
      Idle,
      Busy,
      Injected,
      Removed,
      Failed,
   }

   struct App {
      logo: egui::TextureHandle,
      work: Arc<Mutex<Work>>,
      lines: Arc<Mutex<Vec<Line>>>,
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
            .with_inner_size([460.0, 690.0])
            .with_resizable(false)
            .with_decorations(false)
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

   fn discord() -> PathBuf {
      local()
         .join("DiscordPhoenix")
         .join("app")
         .join("Discord.exe")
   }

   fn megabytes(bytes: usize) -> String {
      format!("{:.1} MB", bytes as f64 / 1_048_576.0)
   }

   impl App {
      fn new(ctx: &egui::Context) -> Self {
         let icon = icon_data(128);
         let image = egui::ColorImage::from_rgba_unmultiplied(
            [icon.width as usize, icon.height as usize],
            &icon.rgba,
         );
         let mut app = Self {
            logo: ctx.load_texture("logo", image, TextureOptions::LINEAR),
            work: Arc::new(Mutex::new(Work::Idle)),
            lines: Arc::new(Mutex::new(Vec::new())),
            phoenix: false,
            injected: false,
            settled: true,
         };
         app.refresh();
         app.greet(ctx);
         app
      }

      /// The first lines of the terminal: what was found.
      fn greet(&self, ctx: &egui::Context) {
         let out = self.sink(ctx);
         out.say(Kind::Cmd, format!("rubycord {}", env!("CARGO_PKG_VERSION")));
         if self.phoenix {
            out.say(
               Kind::Ok,
               format!("Discord Phoenix found: {}", discord().display()),
            );
         } else {
            out.say(Kind::Warn, "Discord Phoenix not found: install it first");
         }
         out.say(
            Kind::Info,
            if self.injected {
               "RubyCord is injected"
            } else {
               "RubyCord is not injected"
            },
         );
      }

      fn refresh(&mut self) {
         self.phoenix = discord().is_file();
         self.injected = launcher().is_file();
      }

      fn sink(&self, ctx: &egui::Context) -> Sink {
         Sink {
            lines: self.lines.clone(),
            ctx: ctx.clone(),
         }
      }

      fn state(&self) -> Work {
         *self.work.lock().unwrap_or_else(PoisonError::into_inner)
      }

      /// Runs `job` off the UI thread; its lines show up in the terminal as they come.
      fn start(&mut self, ctx: &egui::Context, job: fn(&Sink) -> Work) {
         self.settled = false;
         *self.work.lock().unwrap_or_else(PoisonError::into_inner) = Work::Busy;
         let (work, out) = (self.work.clone(), self.sink(ctx));
         thread::spawn(move || {
            let result = job(&out);
            *work.lock().unwrap_or_else(PoisonError::into_inner) = result;
            out.ctx.request_repaint();
         });
      }
   }

   /// Runs the launcher with `args` and shows what it prints. `None` when it could not start.
   fn run_launcher(out: &Sink, args: &[&str]) -> Option<i32> {
      let mut child = Command::new(launcher())
         .args(args)
         .creation_flags(CREATE_NO_WINDOW)
         .stdin(Stdio::null())
         .stdout(Stdio::piped())
         .stderr(Stdio::null())
         .spawn()
         .map_err(|err| out.say(Kind::Error, format!("could not run the launcher: {err}")))
         .ok()?;
      if let Some(stdout) = child.stdout.take() {
         for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            out.launcher_line(&line);
         }
      }
      child.wait().ok()?.code()
   }

   /// Puts the launcher and its loader in place and points Discord Phoenix's shortcuts at it.
   fn inject(out: &Sink) -> Work {
      out.say(Kind::Cmd, "rubycord inject");
      let discord = discord();
      if !discord.is_file() {
         out.say(
            Kind::Error,
            "Discord Phoenix is not installed. Install it, then press Inject.",
         );
         return Work::Failed;
      }
      out.say(
         Kind::Ok,
         format!("found Discord Phoenix: {}", discord.display()),
      );
      if LAUNCHER.is_empty() {
         out.say(
            Kind::Error,
            "this copy was built without the launcher: download the installer from the releases",
         );
         return Work::Failed;
      }
      out.say(
         Kind::Info,
         format!(
            "writing the launcher ({}) to {}",
            megabytes(LAUNCHER.len()),
            home().display()
         ),
      );
      if let Err(err) = fs::create_dir_all(home()) {
         out.say(
            Kind::Error,
            format!("could not create {}: {err}", home().display()),
         );
         return Work::Failed;
      }
      let launcher = launcher();
      // A launcher left running keeps its file, which Windows lets us rename but not overwrite.
      let old = launcher.with_extension("exe.old");
      let _ = fs::remove_file(&old);
      if launcher.is_file() {
         let _ = fs::rename(&launcher, &old);
      }
      if let Err(err) = fs::write(&launcher, LAUNCHER) {
         out.say(Kind::Error, format!("could not write the launcher: {err}"));
         return Work::Failed;
      }
      let _ = fs::remove_file(&old);
      out.say(
         Kind::Info,
         "running the launcher: install the loader and the shortcuts",
      );
      match run_launcher(out, &["--install"]) {
         Some(0) => {
            out.say(
               Kind::Ok,
               "injected: open Discord Phoenix from its usual shortcut",
            );
            Work::Injected
         },
         Some(3) => {
            out.say(
               Kind::Error,
               "no Discord Phoenix shortcut to redirect: pin or create one (Desktop, Start Menu or taskbar), then press Inject again",
            );
            Work::Failed
         },
         Some(2) => {
            out.say(Kind::Error, "Discord Phoenix is not installed");
            Work::Failed
         },
         code => {
            let detail = fs::read_to_string(home().join("last-error.txt")).unwrap_or_default();
            out.say(
               Kind::Error,
               format!(
                  "the launcher failed ({code:?}): {}",
                  detail.lines().next().unwrap_or("")
               ),
            );
            Work::Failed
         },
      }
   }

   /// Points the shortcuts back at Discord Phoenix and deletes what Inject made.
   fn uninject(out: &Sink) -> Work {
      out.say(Kind::Cmd, "rubycord un-inject");
      if launcher().is_file() {
         out.say(Kind::Info, "running the launcher: restore the shortcuts");
         // The launcher restores the shortcuts, but it cannot delete the file it runs from.
         run_launcher(out, &["--uninstall", "--quiet"]);
      } else {
         out.say(Kind::Warn, "no launcher found: nothing to restore");
      }
      out.say(Kind::Info, format!("deleting {}", home().display()));
      match fs::remove_dir_all(home()) {
         Ok(()) => out.say(Kind::Ok, "deleted"),
         Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            out.say(Kind::Info, "nothing to delete");
         },
         Err(_) => out.say(
            Kind::Warn,
            "Discord is still using some files: close it, then press Un-Inject again to delete them",
         ),
      }
      out.say(
         Kind::Ok,
         "un-injected: Discord Phoenix opens as it did before",
      );
      Work::Removed
   }

   /// Opens Discord Phoenix through the launcher, which also offers to restart a running one.
   fn open_discord() {
      let _ = Command::new(launcher()).spawn();
   }

   impl eframe::App for App {
      fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
         let ctx = ui.ctx().clone();
         let state = self.state();
         let busy = state == Work::Busy;
         if !busy && !self.settled && state != Work::Idle {
            self.refresh();
            self.settled = true;
         }
         egui::CentralPanel::default()
            .frame(
               egui::Frame::new()
                  .fill(BACKGROUND)
                  .stroke(Stroke::new(1.0, BORDER)),
            )
            .show(ui, |ui| {
               title_bar(ui, &ctx, &self.logo);
               egui::Frame::new()
                  .inner_margin(Margin::symmetric(30, 12))
                  .show(ui, |ui| {
                     ui.vertical_centered(|ui| {
                        ui.add(egui::Image::new(&self.logo).fit_to_exact_size(vec2(96.0, 96.0)));
                        ui.add_space(6.0);
                        ui.label(
                           RichText::new("RubyCord")
                              .font(FontId::proportional(30.0))
                              .strong(),
                        );
                     });
                     ui.add_space(16.0);
                     self.status_card(ui);
                     ui.add_space(16.0);

                     let width = ui.available_width();
                     if big_button(ui, width, "Inject", true, self.phoenix && !busy).clicked() {
                        self.start(&ctx, inject);
                     }
                     ui.add_space(10.0);
                     if big_button(ui, width, "Un-Inject", false, self.injected && !busy).clicked()
                     {
                        self.start(&ctx, uninject);
                     }
                     ui.add_space(16.0);
                     self.terminal(ui, &ctx, busy);
                     ui.add_space(8.0);
                     if state == Work::Injected && ui.link("Open Discord Phoenix now").clicked() {
                        open_discord();
                     }
                     ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                        ui.label(
                           RichText::new(format!("RubyCord {}", env!("CARGO_PKG_VERSION")))
                              .color(MUTED)
                              .size(12.0),
                        );
                     });
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
            .inner_margin(Margin::symmetric(18, 14))
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

      /// The little terminal: every step the job takes, newest at the bottom.
      fn terminal(&self, ui: &mut egui::Ui, ctx: &egui::Context, busy: bool) {
         egui::Frame::new()
            .fill(TERMINAL)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
               ui.set_width(ui.available_width());
               ui.set_height(TERMINAL_HEIGHT);
               egui::ScrollArea::vertical()
                  .stick_to_bottom(true)
                  .auto_shrink([false, false])
                  .show(ui, |ui| {
                     ui.spacing_mut().item_spacing.y = 3.0;
                     for line in self
                        .lines
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .iter()
                     {
                        let (prefix, color) = match line.kind {
                           Kind::Cmd => ("$ ", RUBY_HOVER),
                           Kind::Info => ("  ", OUTPUT),
                           Kind::Ok => ("[ok] ", OK),
                           Kind::Warn => ("[!!] ", WARN),
                           Kind::Error => ("[xx] ", ERROR),
                        };
                        let text_color = match line.kind {
                           Kind::Cmd => TEXT,
                           Kind::Info => OUTPUT,
                           _ => color,
                        };
                        let font = FontId::monospace(12.0);
                        let mut job = egui::text::LayoutJob::default();
                        job.wrap.max_width = ui.available_width();
                        job.wrap.break_anywhere = true;
                        for (text, color) in [(prefix, color), (line.text.as_str(), text_color)] {
                           job.append(
                              text,
                              0.0,
                              egui::text::TextFormat::simple(font.clone(), color),
                           );
                        }
                        ui.label(job);
                     }
                     if busy {
                        // A blinking cursor while a job runs.
                        let on = (ui.input(|input| input.time) * 2.0) as i64 % 2 == 0;
                        ui.label(
                           RichText::new(if on { "  _" } else { "   " })
                              .font(FontId::monospace(12.0))
                              .color(OUTPUT),
                        );
                        ctx.request_repaint_after(Duration::from_millis(250));
                     }
                  });
            });
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

   /// The window's own title bar, since the system's is off: the logo and name, drag to move, and
   /// minimize and close.
   fn title_bar(ui: &mut egui::Ui, ctx: &egui::Context, logo: &egui::TextureHandle) {
      let (bar, _) = ui.allocate_exact_size(
         vec2(ui.available_width(), TITLE_BAR_HEIGHT),
         egui::Sense::hover(),
      );
      let drag = ui.interact(
         bar,
         ui.scope_id().with("title-drag"),
         egui::Sense::click_and_drag(),
      );
      if drag.drag_started_by(PointerButton::Primary) {
         ctx.send_viewport_cmd(ViewportCommand::StartDrag);
      }
      let painter = ui.painter();
      let icon =
         egui::Rect::from_center_size(pos2(bar.left() + 20.0, bar.center().y), vec2(16.0, 16.0));
      painter.image(
         logo.id(),
         icon,
         egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
         Color32::WHITE,
      );
      painter.text(
         pos2(bar.left() + 36.0, bar.center().y),
         egui::Align2::LEFT_CENTER,
         "RubyCord Installer",
         FontId::proportional(12.5),
         MUTED,
      );

      // The minimize button, then the close button at the very edge.
      for (close, from_right) in [(false, 2.0), (true, 1.0)] {
         let rect = egui::Rect::from_min_size(
            pos2(bar.right() - 46.0 * from_right, bar.top()),
            vec2(46.0, TITLE_BAR_HEIGHT),
         );
         let id = ui.scope_id().with(if close {
            "title-close"
         } else {
            "title-minimize"
         });
         let response = ui.interact(rect, id, egui::Sense::click());
         let painter = ui.painter();
         if response.hovered() {
            painter.rect_filled(
               rect,
               CornerRadius::ZERO,
               if close { CLOSE_HOVER } else { CARD },
            );
         }
         let (c, ink) = (rect.center(), Stroke::new(1.2, TEXT));
         if close {
            painter.line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], ink);
            painter.line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, -5.0)], ink);
         } else {
            painter.line_segment([c + vec2(-5.0, 0.0), c + vec2(5.0, 0.0)], ink);
         }
         if response.clicked() {
            ctx.send_viewport_cmd(if close {
               ViewportCommand::Close
            } else {
               ViewportCommand::Minimized(true)
            });
         }
      }
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
      let (rect, response) = ui.allocate_exact_size(vec2(width, 50.0), sense);
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
