//! frida-gadget in script mode, for debugging on x86 WSA.
//!
//! The loader, or the host library without the plugin runtime, gets its
//! `DT_NEEDED` `liblog.so` renamed to the gadget, whose constructor then runs
//! while the host library loads. The gadget derives its config's path from its
//! own, `.../base.apk!/lib/<abi>/libfrg.so`, and Frida 17 reads `x.apk!/entry`
//! paths with its own zip reader, so the libraries stay stored and unextracted.

use std::borrow::Cow;

/// The same length as `liblog.so`, for the in-place `DT_NEEDED` rename.
pub const GADGET: &str = "libfrg.so";
const CONFIG: &str = "libfrg.config.so";
const SCRIPT: &str = "libfrg.js.so";

/// The gadget's script, `bootstrap` (frida/bootstrap.js) with `plugins`.
///
/// The bootstrap copies the plugins into the app's files and runs them next to
/// plugins added in the app. The gadget's own load errors go nowhere on
/// Android, so a failing bootstrap reports to logcat.
#[must_use]
pub fn script(plugins: &[(String, String)], bootstrap: &str) -> String {
   let mut named: Vec<(String, &str)> = Vec::new();
   for (name, code) in plugins {
      let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
      let mut clean = base
         .chars()
         .map(|char| {
            if char.is_ascii_alphanumeric() || matches!(char, '_' | '.' | '-') {
               char
            } else {
               '_'
            }
         })
         .collect::<String>();
      if clean.strip_suffix(".js").is_none() {
         clean.push_str(".js");
      }
      match named.iter_mut().find(|(other, _)| *other == clean) {
         Some(entry) => entry.1 = code,
         None => named.push((clean, code)),
      }
   }
   let object = named
      .iter()
      .map(|(name, code)| format!("{}:{}", json(name), json(code)))
      .collect::<Vec<String>>()
      .join(",");
   format!(
      "globalThis.BUNDLED_PLUGINS = {{{object}}};\ntry {{\n{bootstrap}\n}} catch (e) {{
  new NativeFunction(Process.getModuleByName('liblog.so').getExportByName('__android_log_write'), \
       'int', ['int', 'pointer', 'pointer'])(
    6, Memory.allocUtf8String('FridaPlugins'), Memory.allocUtf8String('bootstrap failed: ' + \
       (e.stack || e)));
}}\n"
   )
}

/// Files for the native library directory, by name.
#[must_use]
pub fn files<'a>(gadget: &'a [u8], script: &'a str) -> [(&'static str, Cow<'a, [u8]>); 3] {
   let config =
      format!(r#"{{"interaction":{{"type":"script","path":"{SCRIPT}","on_change":"ignore"}}}}"#);
   [
      (GADGET, Cow::Borrowed(gadget)),
      (CONFIG, Cow::Owned(config.into_bytes())),
      (SCRIPT, Cow::Borrowed(script.as_bytes())),
   ]
}

fn json(string: &str) -> String {
   serde_json::to_string(string).unwrap_or_default()
}
