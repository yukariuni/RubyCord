//! Bakes the workspace version into `PHOENIX_VERSION`: `YEAR.MAJOR.MINOR`, like `2026.1.0`.

fn main() {
   let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
   println!("cargo:rustc-env=PHOENIX_VERSION={version}");
}
