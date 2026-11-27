//! phoenix on Desktop Phoenix: the access gate, the HTTP transport hooks, the
//! Ruby pages in User Settings, message accessories, member list
//! decorations and the tray.

#![cfg(any(
   windows,
   target_os = "macos",
   all(target_os = "linux", target_arch = "x86_64")
))]

mod accessories;
mod badges;
mod composer;
mod context_menu;
#[cfg(target_arch = "x86_64")]
mod counts;
mod discord;
mod double_click;
mod emoji;
mod fetch;
mod gateway_send;
mod gpui;
mod headers;
mod icons;
mod js;
mod members;
mod message;
mod modal;
mod os;
#[cfg(windows)]
mod restart;
mod settings;
mod staff;

use std::{
   ffi::c_void,
   path::{Path, PathBuf},
   sync::{Arc, Mutex, PoisonError},
   time::Duration,
};

use phoenix_hook::{image::Image, x86::Abi};
use phoenix_runtime::{
   Error, Notice, Platform, Plugin, Started,
   discord::{Authorization, DiscordError},
   fetch::{FetchError, Request, Response},
   remote::{self, Done},
   store::{Rows, StoreError},
};
use phoenix_sigs::{
   desktop::{Prologue, Prologues},
   registry::GatewayRegistry,
};
use rquickjs::{Ctx, Object};

struct Desktop;

/// A `POST /ui/choose` or `POST /ui/sheet` body.
#[derive(serde::Deserialize)]
struct DialogRequest {
   title: Option<String>,
   message: Option<String>,
   text: Option<String>,
   monospace: Option<bool>,
   highlight: Option<bool>,
   options: Option<Vec<DialogItem>>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum DialogItem {
   Label(String),
   Item {
      label: String,
      #[serde(default)]
      danger: bool,
   },
}

#[derive(serde::Serialize)]
struct DialogAnswer {
   index: i32,
}

/// Whether Discord's code is x86-64, which every locator in
/// `phoenix_sigs::desktop` and `http` decodes. macOS Phoenix is arm64 only, so
/// there they are never run, an x86 patch can never land in arm64 code, and
/// what they locate stays off.
pub(crate) const X86_CODE: bool = cfg!(all(target_arch = "x86_64", not(target_os = "macos")));

/// Hooks `target` with `detour` past the prologue `pick` chooses, and returns
/// the trampoline as `F`, or logs the error after `what`.
///
/// # Safety
///
/// Runs before Discord's threads reach `target`, `detour` has its ABI, and `F`
/// is its function pointer type.
pub(crate) unsafe fn hook<F: Copy>(
   image: &Image<'static>,
   pick: fn(Prologues) -> Prologue,
   target: usize,
   detour: usize,
   what: &str,
) -> Option<F> {
   const { assert!(size_of::<F>() == size_of::<usize>()) };
   let prologue = pick(Prologues::of(Abi::of(image)));
   // SAFETY: forwarded from the caller.
   match unsafe { image.hook_entry(target, prologue.pattern, prologue.len, detour) } {
      // SAFETY: the trampoline runs `target`'s displaced instructions, so it
      // has `F`'s ABI.
      Ok(trampoline) => Some(unsafe { std::mem::transmute_copy::<usize, F>(&trampoline) }),
      Err(err) => {
         log::warn!("{what}: {err}");
         None
      },
   }
}

/// The target and its swapped gateway registry, once [`prepare`] hooked them.
static PREPARED: Mutex<Option<(Image<'static>, &'static GatewayRegistry)>> = Mutex::new(None);

/// Hooks Discord from `DllMain` on Windows or the constructor on Linux and
/// macOS, before its own code runs.
///
/// Desktop Phoenix settles on the access gate in its first frames and stops
/// reading the gateway registry and HTTP vtables once started, so hooks swapped
/// later from [`start`]'s thread never run. [`start`] picks up what this
/// hooked.
pub fn prepare() -> Result<(), Error> {
   phoenix_runtime::set_platform(&Desktop);
   let image = phoenix_runtime::target()?;
   open_access_gate(&image);
   settings::install(&image);
   #[cfg(windows)]
   restart::install(&image);
   staff::prepare(&image);
   if X86_CODE {
      modal::install(&image);
   }
   context_menu::install(&image);
   // Before accessories hooks `MessageRow::render`, whose prologue the header
   // locators check.
   let header = X86_CODE.then(|| headers::locate(&image));
   accessories::install(&image);
   if let Some(header) = header {
      members::install(&image);
      headers::install(&image, header);
      icons::install(&image);
      message::install(&image);
      composer::install(&image);
      #[cfg(target_arch = "x86_64")]
      counts::install(&image);
   }
   badges::install(&image);
   emoji::install(&image);
   #[cfg(target_os = "macos")]
   {
      message::writer::install();
      log::info!("arm64 Desktop: plugin pages and dialogs are AppKit windows");
   }
   <os::Os as os::DesktopOs>::install(&image);
   let registry = phoenix_runtime::hook(&image);
   if registry.is_ok() {
      discord::install(&image);
      gateway_send::install(&image);
      accessories::install_optimistic_delete(&image);
   }
   // The index is tens of MB and nothing reads it after startup.
   phoenix_sigs::desktop::release_call_index();
   *PREPARED.lock().unwrap_or_else(PoisonError::into_inner) = Some((image, registry?));
   Ok(())
}

/// Opens the access gate for testing Discord without the plugin runtime.
pub fn open_gate_only() -> Result<(), Error> {
   phoenix_runtime::set_platform(&Desktop);
   let image = phoenix_runtime::target()?;
   open_access_gate(&image);
   #[cfg(windows)]
   restart::install(&image);
   Ok(())
}

/// Starts phoenix inside Desktop Phoenix, on what [`prepare`] hooked.
pub fn start(home: PathBuf, bundled: Vec<Plugin>) -> Result<Started, Error> {
   let prepared = PREPARED
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .take();
   if let Some((image, registry)) = prepared {
      return phoenix_runtime::start_hooked(&Desktop, image, registry, home, bundled);
   }
   let image = phoenix_runtime::target()?;
   let registry = phoenix_runtime::hook(&image)?;
   discord::install(&image);
   gateway_send::install(&image);
   phoenix_runtime::start_hooked(&Desktop, image, registry, home, bundled)
}

/// Desktop Phoenix shows "This app isn't available for your account" from
/// `libdiscord_ops_surfaces::client_access::ClientAccessGate`, and blocks the
/// login on it. One function reads it (1 blocked, 2 no gate, also what a
/// missing value reads as), and the gate is only considered while it is not 2:
/// its small caller is `signed_in && access(..) != 2`. This makes it return 2.
/// Discord settles on the gate within its first frames, so the loader calls
/// this from `DllMain` or the constructor, before Discord's own code.
fn open_access_gate(image: &Image<'static>) {
   /// `mov al, 2; ret`
   const ALLOWED: [u8; 3] = [0xB0, 0x02, 0xC3];

   let found = if X86_CODE {
      phoenix_sigs::desktop::access_gate(image).map(|access| (access, &ALLOWED[..]))
   } else {
      close_enforced(image);
      phoenix_sigs::macos::access_gate(image)
         .map(|access| (access, &phoenix_sigs::macos::NO_GATE[..]))
   };
   let (access, allowed) = match found {
      Ok(found) => found,
      Err(err) => {
         log::warn!("access gate: {err}, leaving it");
         return;
      },
   };
   // SAFETY: Discord's own code, written from `DllMain` or the constructor
   // before it runs.
   match unsafe { image.write_code(access, allowed) } {
      Ok(()) => log::info!("access gate: open"),
      Err(err) => log::error!("access gate: {err}"),
   }
}

/// Makes macOS's `client_access::enforced` answer false. `push_access` asks it
/// without the access function and drops every message alert for a gated
/// account, notifications and their sounds included.
fn close_enforced(image: &Image<'static>) {
   let enforced = phoenix_sigs::macos::access_read(image)
      .and_then(|read| phoenix_sigs::macos::access_enforced(image, read));
   // SAFETY: as for the access function in `open_access_gate`.
   match enforced.map(|at| unsafe { image.write_code(at, &phoenix_sigs::macos::NOT_ENFORCED) }) {
      Ok(Ok(())) => log::info!("access gate: not enforced, message alerts are on"),
      Ok(Err(err)) => log::error!("access gate: {err}"),
      Err(err) => log::warn!("access gate: {err}, message alerts stay off"),
   }
}

impl Platform for Desktop {
   fn send(&self, request: &Request) -> Result<Response, FetchError> {
      fetch::send(request)
   }

   fn query(&self, database: &Path, sql: &str, args: &[String]) -> Result<Rows, StoreError> {
      phoenix_runtime::store::sqlite(database, sql, args)
   }

   fn discord(
      &self,
      method: &str,
      path: &str,
      headers: Vec<(String, String)>,
      body: Vec<u8>,
   ) -> Result<Response, DiscordError> {
      discord::send(method, path, headers, body)
   }

   fn authorize(&self, request: &Authorization) -> Result<String, DiscordError> {
      discord::authorize(request)
   }

   fn init(&self, ctx: &Ctx<'_>) -> rquickjs::Result<()> {
      ctx.store_userdata(js::Desktop::default())
         .map_err(|err| rquickjs::Exception::throw_internal(ctx, &err.to_string()))?;
      Ok(())
   }

   fn api<'js>(
      &self,
      ctx: &Ctx<'js>,
      plugin: &str,
      api: &Object<'js>,
      messages: &Object<'js>,
   ) -> rquickjs::Result<()> {
      js::ui::desktop_patch_api(ctx, plugin, api, messages)
   }

   fn console<'js>(&self, ctx: &Ctx<'js>, phoenix: &Object<'js>) -> rquickjs::Result<()> {
      phoenix.set("native", js::native::object(ctx)?)
   }

   fn unload(&self, ctx: &Ctx<'_>, plugin: &str) {
      js::unload(ctx, plugin);
   }

   fn staff(&self, image: &'static Image<'static>) {
      staff::start(image);
   }

   fn gateway(&self, store: *mut c_void) {
      // SAFETY: every registry handler takes the store state first, and this
      // runs inside one.
      unsafe { staff::apply(store) };
   }

   fn started(&self, _first_run: bool, _plugins: &[String]) {
      let Some(state) = phoenix_runtime::state() else {
         return;
      };
      let _ = std::fs::create_dir_all(state.plugins_dir());
      // Any local process could reach the shell's `/eval` and the session, so
      // it only listens once DevTools is opened, and only for holders of this
      // start's token. `RUBY_DEVTOOLS` starts it at launch, for `dev.ps1`.
      remote::require_token(&state.plugins_dir().with_file_name("debug-shell-token"));
      if std::env::var_os("RUBY_DEVTOOLS").is_some() {
         start_shell();
      }
      phoenix_ui_desktop::start_tray(phoenix_ui_desktop::TrayConfig {
         devtools_url: Arc::new(start_shell),
         plugins_dir: state.plugins_dir(),
         on_reload_all: Some(Arc::new(phoenix_runtime::reload_all)),
         on_check_updates: Some(Arc::new(phoenix_runtime::update::check_now)),
      });
      phoenix_ui_desktop::notify(
         "Ruby ready",
         Some(if X86_CODE {
            "Open User Settings -> Ruby, or press Ctrl+Shift+I for DevTools"
         } else {
            "Ruby's settings are in its menu bar item"
         }),
      );
      phoenix_runtime::update::check();
   }

   fn refresh(&self) {
      modal::force_redraw();
   }

   fn toast(&self, message: &str) {
      phoenix_ui_desktop::toast(message);
   }

   fn notify(&self, notice: Notice<'_>) {
      match notice {
         Notice::UpdateAvailable(update) => {
            let mut parts = Vec::new();
            if let Some(version) = &update.loader {
               parts.push(format!("Ruby {version} (restart to update)"));
            }
            if !update.plugins.is_empty() {
               parts.push(format!("plugins: {}", update.plugins.join(", ")));
            }
            let body = if parts.is_empty() {
               "Open Settings -> Ruby".to_owned()
            } else {
               parts.join(" · ")
            };
            phoenix_ui_desktop::notify("Ruby update available", Some(&body));
         },
         Notice::PluginsUpdated(names) if !names.is_empty() => {
            let body = format!("Reloaded: {}", names.join(", "));
            phoenix_ui_desktop::notify("Plugins updated", Some(&body));
         },
         Notice::PluginsUpdated(_) => {},
      }
   }

   fn route(&self, request: &remote::Request) -> Option<remote::Reply> {
      let html = || {
         // Enabled themes go last in <head>, so their rules win over the page's own.
         let page = phoenix_ui_desktop::DASHBOARD_HTML.replacen(
            "</head>",
            &format!(
               "<style id=\"ruby-theme\">{}</style></head>",
               phoenix_runtime::theme::css()
            ),
            1,
         );
         ("200 OK", "text/html; charset=utf-8", page.into_bytes())
      };
      match (request.method.as_str(), request.path.as_str()) {
         ("GET", "/ui") => Some(html()),
         ("GET", "/") if request.accept.contains("text/html") => Some(html()),
         ("POST", "/ui/devtools") => {
            phoenix_ui_desktop::open_devtools();
            Some(remote::ok_json(&Done::Started))
         },
         ("POST", "/ui/choose") => Some(dialog(&request.body, false)),
         ("POST", "/ui/sheet") => Some(dialog(&request.body, true)),
         _ => None,
      }
   }

   fn routes(&self) -> &'static str {
      ", GET /ui, POST /ui/devtools, POST /ui/{choose,sheet}"
   }
}

/// Opens a native choose or sheet dialog for the dashboard and blocks this
/// request's thread until it is answered.
/// Starts the debug shell if it isn't running and returns the `DevTools` URL.
fn start_shell() -> String {
   remote::set_flag(true);
   remote::ensure_started();
   remote::dashboard_url()
}

fn dialog(body: &[u8], sheet: bool) -> remote::Reply {
   let Ok(request) = serde_json::from_slice::<DialogRequest>(body) else {
      return remote::err("400 Bad Request", "expected a JSON dialog request");
   };
   let options = request
      .options
      .unwrap_or_default()
      .into_iter()
      .map(|item| match item {
         DialogItem::Label(label) => modal::ChoiceOption {
            label,
            danger: false,
         },
         DialogItem::Item { label, danger } => modal::ChoiceOption { label, danger },
      })
      .collect::<Vec<_>>();
   let content = if sheet {
      request.text.map(|text| modal::ModalSection::Code {
         title: None,
         text,
         monospace: request.monospace.unwrap_or_default(),
         highlight: request.highlight.unwrap_or_default(),
      })
   } else {
      request
         .message
         .map(|text| modal::ModalSection::Text { title: None, text })
   };
   if content.is_none() && options.is_empty() {
      return remote::err(
         "400 Bad Request",
         if sheet {
            "a sheet needs text"
         } else {
            "a choose dialog needs a message or options"
         },
      );
   }
   let title = request.title.unwrap_or_else(|| "Ruby".to_owned());
   let index = modal::ask(title, content, options, Duration::from_secs(120));
   remote::ok_json(&DialogAnswer { index })
}
