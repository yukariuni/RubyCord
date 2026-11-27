#[path = "../loader/winres.rs"]
mod winres;

fn main() {
   // CI builds the launcher (which carries the loader) first and points this at it, so the
   // installer is one file. A plain build embeds nothing, and Inject says so.
   println!("cargo:rerun-if-env-changed=RUBYCORD_LAUNCHER_EXE");
   let out =
      std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("rubycord.exe");
   match std::env::var_os("RUBYCORD_LAUNCHER_EXE") {
      Some(exe) => {
         println!(
            "cargo:rerun-if-changed={}",
            std::path::Path::new(&exe).display()
         );
         std::fs::copy(&exe, &out).expect("copying RUBYCORD_LAUNCHER_EXE");
      },
      None => std::fs::write(&out, []).expect("writing the empty launcher"),
   }
   winres::embed(
      winres::Kind::App,
      "rubycord-installer.exe",
      "RubyCord installer for Discord Phoenix",
      None,
   );
}
