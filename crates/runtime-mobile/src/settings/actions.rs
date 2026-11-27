//! The runtime calls behind presses, submits and picked files.

use std::sync::atomic::Ordering;

use phoenix_runtime::{
   Chosen,
   Control,
   PluginInfo,
   installer::Installed,
   update::Catalog,
};

use crate::settings::{
   COLOR_ERROR,
   COLOR_STRING,
   Field,
   Job,
   LOG_QUEUED,
   Press,
   TITLE,
   settings,
};

fn plugin(index: usize) -> Option<PluginInfo> {
   settings().plugins.get(index).cloned()
}

/// Makes the runtime calls `job` asks for. They take the engine lock, so they
/// run before the settings lock is taken.
pub fn act(job: &Job) {
   match *job {
      Job::Log => LOG_QUEUED.store(false, Ordering::Relaxed),
      // The editor is multiline: Enter is a newline there, and Save saves.
      Job::Submit(Field::Code(_)) | Job::NavigateTo(_) | Job::Draft(..) | Job::Refresh => {},
      Job::Submit(field) => submit(field),
      Job::Picked(ref picked) => {
         match picked {
            Ok(Some((file, bytes))) => {
               installed(phoenix_runtime::install_file(file, bytes.clone()), file);
            },
            Ok(None) => {},
            Err(err) => {
               log::error!("file picker: {err}");
               settings().install_status =
                  Some((format!("The file picker failed: {err}"), COLOR_ERROR));
            },
         }
      },
      Job::Press(pressed) => press(pressed),
   }
}

fn submit(field: Field) {
   let Some(draft) = settings().drafts.remove(&field) else {
      return;
   };
   let input = draft.trim();
   match field {
      Field::Eval if !input.is_empty() => phoenix_runtime::evaluate(input),
      Field::Install if !input.is_empty() => {
         installed(phoenix_runtime::install(input), input);
      },
      Field::Setting(index, at) => {
         let Some(plugin) = plugin(index) else {
            return;
         };
         let Some(setting) = plugin.settings.get(at) else {
            return;
         };
         let value = match setting.control {
            Control::Text(_) | Control::Secret(_) => Chosen::Text(draft.clone()),
            Control::Number(_) => {
               let Ok(number) = input.parse::<f64>() else {
                  log::error!(
                     "[{}] {}: {input:?} is not a number",
                     plugin.name,
                     setting.key
                  );
                  return;
               };
               Chosen::Number(number)
            },
            Control::Switch(_) | Control::Select { .. } | Control::Button => return,
         };
         phoenix_runtime::set_setting(&plugin.name, &setting.key, value);
      },
      Field::Eval | Field::Install | Field::Code(_) => {},
   }
}

fn press(pressed: Press) {
   match pressed {
      Press::PickFile => crate::picker::pick(),
      // The editor starts from the plugin's code, kept as the field's draft.
      Press::EditCode(index) => {
         if let Some(plugin) = plugin(index)
            && let Some(source) = phoenix_runtime::source(&plugin.name)
         {
            settings().drafts.insert(Field::Code(index), source);
         }
      },
      Press::SaveCode(index) => {
         let draft = settings().drafts.get(&Field::Code(index)).cloned();
         if let (Some(plugin), Some(source)) = (plugin(index), draft) {
            match phoenix_runtime::save_source(&plugin.name, source) {
               Ok(()) => log::info!("saved {} and restarted it", plugin.name),
               Err(err) => log::error!("saving {} failed: {err}", plugin.name),
            }
         }
      },
      Press::Toggle(index) => {
         if let Some(plugin) = plugin(index) {
            phoenix_runtime::set_enabled(&plugin.name, !plugin.enabled);
         }
      },
      Press::Reload(index) => {
         if let Some(plugin) = plugin(index) {
            phoenix_runtime::reload(&plugin.name);
         }
      },
      Press::Uninstall(index) => {
         if let Some(plugin) = plugin(index) {
            match phoenix_runtime::uninstall(&plugin.name) {
               Ok(outcome) => log::info!("{}", uninstalled(&plugin.name, outcome)),
               Err(err) => log::error!("uninstall failed: {err}"),
            }
         }
      },
      Press::Setting(index, at) => {
         let Some(plugin) = plugin(index) else {
            return;
         };
         match plugin
            .settings
            .get(at)
            .map(|setting| (&setting.key, &setting.control))
         {
            Some((key, Control::Switch(on))) => {
               phoenix_runtime::set_setting(&plugin.name, key, Chosen::Switch(!on));
            },
            Some((key, Control::Button)) => phoenix_runtime::press(&plugin.name, key),
            _ => {},
         }
      },
      Press::Choose(index, at, option) => {
         let Some(plugin) = plugin(index) else {
            return;
         };
         if let Some(setting) = plugin.settings.get(at)
            && let Control::Select { options, .. } = &setting.control
            && let Some(choice) = options.get(option)
         {
            phoenix_runtime::set_setting(
               &plugin.name,
               &setting.key,
               Chosen::Text(choice.value.clone()),
            );
         }
      },
      Press::Listed(index) => listed(index),
      Press::ApplyUpdate => {
         phoenix_runtime::update::apply();
      },
      Press::OpenSite => {
         if let Err(err) = crate::picker::open(phoenix_runtime::update::SITE) {
            log::error!("opening {}: {err}", phoenix_runtime::update::SITE);
         }
      },
      Press::ToggleStaff => {
         phoenix_runtime::set_staff_enabled(!phoenix_runtime::staff_enabled());
      },
      Press::ToggleRemoteShell => {
         phoenix_runtime::set_remote_shell_enabled(!phoenix_runtime::remote_shell_enabled());
      },
      Press::OpenInstall => phoenix_runtime::update::fetch_catalog(),
      Press::OpenMain | Press::OpenConsole | Press::OpenPlugin(_) | Press::Back => {},
   }
}

/// Flips a listed plugin's switch. An absent plugin installs, an installed one
/// uninstalls, and one bundled in the APK, which cannot go, turns on or off.
fn listed(index: usize) {
   let Catalog::Loaded(listed) = phoenix_runtime::update::catalog() else {
      return;
   };
   let Some(plugin) = listed.get(index) else {
      return;
   };
   let present = settings()
      .plugins
      .iter()
      .find(|other| other.name == plugin.name)
      .cloned();
   match present {
      None => {
         installed(
            phoenix_runtime::update::install_listed(&plugin.name),
            &plugin.name,
         );
      },
      Some(info) if !info.installed => phoenix_runtime::set_enabled(&info.name, !info.enabled),
      Some(info) => {
         settings().install_status = Some(match phoenix_runtime::uninstall(&info.name) {
            Ok(outcome) => {
               (
                  format!("{}.", uninstalled(&info.name, outcome)),
                  COLOR_STRING,
               )
            },
            Err(err) => (format!("Uninstall failed: {err}"), COLOR_ERROR),
         });
      },
   }
}

fn uninstalled(name: &str, outcome: phoenix_runtime::Uninstalled) -> String {
   match outcome {
      phoenix_runtime::Uninstalled::Removed => format!("Uninstalled {name}"),
      phoenix_runtime::Uninstalled::RestoredBundled => format!("Restored the bundled {name}"),
   }
}

/// Logs an install's outcome and keeps it for the install page.
fn installed(result: Result<Installed, phoenix_runtime::Error>, from: &str) {
   let status = match result {
      Ok(installed) => {
         log::info!("{installed} from {from}");
         (
            format!("{installed}. It is on the {TITLE} page now."),
            COLOR_STRING,
         )
      },
      Err(err) => {
         log::error!("install failed: {err}");
         (format!("Install failed: {err}"), COLOR_ERROR)
      },
   };
   settings().install_status = Some(status);
}
