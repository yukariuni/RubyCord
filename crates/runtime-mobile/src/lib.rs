//! phoenix on Phoenix's mobile clients: the UI-op bridge, the settings pages
//! and the UI patches. Android adds JNI for HTTP, SQL and the installer, iOS
//! uses Foundation and a bundled SQLite.

#![cfg(any(target_os = "android", target_os = "ios"))]

mod bridge;
mod debug;
mod discord;
#[cfg(target_os = "android")] mod fetch;
#[cfg(target_os = "ios")] mod foundation;
mod host;
#[cfg(target_os = "android")] mod install;
mod js;
#[cfg(target_os = "android")] mod jvm;
#[cfg(target_os = "android")] mod loader;
mod patches;
mod picker;
mod settings;
mod socket;
#[cfg(target_os = "android")] mod staff;
#[cfg(target_os = "android")] mod store;

pub(crate) use phoenix_runtime::highlight;

use std::{
   ffi::c_void,
   path::{
      Path,
      PathBuf,
   },
};

use phoenix_hook::image::Image;
#[cfg(target_os = "android")]
use phoenix_runtime::update::Update;
use phoenix_runtime::{
   Error,
   Notice,
   Platform,
   Plugin,
   Started,
   discord::{
      Authorization,
      DiscordError,
   },
   fetch::{
      FetchError,
      Request,
      Response,
   },
   remote,
   store::{
      Rows,
      StoreError,
   },
};
use rquickjs::{
   Ctx,
   Object,
};

use crate::patches::banners::{
   self,
   BannerAction,
};

struct Mobile;

/// Starts phoenix inside Phoenix's mobile client.
pub fn start(home: PathBuf, bundled: Vec<Plugin>) -> Result<Started, Error> {
   phoenix_runtime::start(&Mobile, home, bundled)
}

/// Ruby's green, for update banners.
const ACCENT: i64 = 0xFF23_A55A;

impl Platform for Mobile {
   fn send(&self, request: &Request) -> Result<Response, FetchError> {
      #[cfg(target_os = "android")]
      return fetch::send(request);
      #[cfg(target_os = "ios")]
      return foundation::send(request);
   }

   fn query(&self, database: &Path, sql: &str, args: &[String]) -> Result<Rows, StoreError> {
      #[cfg(target_os = "android")]
      return store::query(database, sql, args);
      #[cfg(target_os = "ios")]
      return phoenix_runtime::store::sqlite(database, sql, args);
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
      ctx.store_userdata(js::Mobile::default())
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
      js::messages::patch_api(ctx, plugin, api, messages)?;
      api.set("bridge", js::bridge::bridge_object(ctx)?)
   }

   fn unload(&self, ctx: &Ctx<'_>, plugin: &str) {
      js::unload(ctx, plugin);
   }

   #[cfg(target_os = "android")]
   fn staff(&self, image: &'static Image<'static>) {
      staff::start(image);
   }

   #[cfg(target_os = "ios")]
   fn staff(&self, _image: &'static Image<'static>) {}

   #[cfg(target_os = "android")]
   fn gateway(&self, store: *mut c_void) {
      bridge::jni::attach();
      // SAFETY: every registry handler takes the store state first, and this
      // runs inside one.
      unsafe { staff::apply(store) };
   }

   #[cfg(target_os = "ios")]
   fn gateway(&self, _store: *mut c_void) {}

   fn rewritten(&self, original: String, shown: String) {
      patches::remember_original(shown, original);
   }

   #[cfg(target_os = "android")]
   fn before_plugins(&self, home: &Path) {
      loader::adopt_staged(home, &home.join("plugins"));
   }

   #[cfg(target_os = "ios")]
   fn before_plugins(&self, _home: &Path) {
      if let Some(state) = phoenix_runtime::state() {
         bridge::ios::start(&state.image);
      }
   }

   fn started(&self, _first_run: bool, _plugins: &[String]) {
      // The shell is a saved setting here, so it comes back on at launch.
      phoenix_runtime::remote::ensure_started();
      #[cfg(target_os = "android")]
      if let Some(state) = phoenix_runtime::state() {
         loader::forget_update(&state.files_dir());
      }
      std::thread::spawn(|| {
         if let Err(err) = host::load() {
            log::error!("host names: {err}");
         }
      });
      settings::start();
      patches::start();
      bridge::install(Box::new(socket::Socket));
   }

   fn refresh(&self) {
      settings::refresh();
   }

   fn logged(&self, target: &str) {
      // A warning from refreshing the page would refresh it again, forever.
      if !target.starts_with(concat!(module_path!(), "::settings")) {
         settings::log_changed();
      }
   }

   fn toast(&self, message: &str) {
      let _ = picker::toast(message);
   }

   fn notify(&self, notice: Notice<'_>) {
      match notice {
         Notice::UpdateAvailable(update) => {
            let body = update.loader.as_ref().map_or_else(
               || {
                  format!(
                     "Updated plugins: {}, tap to update now",
                     update.plugins.join(", ")
                  )
               },
               |version| format!("Build {version}, tap to update in-app"),
            );
            banners::show_banner(
               "Ruby update available".to_owned(),
               Some(body),
               Some("res://ic_mana_puzzle_piece_retry".to_owned()),
               Some(ACCENT),
               8000,
               BannerAction::ApplyUpdate,
            );
         },
         Notice::PluginsUpdated(names) => {
            banners::show_banner(
               "Plugins updated".to_owned(),
               Some(format!("Reloaded: {}", names.join(", "))),
               Some("res://ic_mana_checkmark_large".to_owned()),
               Some(ACCENT),
               4500,
               BannerAction::None,
            );
         },
      }
   }

   #[cfg(target_os = "android")]
   fn updates_loader(&self) -> bool {
      true
   }

   #[cfg(target_os = "android")]
   fn update_loader(&self, version: &str, update: &Update) {
      loader::install(version, update);
   }

   fn route(&self, request: &remote::Request) -> Option<remote::Reply> {
      debug::route(request)
   }

   fn routes(&self) -> &'static str {
      debug::ROUTES
   }
}
