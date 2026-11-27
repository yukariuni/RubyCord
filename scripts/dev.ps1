<#
.SYNOPSIS
Fast build, patch, install, hot-reload, and live debug CLI for RubyCord.

.EXAMPLE
.\scripts\dev.ps1                     # Fast dev-fast Rust build + patch + install + launch
.\scripts\dev.ps1 -Release            # Release build + update web/plugins/libphoenix-arm64.so + install
.\scripts\dev.ps1 -SkipBuild          # Bundle plugins + patch + install (no Rust compile)
.\scripts\dev.ps1 -HotReload taptap   # Bundle & hot-reload one plugin over ADB (<0.5s)
.\scripts\dev.ps1 -Logs               # Print live logs from 127.0.0.1:4242
.\scripts\dev.ps1 -Ops banner         # Inspect recent decoded UI-op batches matching "banner"
.\scripts\dev.ps1 -Events view        # Inspect recent view/host events
.\scripts\dev.ps1 -Tree               # Dump live Phoenix view tree from root
.\scripts\dev.ps1 -Eval "phoenix.ui.root()"
.\scripts\dev.ps1 -Test               # Run fast desktop Rust unit tests (phoenix-ui-mobile, phoenix-ui-desktop, phoenix-wire)
.\scripts\dev.ps1 -Desktop            # Same loop for Desktop Phoenix: build, copy plugins, restart Discord through the injector
.\scripts\dev.ps1 -Desktop -Logs      # Any debug-shell command above, against Desktop Phoenix (no adb)
.\scripts\dev.ps1 -Desktop -Events MESSAGE  # Gateway events, Discord's API requests and native hook hits on desktop
.\scripts\dev.ps1 -Desktop -Eval "phoenix.native.strings('MESSAGE_CREATE')"
.\scripts\dev.ps1 -Sigscan strings "some text"   # Offline Discord.exe lookups: check, strings, refs, dis
#>
[CmdletBinding()]
param(
   [switch]$Release,
   [switch]$SkipBuild,
   [switch]$NoInstall,
   [switch]$NoLaunch,
   [switch]$Test,
   [switch]$Desktop,
   [switch]$NoRuntime,
   [string]$HotReload,
   [string]$Eval,
   [switch]$Logs,
   [switch]$Ops,
   [switch]$Events,
   [switch]$Tree,
   [switch]$Clear,
   [switch]$Sigscan,
   [Parameter(Position = 0)]
   [string]$Arg,
   [Parameter(Position = 1, ValueFromRemainingArguments = $true)]
   [string[]]$Rest
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "E:\phoenix-build\target" }
$OutApk = if ($env:PHOENIX_OUT_APK) { $env:PHOENIX_OUT_APK } else { "E:\phoenix-build\phoenix-updated.apk" }
$SplitDir = if ($env:PHOENIX_SPLITS) { $env:PHOENIX_SPLITS } else { Join-Path (Split-Path $RepoRoot -Parent) "arm64\dl" }

function Find-Esbuild {
   if ($env:ESBUILD -and (Test-Path $env:ESBUILD)) { return $env:ESBUILD }
   $candidates = @(
      "$env:APPDATA\npm\node_modules\wrangler\node_modules\@esbuild\win32-x64\esbuild.exe",
      "$env:APPDATA\npm\node_modules\esbuild\esbuild.exe"
   )
   foreach ($c in $candidates) {
      if (Test-Path $c) { return $c }
   }
   $cmd = Get-Command esbuild -ErrorAction SilentlyContinue
   if ($cmd) { return $cmd.Source }
   throw "esbuild not found; set `$env:ESBUILD"
}

function Forward-DebugShell {
   # Desktop Phoenix listens on this machine's own 127.0.0.1:4242, which a
   # forward would take.
   if ($Desktop) { return }
   adb start-server | Out-Null
   adb forward tcp:4242 tcp:4242 | Out-Null
}

# Desktop's shell is always on, so it wants the per-start token it writes
# beside its plugins.
function Get-DebugHeaders {
   if (-not $Desktop) { return @{} }
   $tokenFile = Join-Path $env:LOCALAPPDATA "DiscordPhoenix\rubycord\debug-shell-token"
   if (-not (Test-Path $tokenFile)) { return @{} }
   @{ "X-RubyCord-Token" = (Get-Content $tokenFile -Raw).Trim() }
}

function Invoke-DebugGet([string]$Path) {
   Forward-DebugShell
   Invoke-RestMethod -Uri "http://127.0.0.1:4242$Path" -Headers (Get-DebugHeaders)
}

function Invoke-DebugPost([string]$Path, [string]$Body = "", [string]$ContentType = "text/plain") {
   Forward-DebugShell
   Invoke-RestMethod -Uri "http://127.0.0.1:4242$Path" -Method Post -Body $Body -ContentType $ContentType -Headers (Get-DebugHeaders)
}

# Fast host unit tests
if ($Test) {
   Push-Location $RepoRoot
   try {
      $env:CARGO_TARGET_DIR = $TargetDir
      cargo +stable-x86_64-pc-windows-gnu test -p phoenix-ui-mobile -p phoenix-ui-desktop -p phoenix-wire
      exit $LASTEXITCODE
   } finally {
      Pop-Location
   }
}

# Remote debug shell commands
if ($Clear) {
   Invoke-DebugPost "/clear" | ConvertTo-Json -Depth 4
   exit 0
}
if ($Logs) {
   Invoke-DebugGet "/logs"
   exit 0
}
if ($Sigscan) {
   $tools = Join-Path (Split-Path $TargetDir -Parent) "wintools"
   if (Test-Path $tools) { $env:PATH = "$tools;$env:PATH" }
   $env:CARGO_TARGET_DIR = $TargetDir
   & cargo +stable-x86_64-pc-windows-gnu build -q --release -p sigscan
   if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
   $exe = Join-Path $env:LOCALAPPDATA "DiscordPhoenix\app\Discord.exe"
   $sigscanArgs = @($exe)
   if ($Arg) { $sigscanArgs += $Arg }
   if ($Rest) { $sigscanArgs += $Rest }
   & (Join-Path $TargetDir "release\sigscan.exe") @sigscanArgs
   exit $LASTEXITCODE
}
if ($Eval) {
   Invoke-DebugPost "/eval" $Eval
   exit 0
}
if ($Ops) {
   $q = if ($Arg) { "?match=$([uri]::EscapeDataString($Arg))" } else { "" }
   Invoke-DebugGet "/ops$q" | ConvertTo-Json -Depth 12
   exit 0
}
if ($Events) {
   $q = if ($Arg) { "?match=$([uri]::EscapeDataString($Arg))" } else { "" }
   Invoke-DebugGet "/events$q" | ConvertTo-Json -Depth 12
   exit 0
}
if ($Tree) {
   $p = if ($Arg) { "/tree/$Arg" } else { "/tree" }
   Invoke-DebugGet $p | ConvertTo-Json -Depth 16
   exit 0
}

# Sub-second plugin hot-reload via Remote Debug Shell
if ($HotReload) {
   $entry = Join-Path $RepoRoot "plugins\$HotReload\src\index.ts"
   if (-not (Test-Path $entry)) { throw "Plugin entry not found: $entry" }
   $esbuild = Find-Esbuild
   $tmp = Join-Path ([System.IO.Path]::GetTempPath()) "phoenix-hot-$HotReload.js"
   try {
      $headerLines = Get-Content $entry | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne "" }
      $banner = @()
      foreach ($line in $headerLines) {
         if ($line.StartsWith("//")) { $banner += $line } else { break }
      }
      & $esbuild $entry --bundle --format=iife --target=es2020 --outfile=$tmp
      if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
      $body = [System.IO.File]::ReadAllText($tmp)
      if ($banner.Count -gt 0) {
         $body = ($banner -join "`n") + "`n" + $body
      }
      Forward-DebugShell
      $resp = Invoke-RestMethod -Uri "http://127.0.0.1:4242/plugins/$HotReload" -Method Put -Body ([System.Text.Encoding]::UTF8.GetBytes($body)) -ContentType "text/javascript; charset=utf-8" -Headers (Get-DebugHeaders)
      Write-Host "Hot-reloaded $HotReload`: $($resp | ConvertTo-Json -Compress)"
   } finally {
      Remove-Item $tmp -Force -ErrorAction SilentlyContinue
   }
   exit 0
}

# Desktop Phoenix: build the loader and injector, copy plugins to its home and
# restart Discord through the injector.
if ($Desktop) {
   Push-Location $RepoRoot
   try {
      $profileName = if ($Release) { "release" } else { "dev-fast" }
      if (-not $SkipBuild) {
         # Git's MinGW dlltool writes raw-dylib import libraries its ld cannot
         # link, so take dlltool from the toolchain, with the `as` it runs from
         # its own directory, and keep Git's gcc for QuickJS and SQLite.
         $tools = Join-Path (Split-Path $TargetDir -Parent) "wintools"
         if (-not (Test-Path (Join-Path $tools "as.exe"))) {
            New-Item -ItemType Directory -Force $tools | Out-Null
            $sysroot = rustup run stable-x86_64-pc-windows-gnu rustc --print sysroot
            Copy-Item (Join-Path $sysroot "lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained\dlltool.exe") $tools -Force
            Copy-Item (Get-Command as.exe).Source $tools -Force
         }
         $env:PATH = "$tools;$env:PATH"
         $env:CARGO_TARGET_DIR = $TargetDir
         Write-Host "Building phoenix-loader and phoenix-inject (windows, profile=$profileName)..."
         $cargoArgs = @("+stable-x86_64-pc-windows-gnu", "build", "-p", "phoenix-loader", "-p", "phoenix-inject")
         if ($Release) { $cargoArgs += "--release" } else { $cargoArgs += @("--profile", "dev-fast") }
         & cargo @cargoArgs
         if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
      }

      $env:ESBUILD = Find-Esbuild
      node web/tools/bundle-plugins.ts
      if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
      $pluginDir = Join-Path $env:LOCALAPPDATA "DiscordPhoenix\rubycord\plugins"
      New-Item -ItemType Directory -Force $pluginDir | Out-Null
      (Get-Content (Join-Path $RepoRoot "web\plugins\index.json") -Raw | ConvertFrom-Json) |
         Where-Object { $_.name -ne "example" } |
         ForEach-Object { Copy-Item (Join-Path $RepoRoot "web\plugins\$($_.name).js") $pluginDir -Force }

      if (-not $NoLaunch) {
         Get-Process Discord -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -like "*DiscordPhoenix*" } |
            Stop-Process -Force
         Start-Sleep -Milliseconds 500
         $outDir = Join-Path $TargetDir $profileName
         $injectArgs = @()
         if ($NoRuntime) { $injectArgs += "--no-runtime" }
         # The debug shell otherwise waits for DevTools to open; -Eval and friends need it.
         $env:RUBYCORD_DEVTOOLS = "1"
         $injectArgs += (Join-Path $outDir "phoenix.dll")
         & (Join-Path $outDir "phoenix-inject.exe") @injectArgs
         exit $LASTEXITCODE
      }
   } finally {
      Pop-Location
   }
   exit 0
}

# Build + bundle + patch + install loop
Push-Location $RepoRoot
try {
   $profileName = if ($Release) { "release" } else { "dev-fast" }
   $soPath = Join-Path $TargetDir "aarch64-linux-android\$profileName\libphoenix.so"

   if (-not $SkipBuild) {
      $ndk = if ($env:ANDROID_NDK_HOME -and (Test-Path $env:ANDROID_NDK_HOME)) {
         $env:ANDROID_NDK_HOME
      } elseif (Test-Path "C:/Users/manti/AppData/Local/Android/Sdk/ndk/26.0.10792818") {
         "C:/Users/manti/AppData/Local/Android/Sdk/ndk/26.0.10792818"
      } else {
         (Get-ChildItem "$env:LOCALAPPDATA\Android\Sdk\ndk" | Sort-Object Name -Descending | Select-Object -First 1).FullName.Replace('\', '/')
      }
      $clangVer = (Get-ChildItem "$ndk/toolchains/llvm/prebuilt/windows-x86_64/lib/clang" | Select-Object -First 1).Name
      $env:ANDROID_NDK_HOME = $ndk
      $env:CARGO_TARGET_DIR = $TargetDir
      if (-not $env:LIBCLANG_PATH) { $env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin" }
      [Environment]::SetEnvironmentVariable(
         "BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android",
         "--target=aarch64-linux-android28 --sysroot=$ndk/toolchains/llvm/prebuilt/windows-x86_64/sysroot -isystem $ndk/toolchains/llvm/prebuilt/windows-x86_64/lib/clang/$clangVer/include",
         "Process"
      )
      $cleanPath = ($env:PATH -split ';' | Where-Object { $_ -notmatch 'mingw' }) -join ';'
      if (Test-Path "E:\phoenix-build\bin") {
         $env:PATH = "E:\phoenix-build\bin;$cleanPath"
      } else {
         $env:PATH = $cleanPath
      }

      Write-Host "Building phoenix-loader (arm64-v8a, profile=$profileName)..."
      $cargoArgs = @("+stable-x86_64-pc-windows-gnu", "ndk", "-t", "arm64-v8a", "-P", "28", "--link-builtins", "build", "-p", "phoenix-loader")
      if ($Release) { $cargoArgs += "--release" } else { $cargoArgs += @("--profile", "dev-fast") }
      & cargo @cargoArgs
      if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

      if ($Release) {
         Copy-Item $soPath (Join-Path $RepoRoot "web\plugins\libphoenix-arm64.so") -Force
         Write-Host "Updated web/plugins/libphoenix-arm64.so"
      }
   }

   $env:ESBUILD = Find-Esbuild
   node web/tools/bundle-plugins.ts
   if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

   $loader = if (Test-Path $soPath) { $soPath } else { Join-Path $RepoRoot "web\plugins\libphoenix-arm64.so" }
   $plugins = (Get-Content (Join-Path $RepoRoot "web\plugins\index.json") -Raw | ConvertFrom-Json) |
      Where-Object { $_.name -ne "example" } |
      ForEach-Object { "--plugin"; Join-Path $RepoRoot "web\plugins\$($_.name).js" }

   $splits = @(
      (Join-Path $SplitDir "base.apk"),
      (Join-Path $SplitDir "config.arm64_v8a.apk"),
      (Join-Path $SplitDir "config.xxhdpi.apk"),
      (Join-Path $SplitDir "config.en.apk")
   ) | Where-Object { Test-Path $_ }
   & cargo +stable-x86_64-pc-windows-gnu run --release -p patcher -- --merge --out $OutApk `
      --loader $loader --lib "libshadowhook_nothing.so=$(Join-Path $RepoRoot "web\plugins\libshadowhook_nothing-arm64.so")" `
      @plugins @splits
   if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

   if (-not $NoInstall) {
      adb start-server | Out-Null
      adb install -r $OutApk
      if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
      if (-not $NoLaunch) {
         adb shell am force-stop com.discord
         adb shell monkey -p com.discord -c android.intent.category.LAUNCHER 1 | Out-Null
      }
   }
} finally {
   Pop-Location
}
