# AGENTS.md

Ruby patches Phoenix, Discord's native Rust Android client, and runs TypeScript plugins
inside it. Target build is 356.7 Alpha, versionCode 356207. "Phoenix" is Discord's client. The
runtime, the crates and the plugin global are all named `phoenix`.

Read this file before changing code. Every fact here cost someone hours to learn.

## Hard rules

- Credentials. Phoenix's HTTP requests cross `GuestHost.invoke` carrying the user's
  `authorization` header. Never log, print or save invoke frames, dispatch frames or request
  headers. Filter before writing anything to disk, and delete any capture that holds a token.
  `discord.rs` keeps the session headers in memory only.
- Secrets in logs. Plugin settings of `type: "secret"` are scrubbed from every console line by
  `console.rs`. A new credential-bearing setting must use that type.
- Privacy. Screenshots and logs from the owners' phones show private chats, emails and accounts.
  Never quote them in commits or replies, never commit them, delete them after use.
- Real accounts. Never send account-mutating requests (friend adds, settings PATCHes, guild
  joins) to test something. Test `phoenix.discord` writes against paths that return 404.
  Never inject fake messages into real channels.
- Signing. Every APK, from the patcher CLI, the site or the in-app updater, is signed with
  `crates/patcher/keys/`, which the runtime links through the patcher crate. v3 only, with
  the rotation proof from the old Valenced test key, so builds install over each other and keep
  the login. Never add the old private key. Read `keys/README.md` before touching signing.
- Plugin API lives in Rust. UI ops, patch targets, settings pages, stores and HTTP live in the
  crates and reach plugins through the `phoenix` object. Plugins are thin TypeScript. No JS
  prelude in the runtime.
- Typed boundaries. Plugins get opaque handles (`phoenix.gateway.messageCreate`), not strings
  like `"MESSAGE_CREATE"`. Read lists of events, props and handlers live from the binary. Never
  add a hand-maintained list file.
- No committed build output. `web/plugins/lib*.so` and `web/pkg/` are built by
  `web/tools/bundle-plugins.ts` for plugin bundles.
  The Frida gadgets in `web/plugins/frida-*` are the one exception, kept for x86 WSA debugging.
- Test on a device before pushing. A clean build proves nothing about the bridge.

## Layout

| Path | What |
|---|---|
| `crates/loader` | `libphoenix.so`, injected into the APK as `libplm.so`. Its constructor starts the runtime. |
| `crates/runtime` | QuickJS plugin runtime and the `phoenix` API every platform shares. `phoenix.d.ts` is the plugin API reference. `platform.rs` is the `Platform` trait the platform crates implement. |
| `crates/runtime/src/gateway.rs` | Gateway event hooks by swapping handler pointers in Phoenix's event registry. |
| `crates/runtime/src/discord.rs` | `phoenix.discord`, Discord's API with Phoenix's own session. The platforms carry the requests. |
| `crates/runtime/src/theme.rs` | CSS themes: `<home>/themes/*.css`, `:root` custom properties read into a `Palette` for the Ruby pages in `runtime-desktop/src/gpui.rs`, the raw CSS injected into the DevTools dashboard. |
| `crates/runtime/src/update.rs` | Update checks and one-tap plugin updates. Off while `ASSETS` is empty. |
| `crates/runtime/src/remote.rs` | The debug shell on `127.0.0.1:4242`, off by default on Android, switched on in Settings, Ruby, Debug. On desktop it starts when DevTools first opens, or at launch with `RUBY_DEVTOOLS` set, which `dev.ps1 -Desktop` does. The platforms add their own endpoints. |
| `crates/runtime-mobile` | phoenix on Phoenix's Android and iOS clients, started by the loader there. iOS has no installer or staff hook, and uses Foundation HTTP and a bundled SQLite. |
| `crates/runtime-mobile/src/bridge/` | The sinks every guest/host frame passes, returning `Action::{Pass, Drop, Replace}`. `jni.rs` patches the JNI table on `GuestHost.invoke`/`applyOps` and rebinds `nativeDispatch`. `ios.rs` swaps the dispatch slot in the guest's vtable and the call slot in the host callbacks' vtable. |
| `crates/runtime-mobile/src/settings/` | The Ruby settings pages, built from UI ops. |
| `crates/runtime-mobile/src/patches/` | UI patch targets, slash commands, profile tabs, emoji picker. |
| `crates/runtime-mobile/src/discord.rs` | Plugin requests to Discord through Phoenix's own `Networking` module. |
| `crates/runtime-mobile/src/host.rs` | Phoenix's host call, event and field numbers, read at startup from the name table in the installed APK's dex, or the host's Swift name table on iOS. `host::send` and `host::ask` call host modules. |
| `crates/runtime-mobile/src/picker.rs` | File picker, toasts, clipboard and links through Phoenix's host modules. |
| `crates/runtime-mobile/src/install.rs`, `loader.rs` | The APK self-update, and Android's installer opened on it through Discord's `FileProvider`. |
| `crates/runtime-desktop` | phoenix on Desktop Phoenix (`Discord.exe`, Linux `discord-phoenix`, macOS `Discord`): the access gate, the HTTP transport hooks, the Ruby pages in User Settings, message accessories and the tray. `os.rs` holds what differs per OS. On macOS, which is arm64, everything runs (see Host facts). |
| `crates/ui-mobile` | Typed codec for mobile (Android/iOS) Phoenix UI-op batches. `src/props.in` is Phoenix's prop table, `templates.json` holds captured layouts, `motion.rs` drives native springs, `mirror.rs` keeps a live copy of the view tree that host tests can drive. |
| `crates/ui-desktop` | Desktop UI host primitives (clipboard, URLs, notifications, the tray, GPUI surface hooks), one file per OS. |
| `crates/wire` | Tagged values and bridge frames (`HostCall`, `DispatchEvent`). |
| `crates/hook` | Image scanner, `swap_words`, ShadowHook through `sunhook-sys` on Android ARM. |
| `crates/sigs`, `crates/sigscan` | The gateway registry walk and handler ABI, and every other locator the runtime patches from (`desktop.rs`, `mobile.rs`, `macos.rs`). `sigscan` dumps a library's registry, runs `check` (every locator for that platform, exit 1 on a miss) and looks up strings, references and disassembly in `Discord.exe`. |
| `crates/patcher` | Patches the split APKs and signs them, as a library with a CLI. One APK merged from the splits or each split signed on its own. |
| `crates/patcher-web` | `crates/patcher` as wasm. |
| `crates/stand-in*` | A fake gateway library and a demo, to test hooks without Discord. |
| `plugins/*/src/index.ts` | Bundled plugins. Leading `// @` lines are the bundle banner and metadata, `// @warning` shows a warning and `// @unlisted` keeps a plugin out of the plugin list. |
| `docs/phoenix-356207.*` | Phoenix's schema pulled from the APK by `scripts/extract-schema.py`: 29 view kinds, 855 props, 79 host modules with 433 methods, 109 events, 286 `ic_mana_*` icons and the accessibility ids. Read it before decompiling. |
| `web/` | `tools/bundle-plugins.ts` bundles the plugins into `web/plugins/*.js`; the Frida gadgets live in `web/plugins/frida-*`. No website or update server: `ASSETS` in `update.rs` is empty. |
| `frida/` | Optional Frida gadget bootstrap, for debugging on x86. |
| `scripts/*.nu` | `phoenix.nu` builds, patches and installs on a phone. |
| `scripts/ruby-{linux,macos}.sh` | The Desktop installers, which need `RUBY_SITE` to fetch the loader. Each makes a launcher that fetches the loader before every start and leaves Discord's own files untouched. |
| `scripts/dev.ps1` | The Windows dev loop, described below. |

## Building

- Linux. `nix develop`, then
  `nu scripts/phoenix.nu base.apk config.<abi>.apk config.<dpi>.apk config.<lang>.apk`, where the
  ABI split is `config.arm64_v8a` or `config.armeabi_v7a` and picks the loader's target.
  The devshell provides the SDK, NDK and JDK.
  `phoenix.nu` ends in a blocking `adb logcat` tail, so run it in the background from a tool.
- Windows, without Nix.
  - Rust `stable-x86_64-pc-windows-gnu` with the `aarch64-linux-android` target, `cargo-ndk`, an
    NDK, a JDK and an Android SDK with a platform and build-tools in `ANDROID_SDK_ROOT`.
  - Cargo fetches ShadowHook as `sunhook-sys`.
  - Set `LIBCLANG_PATH` to an LLVM `bin` holding `libclang.dll`, and set
    `BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android="--target=aarch64-linux-android28 --sysroot=<ndk>/toolchains/llvm/prebuilt/windows-x86_64/sysroot -isystem <ndk>/.../lib/clang/<ver>/include"`
    with `env`, since the name has hyphens.
  - Git for Windows' MinGW (gcc 12) cannot link the host build scripts. Take it off `PATH` and put
    a directory with only `dlltool.exe` (from the toolchain's `self-contained`) and `as.exe` with
    its DLLs on `PATH`.
  - `cargo +stable-x86_64-pc-windows-gnu ndk -t arm64-v8a -P 28 --link-builtins build --release -p phoenix-loader`,
    then `cargo build --release -p patcher`.
- iOS. `.github/workflows/ios.yml` builds `libphoenix.dylib` for `aarch64-apple-ios` on macOS,
  since the SDK ships only with Xcode.
- macOS. `cargo build --release -p phoenix-loader` builds `target/release/libphoenix.dylib` for
  Desktop Phoenix. Install it with `RUBY_SITE=http://127.0.0.1:<port> sh scripts/ruby-macos.sh`
  against a local folder serving `version.json` and `plugins/libphoenix-macos-arm64.dylib`, or start
  the copy it makes by hand (below). `sigscan` builds natively on Apple silicon.
- Plugins. `node web/tools/bundle-plugins.ts` (set `ESBUILD` if esbuild is not on `PATH`).

## Fast loop

1. Look up Phoenix internals in `docs/phoenix-356207.md` or `.json` first. After a Discord update,
   regenerate them with `python scripts/extract-schema.py <dir with the splits>`.
2. `.\scripts\dev.ps1 -Test` runs `cargo test -p phoenix-ui-mobile -p phoenix-ui-desktop -p phoenix-wire` on the host in
   under a second.
3. Turn on the debug shell once on the phone. `dev.ps1` forwards port 4242. Then:
   - `-HotReload <plugin>` reloads a plugin in about 200 ms without rebuilding the APK.
   - `-Eval "<js>"` runs a snippet, like `phoenix.ui.root()`.
   - `phoenix.ui.mount`, `phoenix.bridge.invoke`, `phoenix.discord.onRequest` and `onResponse`
     let a UI or API patch be prototyped in JS. Move the finished primitive into Rust.
   - `-Logs`, `-Ops [filter]`, `-Events [filter]` and `-Tree [node]` show the console, decoded
     UI-op batches, view and host events, and the mirrored view tree. `-Clear` empties the ring
     buffers before reproducing something.
4. `.\scripts\dev.ps1` builds with the `dev-fast` profile (about 15 s incremental), patches,
   installs and launches. `-SkipBuild` only rebundles plugins and repatches. `-Release` makes a
   release build.
5. Desktop Phoenix. `.\scripts\dev.ps1 -Desktop` builds the DLL, copies the plugins and restarts
   Discord through the injector. Every debug-shell switch above takes `-Desktop` and skips adb.
   Desktop has no UI-op stream, so `-Ops` and `-Tree` stay Android-only. Reverse it with:
   - `.\scripts\dev.ps1 -Sigscan check|strings|refs|dis ...` on the `Discord.exe` file. A panic
     message or event name usually leads to the function that uses it.
   - `phoenix.native` in `-Eval` (the console only, never plugins): `strings`, `refs`, `fn`, `dis`,
     `scan`, `read`, `readU64`, `readStr`, and `hook(addr, limit)` / `unhook(id)` / `hooks()`.
     A hook logs each call's `rcx rdx r8 r9`, the bytes behind them and the caller to `-Events`.
     Addresses print as RVAs like `+0x7711630`, the same `sigscan` prints.
   - `-Events` also shows every gateway event by name and size and Discord's API requests by path.
6. Keep agent/subagent turns fast:
   - Iterate with `dev-fast` (`.\scripts\dev.ps1 -Desktop`, no `-Release`). Only run `-Release` once at the end before committing or shipping.
   - Batch reads, edits, and shell commands (`test && build && launch` in one command); avoid one-off `grep`/`sed`/`fmt` turns.
   - Use compiled `sigscan.exe` (`check`, `strings`, `refs`, `dis`) or `phoenix.native` for binary lookups — never scan the 515 MB `Discord.exe` `.text` section with Python loops/regexes.
   - Reserve high-effort subagents for hard reverse-engineering only; use `medium` effort / `fastMode` for routine edits and debugging, and pass exact file paths + build env in the brief.

The debug shell answers anyone on the phone's loopback, and `/eval` runs JS with the Discord
session. Leave it off outside development.

## Versions

`YEAR.MAJOR.MINOR` (`2026.1.0`), set once in `[workspace.package]` of `Cargo.toml`. Major updates bump the
second number and reset the third to 0; minor updates bump the third, up to 999. Bump it in the commit
that ships the change, and let `cargo check` refresh `Cargo.lock` (CI builds with `--locked`).

## Code style

- Rust is edition 2024, three-space indent, width 100, pinned in `.rustfmt.toml`. Run
  `rustfmt --edition 2024 <files you touched>`. Never run `cargo fmt`, it reformats the whole tree.
- Clippy must stay clean on the host, `aarch64-linux-android` and `armv7-linux-androideabi`.
  Never widen the workspace lint list. Scope an `#[expect(lint, reason = "...")]` to the one function that needs it.
- Errors use `flub` in libraries and `misstep` in binaries. Untrusted input, including anything
  read from Phoenix, returns an error and never panics. A panic inside Discord's process kills the
  app.
- Code that finds something in Discord's binaries lives in `crates/sigs` and returns `Found`, so
  `sigscan check` and the `check` workflow cover it on every new build. Never fall back to an
  offset seen on one build: when it does not decode, leave that feature off.
- Return named enums over nested `Option`s. Use serde structs over raw `serde_json::Value`.
- Comments say why, and only when the reason is not in the code: a host quirk, a measured number,
  a version. No comments that restate the code or narrate history.
- Commit subjects read `area: lowercase imperative`, like `runtime: add select settings`. Bodies
  are optional and explain why.

## Testing on a phone

- Install over the existing `com.discord` with `adb install-multiple -r` so the login survives.
- Logs: `adb logcat -s phoenix` (`-s FridaPlugins` for Frida). Fail fast. When waiting on a build
  or a launch, match `error[E`, `error:` and panics as well as the success line, and never block
  minutes on log lines that need live chat traffic.
- Open settings with
  `adb shell am start -a android.intent.action.VIEW -d discord://app/settings com.discord`.
- Find views with `uiautomator dump` by resource id, which is Phoenix's accessibility id. Ours are
  `phoenix-<title>`. Tap by the dumped bounds, never by guessed coordinates. A blind tap once
  opened a reply draft in a real channel.
- Owners deploy to the same phone while you work. When expected logs vanish, check the pid and the
  startup log to see whose build runs before debugging. Ask before installing over theirs.
- Never enter passwords. Remove test plugins and files you add.
- Git Bash on Windows turns `/word` into a path in adb arguments. Set `MSYS_NO_PATHCONV=1` and
  pass local paths through `cygpath -w`.

## Host facts

Read the decompiled host before guessing or bisecting.
`jadx -q -j 16 --no-res -d out <base apk>` decompiles the Java side. The native host is
`lib/arm64-v8a/liblibdiscord_wasm_host.so`, 97 MB and stripped, with JNI exports visible to
`nm -D`. The armeabi-v7a split's copy is 68 MB of Thumb-2, and inlines differently, so an arm64
byte pattern says nothing about arm32.

- Inline hooks. ShadowHook comes from the `sunhook-sys` crate of sunbird-tools/sunhook. On arm32
  no island fits inside the host library, so a hook rewrites the first 8 or 10 bytes of its
  target in place. Install hooks from the loader's constructor, before any thread can be inside
  the target, and pass Thumb targets with bit 0 set or ShadowHook writes an ARM jump.
- Update risks. `crates/ui-mobile/src/props.in` (props are named by index, so a new prop shifts them),
  `GuestHost.d(byte[], Function0)` (an obfuscated name) and `templates.json`. Host module,
  method, event and field numbers are read at startup by `host.rs`, so the numbers below are
  for reading frames by hand. Everything else is found by names, strings and patterns.
- Threads. Never send UI ops from the main thread, since Rust may hold the UI lock while it waits
  on Java. The settings worker sends them. Java only honors `WasmRuntime.nativeDispatch` on the
  `wasm-guest` HandlerThread, and a call from another thread is silently ignored. Queue a frame
  from any thread with `GuestHost.d([B, Function0)`, which takes the frame without its 4 zero
  header bytes. Queued frames come back through the dispatch sinks, so mark re-sent frames or they
  loop.
- Invoke frames. 16-byte header: kind u8 (1 call, 3 event, 4 batch), a zero byte, module u16,
  method u16, u16 0, call i32, args length u32. Then a tagged args record and the raw body.
  Module, method and field numbers index the name table in `hh/jb.java` (strings like
  `"Networking.request 168"`). Host events return as kind-3 frames inside kind-4 batches through
  `nativeDispatch`.
- Networking. Module 43, request 168, cancel 167, upload 169, reply event `@http` 321. Request
  fields are id 417, method 421, url 422, headers 423 (array of `[name, value]`), responseHeaders
  424. Reply fields are id 417, status 430, headers 423 (map), error 403. Java's side is a plain
  OkHttp executor and the guest sets every header, `authorization` included.
- `READY` never passes through the gateway registry on 356207. Patches that rewrite READY do not
  apply. `LIBDISCORD_REDUCERS_TABLE` is an exported second registry with 86 more events and no
  overlap.
- Motion. The host has 65536 animation slots. Ids at or above that are dropped silently. Phoenix
  allocates upward from 0, so ours come down from 65535. A reused slot keeps a dead view's spring
  until something re-springs it. Channel numbers are the cases of `mh/e0.d`. The `animate` prop
  only applies to images.
- UI text. Text and inputs take `spans` and `valueSpans`, arrays of `{text, color, weight,
  italic, bg}` pieces that concatenate to the text. Set an input with
  `forceValue {seq, text, start, end}`, applied when `seq` changes. `value` is ignored while the
  keyboard has edits in flight.
- Message list. It is virtualized. A row event's `index` is absolute, the list's `first` prop is
  the index of its first mounted child, so the row is `kids[index - first]`.
- Stores. Phoenix keeps state in `files/store/<account>/cache.sqlite`, tables of `k` and a JSON
  BLOB `v`. Message keys end in the id zero-padded to 20 digits.
- Edits. Phoenix fills the edit box from the stored message, so a hook that rewrites your own
  MESSAGE_CREATE changes what an edit posts. The runtime restores the original text in the
  composer.
- QuickJS has no `URLSearchParams`. Check any other web global before a plugin uses it.
- Discord payload shapes are documented at https://docs.discord.food/.

### macOS Desktop

- `Discord Phoenix.app` has the hardened runtime, so dyld ignores `DYLD_INSERT_LIBRARIES`.
  `ruby-macos.sh` clones it (`cp -c`, free on APFS) into
  `~/Library/Application Support/Ruby/Ruby.app`, renames it, sets `DYLD_INSERT_LIBRARIES` and
  `RUBY` in its Info.plist `LSEnvironment`, and signs it ad hoc. Discord relaunches itself with a
  bare `open -n`, which would lose a launcher's `--env`. After an update the loader rewrites
  Discord's relaunch script to open `$RUBY`, the launcher, which re-signs the newer copy. A
  second Discord hands off to a running one, so the launcher first quits any copy without
  `libphoenix` mapped.
- Patching Discord Phoenix in place was rejected. Writing into the bundle needs App Management with
  SIP on, dyld refuses to start it when the library is missing, and each update swaps the bundle.
  A Developer ID can't help, since Discord's keychain and App Group entitlements are bound to its
  team id.
- Phoenix re-execs itself once with its malloc settings and `DISCORD_DESKTOP_MALLOC_REEXEC=1`
  unless `LSEnvironment` already set them. The loader starts only in the last image. With SIP off,
  dyld kills any arm64e child (`osascript`, `nohup`) that inherits an arm64 library.
- Data lives in `~/Library/Application Support/libdiscord` (or `LIBDISCORD_DATA_DIR`), Ruby's home
  in its `ruby/`. Tokens are in the login keychain, so a copy pointed at another data directory
  still logs in to the owner's accounts.
- Logs go to the unified log: `/usr/bin/log stream --predicate 'eventMessage CONTAINS "phoenix:"'`.
  After killed launches, pass `--args -ApplePersistenceIgnoreState YES` or a "reopen windows?"
  alert stalls Discord. A `discord://-/channels/<guild>/<channel>` link opened with `open -a <copy>`
  loads a channel without input.
- The binary has no symbols. `sigscan strings`/`refs`/`flat` resolve `adrp` pairs and
  outlined stubs. lldb attaches over ssh once Developer Mode is on (`DevToolsSecurity -enable`).
  End a wait with `script dbg = lldb.debugger; threading.Timer(n, lambda:
  dbg.GetSelectedTarget().GetProcess().SendAsyncInterrupt()).start()`, then `detach`. Never set
  `process handle SIGTRAP -p true`, which kills Discord.
- `__TEXT` never takes `PROT_WRITE`. `Image::write_code` copies the page with
  `vm_protect(VM_PROT_COPY)`. The arm64 `hook_entry` overwrites 4 instructions with
  `ldr x16,#8; br x16; .quad` and refuses literal loads or branches back into them, so a target that
  is a lone `b` must be followed first.
- Calling convention. x86 passes a large return's slot first, arm64 in `x8`. gpui.rs and the
  detours are written the x86 way, so calls out go through `image::sret_caller` and detours are
  entered through `image::sret_callee`.
- `Pattern::new` panics without a literal byte, and a panic in the loader's constructor aborts
  Discord. Pass the x86 frame prologue to the arm64 `hook_entry`, which ignores it.
- Gateway registry. macOS keeps message, reaction, presence and typing events in a flat table apart
  from the domain tables, in two copies, and dispatches `MESSAGE_DELETE` through the second.
  `MESSAGE_CREATE` is in it with another handler than the domain table's, which never runs.
  `registry.rs` swaps both copies and takes the table's handler.
- Plugin pages and dialogs are AppKit windows (`ui-desktop/src/macos/panel.rs`).
- Command-Shift-I opens DevTools through an `NSEvent` local monitor that only sees keys while
  Discord is focused.
