//! A plugin's page: its switch, settings, actions and info.

use phoenix_runtime::{
   Control,
   PluginInfo,
};
use phoenix_ui_mobile::Prop;
use phoenix_wire::Value;

use crate::settings::{
   COLOR_ERROR,
   Field,
   ICON_CONSOLE,
   ICON_PLUGIN,
   ICON_RELOAD,
   ICON_UNINSTALL,
   Press,
   rows::{
      InputKind,
      button_row,
      section,
      select_row,
      switch_row,
      text_field,
      value_row,
   },
   tree::Tree,
};

pub fn plugin_sections(index: usize, plugin: &PluginInfo) -> Vec<Tree> {
   let subtitle = plugin
      .error
      .as_deref()
      .map(|error| format!("Failed: {error}"));
   let status = match (&plugin.error, plugin.enabled) {
      (Some(_), _) => "failed",
      (None, true) => "running",
      (None, false) => "off",
   };
   let (rows, fields) = setting_controls(index, plugin);
   let mut sections = vec![section("", vec![switch_row(
      Some(ICON_PLUGIN),
      "Enabled",
      subtitle.as_deref(),
      plugin.enabled,
      Press::Toggle(index),
      Press::Toggle(index),
   )])];
   if !rows.is_empty() {
      sections.push(section("Settings", rows));
   }
   sections.extend(fields);
   let mut actions = vec![
      button_row(
         "Edit code",
         ICON_CONSOLE,
         Some(Press::EditCode(index)),
         true,
      ),
      button_row("Reload", ICON_RELOAD, Some(Press::Reload(index)), false),
   ];
   if plugin.installed {
      let mut uninstall = button_row(
         "Uninstall",
         ICON_UNINSTALL,
         Some(Press::Uninstall(index)),
         false,
      );
      // `root > row > [icon, label]`, both in the danger color.
      if let Some(row) = uninstall.kids.first_mut() {
         if let Some(icon) = row.kids.get_mut(0) {
            icon.set(Prop::TINT, Some(Value::I64(COLOR_ERROR)));
         }
         if let Some(label) = row.kids.get_mut(1) {
            label.set(Prop::COLOR, Some(Value::I64(COLOR_ERROR)));
         }
      }
      actions.push(uninstall);
   }
   sections.push(section("", actions));
   let mut info = vec![value_row("Name", &plugin.name)];
   for (label, value) in [
      ("Version", &plugin.header.version),
      ("Author", &plugin.header.author),
   ] {
      if let Some(value) = value {
         info.push(value_row(label, value));
      }
   }
   info.push(value_row("Status", status));
   sections.push(section("Info", info));
   sections
}

fn setting_controls(index: usize, plugin: &PluginInfo) -> (Vec<Tree>, Vec<Tree>) {
   let mut rows = Vec::new();
   let mut fields = Vec::new();
   for (at, setting) in plugin.settings.iter().enumerate() {
      let press = Press::Setting(index, at);
      let description = setting.description.as_deref();
      let field = |value: &str, entry| {
         let input = text_field(
            Field::Setting(index, at),
            None,
            description.unwrap_or(""),
            value,
            entry,
         );
         section(&setting.title, vec![input])
      };
      match &setting.control {
         Control::Switch(on) => {
            rows.push(switch_row(
               None,
               &setting.title,
               description,
               *on,
               press,
               press,
            ));
         },
         Control::Button => {
            let mut row = button_row(&setting.title, ICON_PLUGIN, Some(press), false);
            if let Some(inner) = row.kids.first_mut() {
               inner.kids.remove(0);
            }
            rows.push(row);
         },
         Control::Text(current) => fields.push(field(current, InputKind::Text)),
         Control::Secret(current) => fields.push(field(current, InputKind::Secret)),
         Control::Number(number) => fields.push(field(&number.to_string(), InputKind::Number)),
         Control::Select { options, chosen } => {
            let rows = (0..)
               .zip(options)
               .map(|(option, choice)| {
                  select_row(
                     &choice.label,
                     choice.value == *chosen,
                     Press::Choose(index, at, option),
                  )
               })
               .collect();
            fields.push(section(&setting.title, rows));
         },
      }
   }
   (rows, fields)
}
