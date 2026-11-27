#[path = "../loader/winres.rs"]
mod winres;

/// Runs without elevation, and tells Windows it knows Windows 10, so no
/// compatibility shims apply.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;

fn main() {
   // CI builds the loader first and points this at it, so the launcher is one file. A plain
   // build embeds nothing and falls back to the `phoenix.dll` beside the launcher.
   println!("cargo:rerun-if-env-changed=RUBYCORD_LOADER_DLL");
   let out =
      std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("phoenix.dll");
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
      "rubycord.exe",
      "RubyCord launcher for Discord Phoenix",
      Some(MANIFEST),
   );
}
