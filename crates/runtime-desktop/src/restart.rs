use phoenix_hook::image::Image;
use phoenix_sigs::desktop::{WINDOWS_RESTART_SCRIPT, windows_restart_script};

/// GPUI's restart script, relaunching through the launcher that started this
/// Discord so RubyCord loads again. Without one it restarts Discord as GPUI
/// would.
const RESTART_SCRIPT: &str = r#"$oldPid = $env:ZED_RESTART_PID
$exePath = $env:ZED_RESTART_EXECUTABLE
$arguments = $env:ZED_RESTART_ARGUMENTS
foreach ($name in 'PID','EXECUTABLE','ARGUMENTS') {
    Remove-Item "Env:ZED_RESTART_$name" -ErrorAction SilentlyContinue
}
$launcher = $env:RUBYCORD_RESTART_EXE
if ($launcher -and (Test-Path -LiteralPath $launcher -PathType Leaf)) {
    $exePath = $launcher
    $arguments = "$env:RUBYCORD_RESTART_ARGS $arguments"
}
while (Get-Process -Id $oldPid -ErrorAction SilentlyContinue) {
    Start-Sleep -Seconds 0.1
}
$opts = @{}
if ($arguments) { $opts.ArgumentList = $arguments }
Start-Process -FilePath $exePath @opts
"#;

pub fn install(image: &Image<'static>) {
   let addr = match windows_restart_script(image) {
      Ok(addr) => addr,
      Err(err) => {
         log::warn!("could not redirect Discord restart: {err}");
         return;
      },
   };
   const {
      assert!(RESTART_SCRIPT.len() <= WINDOWS_RESTART_SCRIPT.len());
   }
   // GPUI passes the script as a Rust string slice, so keep its original
   // length.
   let mut script = [b' '; WINDOWS_RESTART_SCRIPT.len()];
   script[..RESTART_SCRIPT.len()].copy_from_slice(RESTART_SCRIPT.as_bytes());
   // SAFETY: the locator matched the script and Discord has yet to start.
   if let Err(err) = unsafe { image.write_code(addr, &script) } {
      log::warn!("could not redirect Discord restart: {err}");
   }
}
