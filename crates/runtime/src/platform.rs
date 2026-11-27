//! What a platform crate gives the core: its HTTP stack, its view of
//! Phoenix's cache, its share of the `phoenix` API and its way of telling the
//! user things. `phoenix-runtime-mobile` and `phoenix-runtime-desktop`
//! implement it and pass it to [`crate::start`].

use std::{
   ffi::c_void,
   path::Path,
};

use phoenix_hook::image::Image;
use rquickjs::{
   Ctx,
   Object,
};

use crate::{
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
   update::Update,
};

/// Something the user should hear about outside the settings page.
pub enum Notice<'a> {
   /// The site has a newer loader or newer plugins.
   UpdateAvailable(&'a Update),
   /// Plugins updated in place.
   PluginsUpdated(&'a [String]),
}

pub trait Platform: Sync {
   /// Sends `request` through the platform's own HTTP stack.
   fn send(&self, request: &Request) -> Result<Response, FetchError>;

   /// Runs `sql` with `args` bound to its `?`s over the cache at `database`.
   fn query(&self, database: &Path, sql: &str, args: &[String]) -> Result<Rows, StoreError>;

   /// Sends `method path` with Phoenix's session headers and waits for the
   /// answer.
   fn discord(
      &self,
      method: &str,
      path: &str,
      headers: Vec<(String, String)>,
      body: Vec<u8>,
   ) -> Result<Response, DiscordError>;

   /// Opens Phoenix's `OAuth2` authorize flow and returns the redirect URL.
   fn authorize(&self, request: &Authorization) -> Result<String, DiscordError>;

   /// Keeps the platform's own JS state in `ctx`'s userdata.
   fn init(&self, ctx: &Ctx<'_>) -> rquickjs::Result<()>;

   /// Adds the platform's namespaces to a plugin's `phoenix` object.
   fn api<'js>(
      &self,
      ctx: &Ctx<'js>,
      plugin: &str,
      api: &Object<'js>,
      messages: &Object<'js>,
   ) -> rquickjs::Result<()>;

   /// Adds what only the console prompt's `phoenix` object gets.
   fn console<'js>(&self, _ctx: &Ctx<'js>, _phoenix: &Object<'js>) -> rquickjs::Result<()> {
      Ok(())
   }

   /// Drops what `plugin` registered with the platform.
   fn unload(&self, ctx: &Ctx<'_>, plugin: &str);

   /// Starts the staff unlock against `image`.
   fn staff(&self, image: &'static Image<'static>);

   /// Runs first in every gateway handler call, with the store it received.
   fn gateway(&self, store: *mut c_void);

   /// A plugin hook changed the text of a message from `original` to `shown`.
   fn rewritten(&self, _original: String, _shown: String) {}

   /// Runs before the plugins load, with phoenix's home directory.
   fn before_plugins(&self, _home: &Path) {}

   /// Runs once the plugins loaded. `first_run` is true when the user had no
   /// saved choices yet.
   fn started(&self, first_run: bool, plugins: &[String]);

   /// Redraws the settings pages, after what they show changed.
   fn refresh(&self);

   /// A line was logged by the module `target`.
   fn logged(&self, _target: &str) {}

   fn toast(&self, message: &str);

   fn notify(&self, notice: Notice<'_>);

   /// Whether the site's loader updates this install, so site plugins wait
   /// for it.
   fn updates_loader(&self) -> bool {
      false
   }

   /// Installs the site's loader `version`, with `update`'s plugins.
   fn update_loader(&self, _version: &str, _update: &Update) {}

   /// Answers a debug shell request the core does not know.
   fn route(&self, _request: &remote::Request) -> Option<remote::Reply> {
      None
   }

   /// The endpoints [`Self::route`] answers, for the debug shell's help.
   fn routes(&self) -> &'static str {
      ""
   }
}
