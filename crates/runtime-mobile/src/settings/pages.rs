//! The main page, with its update and debug sections, and the install page.

use phoenix_runtime::{
   PluginInfo,
   update::Catalog,
};
use phoenix_ui_mobile::Prop;
use phoenix_wire::Value;

use crate::settings::{
   COLOR_ACCENT,
   COLOR_ERROR,
   COLOR_STRING,
   COLOR_WARN,
   Field,
   ICON_CONSOLE,
   ICON_FILE,
   ICON_PLUGIN,
   ICON_RELOAD,
   Press,
   console::log_line,
   rows::{
      InputKind,
      button_row,
      section,
      switch_row,
      text_field,
      value_row,
   },
   tree::Tree,
};

pub fn main_sections(plugins: &[PluginInfo]) -> Vec<Tree> {
   let rows = if plugins.is_empty() {
      vec![button_row("No plugins found", ICON_PLUGIN, None, false)]
   } else {
      plugins
         .iter()
         .enumerate()
         .map(|(index, plugin)| {
            let subtitle = plugin
               .error
               .as_deref()
               .map(|error| format!("Failed: {error}"))
               .or_else(|| plugin.header.description.clone());
            switch_row(
               Some(ICON_PLUGIN),
               &plugin.name,
               subtitle.as_deref(),
               plugin.enabled,
               Press::OpenPlugin(index),
               Press::Toggle(index),
            )
         })
         .collect()
   };
   let enabled = plugins.iter().filter(|plugin| plugin.enabled).count();
   update_section()
      .into_iter()
      .chain([
         section("Plugins", rows),
         section("", vec![
            button_row("Install plugin", ICON_FILE, Some(Press::OpenInstall), true),
            button_row("Console", ICON_CONSOLE, Some(Press::OpenConsole), true),
         ]),
         debug_section(),
         section("About", vec![
            value_row("Version", phoenix_runtime::update::VERSION),
            value_row("Runtime", "rquickjs"),
            value_row("Gateway hooks", "registry pointer swap"),
            // iOS hooks nothing inline: it swaps the bridge's vtable slots.
            value_row(
               "Hook library",
               if cfg!(target_os = "ios") { "vtable slot swap" } else { "ShadowHook" },
            ),
            value_row("Enabled", &format!("{enabled} of {}", plugins.len())),
         ]),
      ])
      .collect()
}

fn update_section() -> Option<Tree> {
   let update_found = phoenix_runtime::update::found();
   let update_status = phoenix_runtime::update::status();
   let update_busy = phoenix_runtime::update::busy();
   (update_found.is_some() || update_status.is_some()).then(|| {
      let mut update_rows = Vec::new();
      if let Some(update) = &update_found {
         let what = if update_busy {
            update_status
               .clone()
               .unwrap_or_else(|| "Updating...".to_owned())
         } else {
            match (&update.loader, update.plugins.len()) {
               (Some(version), 0) => format!("Install update ({version})"),
               (Some(version), n) => {
                  format!(
                     "Install update ({version}) + {n} plugin{}",
                     if n == 1 { "" } else { "s" }
                  )
               },
               (None, _) => format!("Update plugins now: {}", update.plugins.join(", ")),
            }
         };
         let mut row = button_row(
            &what,
            ICON_RELOAD,
            (!update_busy).then_some(Press::ApplyUpdate),
            !update_busy,
         );
         if let Some(label) = row.kids.first_mut().and_then(|row| row.kids.get_mut(1)) {
            let color = if update_busy {
               COLOR_ACCENT
            } else {
               COLOR_STRING
            };
            label.set(Prop::COLOR, Some(Value::I64(color)));
         }
         update_rows.push(row);
      }
      if let Some(status) = &update_status
         && (!update_busy || update_found.is_none())
      {
         let mut line = log_line(&[(status.clone(), COLOR_WARN)]);
         line.set(Prop::FONT_FAMILY, None);
         line.set(Prop::FONT_SIZE, Some(Value::F64(13.0)));
         line.set(Prop::LINE_HEIGHT, Some(Value::F64(18.0)));
         update_rows.push(line);
      }
      // Plugins update in place, so the site's full download is only for a
      // loader.
      if update_found
         .as_ref()
         .is_some_and(|update| update.loader.is_some())
      {
         update_rows.push(button_row(
            "Open download page",
            ICON_FILE,
            Some(Press::OpenSite),
            true,
         ));
      }
      section("Update available", update_rows)
   })
}

/// The site's plugins, each with a switch that installs or uninstalls it.
fn listed_rows(plugins: &[PluginInfo]) -> Vec<Tree> {
   let listed = match phoenix_runtime::update::catalog() {
      Catalog::Unfetched | Catalog::Loading => {
         return vec![button_row(
            "Loading the plugin list...",
            ICON_PLUGIN,
            None,
            false,
         )];
      },
      Catalog::Failed(err) => {
         let mut line = log_line(&[(format!("The plugin list did not load: {err}"), COLOR_ERROR)]);
         line.set(Prop::FONT_FAMILY, None);
         return vec![line];
      },
      Catalog::Loaded(listed) => listed,
   };
   listed
      .iter()
      .enumerate()
      .map(|(index, plugin)| {
         let on = plugins
            .iter()
            .any(|other| other.name == plugin.name && (other.installed || other.enabled));
         let subtitle = plugin.warning.as_ref().map_or_else(
            || plugin.description.clone(),
            |warning| format!("{} \u{26A0} {warning}", plugin.description),
         );
         switch_row(
            Some(ICON_PLUGIN),
            &plugin.name,
            Some(&subtitle),
            on,
            Press::Listed(index),
            Press::Listed(index),
         )
      })
      .collect()
}

fn debug_section() -> Tree {
   let debug_rows = vec![
      switch_row(
         None,
         "Staff unlock",
         Some("Grant the staff flag to unlock Discord experiments"),
         phoenix_runtime::staff_enabled(),
         Press::ToggleStaff,
         Press::ToggleStaff,
      ),
      switch_row(
         None,
         "Remote debug shell",
         Some(
            "Listen on 127.0.0.1:4242 for ADB eval, logs & plugin hot-reload (adb forward \
             tcp:4242 tcp:4242)",
         ),
         phoenix_runtime::remote_shell_enabled(),
         Press::ToggleRemoteShell,
         Press::ToggleRemoteShell,
      ),
   ];
   section("Debug", debug_rows)
}

pub fn install_sections(status: Option<&(String, i64)>, plugins: &[PluginInfo]) -> Vec<Tree> {
   let mut sections = vec![
      section("From Ruby", listed_rows(plugins)),
      section("From a URL", vec![text_field(
         Field::Install,
         Some(ICON_PLUGIN),
         "https://example.com/plugin.js",
         "",
         InputKind::Text,
      )]),
      section("From a file", vec![button_row(
         "Choose a .js file",
         ICON_FILE,
         Some(Press::PickFile),
         false,
      )]),
   ];
   if let Some((message, color)) = status {
      let mut line = log_line(&[(message.clone(), *color)]);
      line.set(Prop::FONT_FAMILY, None);
      line.set(Prop::FONT_SIZE, Some(Value::F64(14.0)));
      line.set(Prop::LINE_HEIGHT, Some(Value::F64(18.0)));
      sections.push(section("", vec![line]));
   }
   sections
}
