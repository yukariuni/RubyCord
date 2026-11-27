//! What "Install plugin" makes of the link or file it is given: the URL to
//! download, the plugin's name, and a source the engine can run, or an error
//! that says what to give it instead.

use std::{collections::BTreeMap, fmt};

use rquickjs::{CatchResultExt, Context, Function, Runtime};

use crate::{Error, Header, fetch};

/// File stems that name a build's entry point rather than the plugin.
const ENTRY_STEMS: [&str; 4] = ["index", "main", "plugin", "bundle"];
/// Folders that hold a build rather than name one.
const BUILD_DIRS: [&str; 5] = ["src", "dist", "build", "lib", "out"];

#[derive(Debug, PartialEq, Eq)]
pub enum Link {
   File {
      url: String,
      name: String,
   },
   /// A gist page. Its files come from GitHub's API; `anchor` is the `#file-…`
   /// part.
   Gist {
      id: String,
      anchor: Option<String>,
   },
}

fn bad_url(url: &str) -> Error {
   Error::PluginUrl {
      url: url.to_owned(),
   }
}

/// Reads a pasted link. GitHub file pages become their raw files.
pub fn parse(input: &str) -> Result<Link, Error> {
   let bad = || bad_url(input);
   let rest = input.strip_prefix("https://").ok_or_else(bad)?;
   let (rest, anchor) = match rest.split_once('#') {
      Some((rest, anchor)) => (rest, Some(anchor)),
      None => (rest, None),
   };
   let address = rest.split('?').next().unwrap_or_default();
   let (host, path) = address.split_once('/').unwrap_or((address, ""));
   let segments = path
      .split('/')
      .filter(|segment| !segment.is_empty())
      .collect::<Vec<_>>();

   match host.to_ascii_lowercase().as_str() {
      "github.com" => {
         let [owner, repo, kind, _reference, file @ ..] = segments.as_slice() else {
            return Err(Error::PluginGitHubPage {
               url: input.to_owned(),
            });
         };
         if !matches!(*kind, "blob" | "raw") || file.is_empty() {
            return Err(Error::PluginGitHubPage {
               url: input.to_owned(),
            });
         }
         Ok(Link::File {
            url: format!(
               "https://raw.githubusercontent.com/{owner}/{repo}/{}",
               segments[3..].join("/")
            ),
            name: name_of(input, file, Some(repo))?,
         })
      },
      "raw.githubusercontent.com" => {
         let start = match segments.as_slice() {
            [_, _, "refs", "heads" | "tags", _, ..] => 5,
            _ => 3,
         };
         let file = segments
            .get(start..)
            .filter(|file| !file.is_empty())
            .ok_or_else(bad)?;
         Ok(Link::File {
            url: format!("https://{rest}"),
            name: name_of(input, file, segments.get(1).copied())?,
         })
      },
      "gist.github.com" => {
         let id = match segments.as_slice() {
            [id] | [_, id, ..] => (*id).to_owned(),
            [] => return Err(bad()),
         };
         Ok(Link::Gist {
            id,
            anchor: anchor.map(str::to_owned),
         })
      },
      // Revisions and `raw` sit between the gist and its file, so only the file
      // names it.
      "gist.githubusercontent.com" => Ok(Link::File {
         url: format!("https://{rest}"),
         name: name_of(input, &segments[segments.len().saturating_sub(1)..], None)?,
      }),
      _ => Ok(Link::File {
         url: format!("https://{rest}"),
         name: name_of(input, &segments, None)?,
      }),
   }
}

/// The plugin a path names. A build entry like `plugins/foo/src/index.js` is
/// named by its folder, then by `repo`, then by its own stem.
pub fn name_of(url: &str, path: &[&str], repo: Option<&str>) -> Result<String, Error> {
   let bad = || bad_url(url);
   let (file, folders) = path.split_last().ok_or_else(bad)?;
   let stem = match file.rsplit_once('.') {
      None => *file,
      Some((stem, extension)) if extension.eq_ignore_ascii_case("js") => stem,
      Some((_, "ts" | "tsx" | "mts" | "cts")) => {
         return Err(Error::PluginTypeScript {
            name: (*file).to_owned(),
         });
      },
      Some(_) => return Err(bad()),
   };
   let entry = ENTRY_STEMS
      .iter()
      .any(|entry| stem.eq_ignore_ascii_case(entry));
   let named = entry
      .then(|| {
         folders
            .iter()
            .rev()
            .find(|folder| {
               !BUILD_DIRS
                  .iter()
                  .any(|dir| folder.eq_ignore_ascii_case(dir))
            })
            .copied()
            .or(repo)
      })
      .flatten()
      .and_then(clean);
   named.or_else(|| clean(stem)).ok_or_else(bad)
}

/// `name` with everything but letters, digits, `-` and `_` made a `-`.
fn clean(name: &str) -> Option<String> {
   let cleaned = name
      .chars()
      .map(|char| {
         if char.is_ascii_alphanumeric() || char == '-' || char == '_' {
            char
         } else {
            '-'
         }
      })
      .collect::<String>();
   let cleaned = cleaned.trim_matches('-');
   (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

#[derive(serde::Deserialize)]
struct Gist {
   files: BTreeMap<String, GistFile>,
}

#[derive(serde::Deserialize)]
struct GistFile {
   filename: String,
   raw_url: String,
}

/// The `.js` file of gist `id` to download, and its plugin name. A gist with
/// several picks the one its `#file-…` anchor names.
pub fn gist(url: &str, id: &str, anchor: Option<&str>) -> Result<(String, String), Error> {
   // GitHub's API refuses requests without a User-Agent.
   let body = fetch::get_with(
      &format!("https://api.github.com/gists/{id}"),
      vec![
         (
            "Accept".to_owned(),
            "application/vnd.github+json".to_owned(),
         ),
         ("User-Agent".to_owned(), "rubycord".to_owned()),
      ],
   )?;
   let gist = serde_json::from_slice::<Gist>(&body).map_err(|_| bad_url(url))?;
   let mut scripts = gist
      .files
      .into_values()
      .filter(|file| file.filename.to_ascii_lowercase().ends_with(".js"))
      .collect::<Vec<_>>();
   let found = scripts.len();
   let chosen = match anchor.and_then(|anchor| anchor.strip_prefix("file-")) {
      Some(anchor) => scripts
         .into_iter()
         .find(|file| clean(&file.filename).is_some_and(|name| name.eq_ignore_ascii_case(anchor))),
      None if found == 1 => scripts.pop(),
      None => None,
   };
   let Some(chosen) = chosen else {
      return Err(Error::PluginGist {
         url: url.to_owned(),
         found,
      });
   };
   let name = name_of(url, &[chosen.filename.as_str()], None)?;
   Ok((chosen.raw_url, name))
}

/// Refuses what the engine could not run, saying why: a web page, JSON data, an
/// ES module or a script that does not parse.
pub fn check(name: &str, source: &str) -> Result<(), Error> {
   let head = source.trim_start_matches('\u{FEFF}').trim_start();
   if head.starts_with('<') {
      return Err(Error::PluginPage {
         name: name.to_owned(),
      });
   }
   // A JSON array or object parses as a script that does nothing, so the engine
   // would take an API's answer for a plugin.
   if head.starts_with(['[', '{']) && serde_json::from_str::<serde::de::IgnoredAny>(head).is_ok() {
      return Err(Error::PluginData {
         name: name.to_owned(),
      });
   }
   let Err(message) = parses(source) else {
      return Ok(());
   };
   if source.lines().map(str::trim_start).any(module_line) {
      return Err(Error::PluginModule {
         name: name.to_owned(),
      });
   }
   Err(Error::PluginSyntax {
      name: name.to_owned(),
      message,
   })
}

/// Compiles `source` the way the engine wraps it, without running it, in a
/// runtime of its own so nothing of it reaches the plugins'.
fn parses(source: &str) -> Result<(), String> {
   let runtime = Runtime::new().map_err(|err| err.to_string())?;
   let context = Context::full(&runtime).map_err(|err| err.to_string())?;
   context.with(|ctx| {
      ctx.eval::<Function<'_>, _>(crate::js::wrap(source))
         .map(drop)
         .catch(&ctx)
         .map_err(|err| err.to_string())
   })
}

fn module_line(line: &str) -> bool {
   ["import", "export"].iter().any(|keyword| {
      line
         .strip_prefix(keyword)
         .is_some_and(|rest| rest.starts_with([' ', '{', '*', '"', '\'']))
   })
}

/// A plugin that was installed, and what it took the place of.
#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
   pub name: String,
   pub replaced: Replaced,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Replaced {
   Nothing,
   Bundled,
   Installed {
      from: Option<String>,
      to: Option<String>,
   },
}

impl Replaced {
   /// What installing `source` does to `previous`, a plugin of the same name.
   #[must_use]
   pub fn of(previous: Option<&crate::PluginInfo>, source: &str) -> Self {
      match previous {
         None => Self::Nothing,
         Some(previous) if !previous.installed => Self::Bundled,
         Some(previous) => Self::Installed {
            from: previous.header.version.clone(),
            to: Header::from(source).version,
         },
      }
   }
}

impl fmt::Display for Installed {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      let name = &self.name;
      match &self.replaced {
         Replaced::Nothing => write!(f, "Installed {name}"),
         Replaced::Bundled => write!(f, "Installed {name} over the bundled one"),
         Replaced::Installed {
            from: Some(from),
            to: Some(to),
         } if from != to => write!(f, "Updated {name} {from} → {to}"),
         Replaced::Installed {
            from: Some(version),
            to: Some(_),
         } => write!(f, "Reinstalled {name} {version}"),
         Replaced::Installed { .. } => write!(f, "Updated {name}"),
      }
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   fn file(input: &str) -> (String, String) {
      match parse(input) {
         Ok(Link::File { url, name }) => (url, name),
         other => panic!("{input}: {other:?}"),
      }
   }

   #[test]
   fn turns_github_pages_into_raw_files() {
      assert_eq!(
         file("https://github.com/someone/plugins/blob/main/dist/pin-dms.js?plain=1"),
         (
            "https://raw.githubusercontent.com/someone/plugins/main/dist/pin-dms.js".to_owned(),
            "pin-dms".to_owned()
         )
      );
      assert_eq!(
         file("https://github.com/someone/plugins/raw/v1/foo.js").0,
         "https://raw.githubusercontent.com/someone/plugins/v1/foo.js"
      );
      assert!(matches!(
         parse("https://github.com/someone/plugins/tree/main/plugins/foo"),
         Err(Error::PluginGitHubPage { .. })
      ));
      assert!(matches!(
         parse("https://github.com/someone/plugins"),
         Err(Error::PluginGitHubPage { .. })
      ));
   }

   #[test]
   fn names_build_entries_by_their_folder() {
      assert_eq!(
         file("https://raw.githubusercontent.com/o/r/main/plugins/fix-embeds/dist/index.js").1,
         "fix-embeds"
      );
      assert_eq!(
         file("https://raw.githubusercontent.com/o/cool-plugin/refs/heads/main/index.js"),
         (
            "https://raw.githubusercontent.com/o/cool-plugin/refs/heads/main/index.js".to_owned(),
            "cool-plugin".to_owned()
         )
      );
      assert_eq!(file("https://example.com/index.js").1, "index");
      assert_eq!(
         file("https://example.com/p/My Plugin.min.js").1,
         "My-Plugin-min"
      );
      assert_eq!(file("https://example.com/plugins/translate").1, "translate");
   }

   #[test]
   fn keeps_other_links_as_they_are() {
      assert_eq!(
         file("https://assets.rubycord.tools/plugins/translate.js?v=2#top"),
         (
            "https://assets.rubycord.tools/plugins/translate.js?v=2".to_owned(),
            "translate".to_owned()
         )
      );
      assert_eq!(
         file("https://gist.githubusercontent.com/u/abc/raw/def/view-raw.js").1,
         "view-raw"
      );
      assert!(matches!(
         parse("http://example.com/a.js"),
         Err(Error::PluginUrl { .. })
      ));
      assert!(matches!(
         parse("https://example.com/a.json"),
         Err(Error::PluginUrl { .. })
      ));
   }

   #[test]
   fn reads_gists_and_their_anchors() {
      assert_eq!(
         parse("https://gist.github.com/someone/0123abcd#file-my_plugin-js").ok(),
         Some(Link::Gist {
            id: "0123abcd".to_owned(),
            anchor: Some("file-my_plugin-js".to_owned()),
         })
      );
   }

   #[test]
   fn refuses_typescript_links_before_downloading() {
      assert!(matches!(
         parse("https://github.com/o/r/blob/main/plugins/foo/src/index.ts"),
         Err(Error::PluginTypeScript { .. })
      ));
   }

   #[test]
   fn accepts_plain_scripts_with_or_without_a_wrapper() {
      assert!(check("a", "const { gateway } = phoenix;\nphoenix.log('hi');").is_ok());
      assert!(
         check(
            "b",
            "// @version 1\n\"use strict\";\n(() => { var x = 1; })();"
         )
         .is_ok()
      );
   }

   #[test]
   fn says_why_a_source_cannot_run() {
      let refused = |source| check("p", source).unwrap_err();
      assert!(matches!(
         refused("<!DOCTYPE html><html></html>"),
         Error::PluginPage { .. }
      ));
      assert!(matches!(
         refused("[\n  { \"name\": \"clear-urls\" }\n]"),
         Error::PluginData { .. }
      ));
      assert!(check("c", "{ phoenix.log('a block'); }").is_ok());
      assert!(matches!(
         refused("import { x } from \"./x\";\nx();"),
         Error::PluginModule { .. }
      ));
      assert!(matches!(
         refused("phoenix.log(("),
         Error::PluginSyntax { .. }
      ));
   }
}
