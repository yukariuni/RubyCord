# Cross-builds the stand-in demo for arm64 Android, pushes it with adb, and runs it on the
# device. Run inside `nix develop`.
#
# cargo-ndk sets BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android to a sysroot without an API
# level, which NDK r30 headers reject. bindgen reads the hyphenated name first, and bash
# cannot export a hyphenated name, so the override lives here.

const API = 28
const DEVICE_DIR = "/data/local/tmp/phoenix"

def main [] {
   let root = $env.FILE_PWD | path dirname
   let sysroot = $"($env.ANDROID_NDK_ROOT)/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
   cd $root

   with-env {"BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android": $"--target=aarch64-linux-android($API) --sysroot=($sysroot)"} {
      for package in [stand-in phoenix-loader stand-in-demo] {
         ^cargo ndk -t arm64-v8a -P $API --link-builtins build --release -p $package
      }
   }

   let staging = $root | path join target phone
   rm -rf $staging
   mkdir ($staging | path join home plugins)
   ^esbuild plugins/example/src/index.ts --bundle --format=iife --target=es2023 --log-level=warning $"--outfile=($staging)/home/plugins/example.js"
   let release = $root | path join target aarch64-linux-android release
   for file in [libstand_in.so libphoenix.so stand-in-demo] {
      cp ($release | path join $file) $staging
   }
   let probe = $release | path join libshadowhook_nothing.so
   cp $probe $staging

   ^adb shell $"rm -rf ($DEVICE_DIR)"
   ^adb push $staging $DEVICE_DIR
   ^adb logcat -c
   ^adb shell $"cd ($DEVICE_DIR) && chmod 755 stand-in-demo && LD_LIBRARY_PATH=. ./stand-in-demo run ./libstand_in.so ./libphoenix.so ./home"
   ^adb logcat -d -s phoenix
}
