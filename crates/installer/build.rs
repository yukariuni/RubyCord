#[path = "../loader/winres.rs"]
mod winres;

fn main() {
   // CI builds the loader first and points this at it, so the installer is one file that patches
   // Discord.exe to load the loader. A plain build embeds nothing, and Inject says so.
   println!("cargo:rerun-if-env-changed=RUBYCORD_LOADER_DLL");
   let out =
      std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("rubycord.dll");
   match std::env::var_os("RUBYCORD_LOADER_DLL") {
      Some(dll) => {
         println!(
            "cargo:rerun-if-changed={}",
            std::path::Path::new(&dll).display()
         );
         std::fs::copy(&dll, &out).expect("copying RUBYCORD_LOADER_DLL");
      },
      None => std::fs::write(&out, []).expect("writing the empty loader"),
   }
   winres::embed(
      winres::Kind::App,
      "rubycord-installer.exe",
      "RubyCord installer for Discord Phoenix",
      None,
   );
}
