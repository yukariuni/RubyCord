//! CSS themes. A theme is a `.css` file in `<home>/themes`. Desktop Phoenix draws with GPUI, not a
//! DOM, so Ruby reads only the custom properties of `:root` (or `html`) rules, and a theme
//! restyles Ruby's own pages through them: `--bg`, `--surface`, `--border`, `--text`, `--muted`,
//! `--accent` and `--radius`. The DevTools dashboard is HTML, so it gets the enabled files as they
//! are and every rule in them applies. Enabled themes stack in name order, later ones winning.

use std::{
   collections::BTreeMap,
   fs,
   path::PathBuf,
   sync::{Mutex, PoisonError},
};

type Vars = BTreeMap<String, String>;

struct Theme {
   name: String,
   css: String,
   vars: Vars,
   enabled: bool,
}

/// What the Themes page lists.
pub struct Listed {
   pub name: String,
   pub enabled: bool,
   /// Whether it sets any property Ruby's own pages read.
   pub used: bool,
}

/// Colors as `0xAARRGGBB` and the corner radius in pixels, each `None` until a theme sets it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Palette {
   pub bg: Option<u32>,
   pub surface: Option<u32>,
   pub border: Option<u32>,
   pub text: Option<u32>,
   pub muted: Option<u32>,
   pub accent: Option<u32>,
   pub radius: Option<f32>,
}

static THEMES: Mutex<Option<Vec<Theme>>> = Mutex::new(None);

/// Where theme files live, once phoenix has started.
pub fn dir() -> Option<PathBuf> {
   crate::state().map(|state| state.home.join("themes"))
}

fn enabled_path() -> Option<PathBuf> {
   crate::state().map(|state| state.home.join("themes.json"))
}

fn load() -> Vec<Theme> {
   let Some(dir) = dir() else {
      return Vec::new();
   };
   let on: Vec<String> = enabled_path()
      .and_then(|path| fs::read(path).ok())
      .and_then(|bytes| serde_json::from_slice(&bytes).ok())
      .unwrap_or_default();
   let mut themes: Vec<Theme> = fs::read_dir(&dir)
      .into_iter()
      .flatten()
      .filter_map(Result::ok)
      .map(|entry| entry.path())
      .filter(|path| {
         path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("css"))
      })
      .filter_map(|path| {
         let name = path.file_stem()?.to_str()?.to_owned();
         let css = fs::read_to_string(&path)
            .inspect_err(|err| log::warn!("theme {name}: {err}"))
            .ok()?;
         Some(Theme {
            vars: parse(&css),
            enabled: on.contains(&name),
            name,
            css,
         })
      })
      .collect();
   themes.sort_by(|a, b| a.name.cmp(&b.name));
   themes
}

fn with<T>(f: impl FnOnce(&mut Vec<Theme>) -> T) -> T {
   let mut themes = THEMES.lock().unwrap_or_else(PoisonError::into_inner);
   f(themes.get_or_insert_with(load))
}

pub fn list() -> Vec<Listed> {
   with(|themes| {
      themes
         .iter()
         .map(|theme| Listed {
            name: theme.name.clone(),
            enabled: theme.enabled,
            used: palette_of(&theme.vars) != Palette::default(),
         })
         .collect()
   })
}

/// Rereads the folder, for files added or edited since.
pub fn reload() {
   *THEMES.lock().unwrap_or_else(PoisonError::into_inner) = None;
   crate::refresh_settings();
}

/// Saves a picked `.css` file into the themes folder and turns it on. Returns the theme's name.
pub fn import(file_name: &str, bytes: &[u8]) -> Result<String, String> {
   const MAX: usize = 1 << 20;
   let name = file_name
      .strip_suffix(".css")
      .or_else(|| file_name.strip_suffix(".CSS"))
      .filter(|name| {
         !name.is_empty()
            && name
               .chars()
               .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '-' | '.'))
            && !name.starts_with('.')
      })
      .ok_or("pick a .css file with a plain name")?;
   if bytes.len() > MAX {
      return Err("the file is over 1 MB".to_owned());
   }
   std::str::from_utf8(bytes).map_err(|_| "the file is not UTF-8 text")?;
   let dir = dir().ok_or("Ruby has not started")?;
   fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
   fs::write(dir.join(format!("{name}.css")), bytes).map_err(|err| err.to_string())?;
   reload();
   set_enabled(name, true);
   Ok(name.to_owned())
}

pub fn set_enabled(name: &str, enabled: bool) {
   let on = with(|themes| {
      for theme in themes.iter_mut().filter(|theme| theme.name == name) {
         theme.enabled = enabled;
      }
      themes
         .iter()
         .filter(|theme| theme.enabled)
         .map(|theme| theme.name.clone())
         .collect::<Vec<_>>()
   });
   if let Some(path) = enabled_path()
      && let Err(err) = serde_json::to_vec(&on)
         .map_err(std::io::Error::other)
         .and_then(|bytes| fs::write(path, bytes))
   {
      log::warn!("themes: saving the enabled list: {err}");
   }
   crate::refresh_settings();
}

/// The enabled themes' CSS, for the DevTools dashboard.
pub fn css() -> String {
   with(|themes| {
      themes
         .iter()
         .filter(|theme| theme.enabled)
         // Keeps a theme from closing the <style> element it is placed in.
         .map(|theme| theme.css.replace("</", "<\\/"))
         .collect::<Vec<_>>()
         .join("\n")
   })
}

/// The enabled themes' custom properties merged into one [`Palette`].
pub fn palette() -> Palette {
   with(|themes| {
      let mut vars = Vars::new();
      for theme in themes.iter().filter(|theme| theme.enabled) {
         vars.extend(theme.vars.clone());
      }
      palette_of(&vars)
   })
}

fn palette_of(vars: &Vars) -> Palette {
   let color = |name: &str| resolve(vars, name).and_then(|value| parse_color(&value));
   Palette {
      bg: color("--bg"),
      surface: color("--surface"),
      border: color("--border"),
      text: color("--text"),
      muted: color("--muted"),
      accent: color("--accent"),
      radius: resolve(vars, "--radius").and_then(|value| {
         let px = value.strip_suffix("px").unwrap_or(&value).trim();
         px.parse::<f32>()
            .ok()
            .filter(|px| (0.0..=64.0).contains(px))
      }),
   }
}

/// A property's value with `var(--other)` followed, a few levels deep to stop cycles.
fn resolve(vars: &Vars, name: &str) -> Option<String> {
   let mut value = vars.get(name)?.clone();
   for _ in 0..4 {
      let Some(inner) = value
         .strip_prefix("var(")
         .and_then(|rest| rest.strip_suffix(')'))
      else {
         return Some(value);
      };
      let (target, fallback) = inner.split_once(',').unwrap_or((inner, ""));
      value = vars
         .get(target.trim())
         .cloned()
         .or_else(|| (!fallback.trim().is_empty()).then(|| fallback.trim().to_owned()))?;
   }
   None
}

/// The custom properties set by `:root` and `html` rules. Everything else, at-rules included,
/// is skipped.
fn parse(css: &str) -> Vars {
   let mut plain = String::with_capacity(css.len());
   let mut rest = css;
   while let Some(start) = rest.find("/*") {
      plain.push_str(&rest[..start]);
      rest = rest[start..]
         .find("*/")
         .map_or("", |end| &rest[start + end + 2..]);
   }
   plain.push_str(rest);

   let mut vars = Vars::new();
   let mut rest = plain.as_str();
   while let Some((selector, tail)) = rest.split_once('{') {
      let (body, after) = tail.split_once('}').unwrap_or((tail, ""));
      rest = after;
      // An at-rule's inner rule leaves its own `}` behind, which the next selector then holds.
      let selector = selector.rsplit('}').next().unwrap_or(selector);
      if !selector
         .split(',')
         .any(|part| matches!(part.trim(), ":root" | "html"))
      {
         continue;
      }
      for declaration in body.split(';') {
         if let Some((name, value)) = declaration.split_once(':')
            && name.trim().starts_with("--")
            && !value.trim().is_empty()
         {
            vars.insert(name.trim().to_owned(), value.trim().to_owned());
         }
      }
   }
   vars
}

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()`/`rgba()` and `transparent`, as `0xAARRGGBB`.
fn parse_color(text: &str) -> Option<u32> {
   let text = text.trim().to_ascii_lowercase();
   if text == "transparent" {
      return Some(0);
   }
   if let Some(hex) = text.strip_prefix('#') {
      if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
         return None;
      }
      let digit = |at: usize| u32::from_str_radix(&hex[at..=at], 16).ok().map(|d| d * 17);
      let pair = |at: usize| u32::from_str_radix(&hex[at..at + 2], 16).ok();
      let (r, g, b, a) = match hex.len() {
         3 | 4 => (
            digit(0)?,
            digit(1)?,
            digit(2)?,
            if hex.len() == 4 { digit(3)? } else { 255 },
         ),
         6 | 8 => (
            pair(0)?,
            pair(2)?,
            pair(4)?,
            if hex.len() == 8 { pair(6)? } else { 255 },
         ),
         _ => return None,
      };
      return Some(a << 24 | r << 16 | g << 8 | b);
   }
   let inner = text
      .strip_prefix("rgba(")
      .or_else(|| text.strip_prefix("rgb("))?
      .strip_suffix(')')?;
   let parts: Vec<&str> = inner
      .split([',', ' ', '/'])
      .filter(|part| !part.is_empty())
      .collect();
   if !matches!(parts.len(), 3 | 4) {
      return None;
   }
   let channel = |part: &str| {
      part
         .parse::<f32>()
         .ok()
         .map(|n| n.clamp(0.0, 255.0).round() as u32)
   };
   let alpha = match parts.get(3) {
      Some(part) => match part.strip_suffix('%') {
         Some(percent) => percent.parse::<f32>().ok()? / 100.0,
         None => part.parse::<f32>().ok()?,
      },
      None => 1.0,
   };
   let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
   Some(a << 24 | channel(parts[0])? << 16 | channel(parts[1])? << 8 | channel(parts[2])?)
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn colors() {
      assert_eq!(parse_color("#f00"), Some(0xFFFF_0000));
      assert_eq!(parse_color("#1a0f14"), Some(0xFF1A_0F14));
      assert_eq!(parse_color("#1a0f1480"), Some(0x801A_0F14));
      assert_eq!(parse_color("rgb(255, 0, 128)"), Some(0xFFFF_0080));
      assert_eq!(parse_color("rgba(0 0 0 / 50%)"), Some(0x8000_0000));
      assert_eq!(parse_color("transparent"), Some(0));
      assert_eq!(parse_color("#12"), None);
      assert_eq!(parse_color("#ggg"), None);
      assert_eq!(parse_color("rgb(1,2)"), None);
      assert_eq!(parse_color("hotpink"), None);
   }

   #[test]
   fn palette_from_root_variables() {
      let palette = palette_of(&parse(
         ":root { --bg: #1a0f14; --accent: #e0115f; --radius: 12px; }",
      ));
      assert_eq!(palette.bg, Some(0xFF1A_0F14));
      assert_eq!(palette.accent, Some(0xFFE0_115F));
      assert_eq!(palette.radius, Some(12.0));
   }

   #[test]
   fn only_root_variables_count() {
      let css = "/* :root { --bg: red } */
         .card { --bg: #000 }
         @media (min-width: 1px) { a { color: red } }
         html, body { --text: #fff; color: red; --muted: var(--text) }
         :root { --accent: var(--missing, #0f0); --radius: 99px; --loop: var(--loop) }";
      let vars = parse(css);
      assert!(!vars.contains_key("--bg"));
      assert_eq!(vars.len(), 5, "{vars:?}");
      let palette = palette_of(&vars);
      assert_eq!(palette.muted, Some(0xFFFF_FFFF));
      assert_eq!(palette.accent, Some(0xFF00_FF00));
      assert_eq!(palette.radius, None, "out of range");
      assert_eq!(resolve(&vars, "--loop"), None);
   }
}
