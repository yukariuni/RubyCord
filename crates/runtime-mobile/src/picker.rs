//! The document picker, toasts, the clipboard and links through Phoenix's own
//! host modules. The chosen file goes to [`crate::settings::picked`].

use std::time::Duration;

use phoenix_wire::Value;

use crate::host::{
   self,
   Event,
   Field,
   Method,
};

/// The user takes their time in the picker.
const PICK_TIMEOUT: Duration = Duration::from_mins(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The file the user chose, by name, or `None` when they backed out.
pub type Picked = Result<Option<(String, Vec<u8>)>, String>;

/// Shows `text` briefly at the bottom of the screen.
#[cfg(target_os = "android")]
pub fn toast(text: &str) -> Result<(), String> {
   host::send(
      Method::SystemOverlayToast,
      &[(Field::Text, Value::Str(text.to_owned()))],
      Vec::new(),
   )
   .map_err(|err| err.to_string())
}

/// Shows `text` briefly in a banner at the top of the screen, since iOS
/// Phoenix takes `SystemOverlay.toast` and shows nothing.
#[cfg(target_os = "ios")]
#[expect(
   clippy::unnecessary_wraps,
   reason = "the same signature as Android's, which can fail"
)]
pub fn toast(text: &str) -> Result<(), String> {
   crate::patches::banners::show_banner(
      text.to_owned(),
      None,
      None,
      None,
      TOAST_MS,
      crate::patches::banners::BannerAction::None,
   );
   Ok(())
}

/// About Android's `Toast.LENGTH_SHORT`, 2 s.
#[cfg(target_os = "ios")]
const TOAST_MS: u64 = 2500;

/// Copies `text` to the Android clipboard.
pub fn copy(text: &str) -> Result<(), String> {
   host::send(Method::ClipboardSetString, &[], text.as_bytes().to_vec())
      .map_err(|err| err.to_string())
}

/// Opens the document picker. The answer comes later through
/// [`crate::settings::picked`].
pub fn pick() {
   std::thread::spawn(|| crate::settings::picked(pick_file()));
}

/// Waits for the user to pick a file and reads it.
fn pick_file() -> Picked {
   let failed = |err: host::CallError| err.to_string();
   // Without `extensions` the picker takes any type, since pickers often file
   // `.js` under an unexpected MIME type.
   let picked = host::ask(
      Method::MediaPickerPick,
      &[
         (Field::Kind, Value::Str("files".to_owned())),
         (Field::Max, Value::I32(1)),
      ],
      Event::Picked,
      PICK_TIMEOUT,
   )
   .map_err(failed)?;
   if let Some(error) = host::field(&picked.args, Field::Error).and_then(Value::as_str) {
      return Err(error.to_owned());
   }
   let Some(Value::Arr(files)) = host::field(&picked.args, Field::Files) else {
      return Ok(None);
   };
   let Some(file) = files.first() else {
      return Ok(None);
   };
   let handle = host::field(file, Field::Handle)
      .and_then(Value::as_str)
      .ok_or("the picked file has no handle")?
      .to_owned();
   let name = host::field(file, Field::Name)
      .and_then(Value::as_str)
      .unwrap_or("plugin.js")
      .to_owned();

   let read = host::ask(
      Method::MediaPickerRead,
      &[(Field::Handle, Value::Str(handle.clone()))],
      Event::Read,
      READ_TIMEOUT,
   );
   let _ = host::send(
      Method::MediaPickerRelease,
      &[(Field::Handles, Value::Arr(vec![Value::Str(handle)]))],
      Vec::new(),
   );
   let read = read.map_err(failed)?;
   if let Some(error) = host::field(&read.args, Field::Error).and_then(Value::as_str) {
      return Err(error.to_owned());
   }
   Ok(Some((name, read.body)))
}

/// Opens `url` in the browser over Phoenix's activity.
pub fn open(url: &str) -> Result<(), String> {
   host::send(
      Method::LinkingOpenUrl,
      &[(Field::Url, Value::Str(url.to_owned()))],
      Vec::new(),
   )
   .map_err(|err| err.to_string())
}
