//! Bakes the build's version into `PHOENIX_VERSION`: the commit's UTC date and
//! short hash, `2026.11.22.2049-146126e`, so versions sort by date. Without
//! git it is `0`, older than any site build.

use std::process::Command;

fn main() {
   // Moves on every commit, checkout and reset.
   println!("cargo:rerun-if-changed=../../.git/logs/HEAD");
   let version = Command::new("git")
      .args([
         "log",
         "-1",
         "--no-show-signature",
         "--format=%cd-%h",
         "--date=format-local:%Y.%m.%d.%H%M",
      ])
      .env("TZ", "UTC0")
      .output()
      .ok()
      .filter(|output| output.status.success())
      .and_then(|output| String::from_utf8(output.stdout).ok())
      .map(|version| version.trim().to_owned())
      .filter(|version| !version.is_empty())
      .unwrap_or_else(|| "0".to_owned());
   println!("cargo:rustc-env=PHOENIX_VERSION={version}");
}
