# Builds the loader for arm64 or armv7 Android, patches Discord's split APKs with it, installs
# them on the device and tails phoenix's log. Pass base.apk plus the ABI, density and language
# splits, and the ABI split picks the loader's target. Plugins ride along in the APK. Run inside
# `nix develop`. The bindgen override is explained in phone-demo.nu. `--fast` builds the loader
# with the dev-fast profile, 3 s incremental against 25 s for release on a runtime change.

const API = 28
const PACKAGE = "com.discord"

def main [--fast, ...apks: path] {
   let root = $env.FILE_PWD | path dirname
   let sysroot = $"($env.ANDROID_NDK_ROOT)/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
   let apks = $apks | each { path expand }
   cd $root

   let profile = if $fast { "dev-fast" } else { "release" }
   let abis = [
      {android: arm64-v8a, rust: aarch64-linux-android}
      {android: armeabi-v7a, rust: armv7-linux-androideabi}
   ]
   let libs = $apks | each {|apk| ^unzip -Z1 $apk | lines } | flatten
   let abi = $abis | where {|abi| $"lib/($abi.android)/liblibdiscord_wasm_host.so" in $libs } | first

   with-env {$"BINDGEN_EXTRA_CLANG_ARGS_($abi.rust)": $"--target=($abi.rust)($API) --sysroot=($sysroot)"} {
      ^cargo ndk -t $abi.android -P $API --link-builtins build --profile $profile -p phoenix-loader
   }
   ^cargo build --release -p patcher

   let built = $root | path join target $abi.rust $profile
   let probe = $built | path join libshadowhook_nothing.so
   let out = $root | path join target phoenix
   rm -rf $out
   let plugins = $root | path join target phoenix-plugins
   rm -rf $plugins
   for plugin in (ls plugins | where type == dir | get name | path basename) {
      let header = open $"plugins/($plugin)/src/index.ts"
         | lines
         | take while {|line| $line starts-with "// @" }
         | str join (char nl)
      ^esbuild $"plugins/($plugin)/src/index.ts" --bundle --format=iife --target=es2023 --log-level=warning $"--banner:js=($header)" $"--outfile=($plugins)/($plugin).js"
   }
   let bundled = glob $"($plugins)/*.js" | each {|plugin| [--plugin $plugin] } | flatten
   ^($root | path join target release patcher) --out $out --loader ($built | path join libphoenix.so) --lib $probe ...$bundled ...$apks

   ^adb install-multiple -r --abi $abi.android ...(glob $"($out)/*.apk")
   ^adb shell am force-stop $PACKAGE
   ^adb logcat -c
   ^adb shell monkey -p $PACKAGE -c android.intent.category.LAUNCHER 1 | ignore
   ^adb logcat -s phoenix
}
