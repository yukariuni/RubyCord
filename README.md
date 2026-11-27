# Ruby

Patches Discord Phoenix (Discord's native Rust client) on Android, iOS, Windows, Linux, and macOS so it opens without the access gate, and injects a QuickJS plugin runtime. TypeScript plugins can hook gateway events and Discord API requests/responses, add UI buttons, decorations, message actions and slash commands, and get their own settings pages inside Discord.

No root, no Xposed, and on desktop Discord's own files stay untouched.

## How to use (Windows)

1. Install Discord Phoenix and the [Rust](https://rustup.rs) toolchain.
2. Build the loader and the launcher:
   ```powershell
   $env:RUSTFLAGS = "-C target-feature=+crt-static"
   cargo build --release -p phoenix-loader -p phoenix-inject
   ```
3. Quit Discord Phoenix completely (tray icon too), then run `target\release\phoenix-inject.exe` once with no arguments.
   It installs itself to `%LOCALAPPDATA%\Ruby`, copies `phoenix.dll` from beside it, points your **Discord Phoenix** Start Menu and Desktop shortcuts at the launcher, and starts Discord with Ruby loaded.
4. From then on, open **Discord Phoenix** from the Start Menu as usual. After rebuilding, run `phoenix-inject.exe` again to refresh the DLL.
5. In Discord open User Settings → **Ruby**. Tabs: Plugins, Themes, Catalog, General. Ctrl+Shift+I opens DevTools.

Notes:
- A pinned taskbar icon keeps its old target: unpin it and pin **Discord Phoenix** again from the Start Menu. If Discord's updater recreates its shortcut, run `phoenix-inject.exe` again. A `discord://` link or Windows startup entry starts Discord without Ruby.
- `phoenix-inject.exe --uninstall` removes Ruby and points the shortcuts back at `Discord.exe`. Discord's own files are never modified.
- The **Ruby (access gate only)** shortcut starts Discord without plugins, in case a Discord update breaks the runtime.
- `.\scripts\dev.ps1 -Desktop` builds, bundles the plugins and restarts Discord with Ruby in one step (see AGENTS.md).

### Plugins and themes

- Plugins are TypeScript in `plugins/*/src/index.ts`. Bundle them with `node web/tools/bundle-plugins.ts` (set `ESBUILD` if esbuild is not on `PATH`), or install one from a URL or file in Settings → Ruby → Plugins.
- There is no default theme. In the Themes tab press **Import theme** and pick a `.css` file. It is copied to `%LOCALAPPDATA%\DiscordPhoenix\ruby\themes`, turned on, and can be toggled there. Custom properties on `:root` (`--bg`, `--surface`, `--border`, `--text`, `--muted`, `--accent`, `--radius`) restyle Ruby's own pages; the DevTools page applies every rule.

### Other platforms

- **Android / iOS**: `crates/patcher` patches and signs the APK or IPA (`nu scripts/phoenix.nu` on Linux).
- **Linux / macOS**: `RUBY_SITE=<server> sh scripts/ruby-linux.sh` or `ruby-macos.sh`, against a server that hosts the loader.

Ruby has no website or update server. Update checks and the plugin catalog stay off until `ASSETS` in `crates/runtime/src/update.rs` names one.

## How it works

- **Unlock**: the access gate check in Phoenix's native binary is located by strings and code shape rather than fixed offsets and patched to allow startup.
- **Loading**: `crates/loader` builds the injected library (`libplm.so` on Android via renamed `DT_NEEDED`, a dylib on iOS, `phoenix.dll` on Windows, `libphoenix.so` via `LD_PRELOAD` on Linux, and `libphoenix.dylib` via `DYLD_INSERT_LIBRARIES` on macOS).
- **Hooks**: gateway events are hooked by walking Phoenix's event registry by name and swapping handler pointers. Mobile drives UI through Phoenix's Rust/native UI-op bridge (using [sunhook](https://github.com/sunbird-tools/sunhook) for inline hooks on Android and vtable swaps on iOS); desktop hooks GPUI builders, `libdiscord/crates/http` vtables, and native settings directly.
- **Signing**: Android builds are signed with the public key in `crates/patcher/keys/` (APK Signature Scheme v3) so updates install over each other without losing login state.

## Plugins

Plugins are TypeScript bundles that receive a global `phoenix` object. The API reference is [`crates/runtime/phoenix.d.ts`](crates/runtime/phoenix.d.ts), and [`plugins/example`](plugins/example/src/index.ts) exercises most of it:

```ts
// @description Logs incoming messages
// @version 1.0.0
// @author you

phoenix.on(phoenix.gateway.messageCreate, (message) => {
  phoenix.log.info(`<${message.author.username}> ${message.content}`);
});

phoenix.commands.register({
  name: "shrug",
  description: "Sends your text with ¯\\_(ツ)_/¯",
  // the returned text is sent in place of "/shrug ..."
  execute: (args) => `${args} ¯\\_(ツ)_/¯`.trim(),
});
```

Leading `// @` lines describe the plugin. Besides gateway hooks there are `settings`, `storage`, `ui` (buttons, message/member/header decorations, message actions, profile sections and tabs, dialogs), `commands`, `messages` (local-only messages, send interceptors), `stores` (read-only SQL over Phoenix's cache), `fetch`, and `discord` (session-authenticated API calls plus `onRequest` / `onResponse` hooks).

The bundled plugins live in `plugins/*/src/index.ts`. `node web/tools/bundle-plugins.ts` bundles them with esbuild (`web/tools/bundle-plugins.ts`).

## Repository

| Path | What |
|---|---|
| `crates/loader` | The injected library (`libplm.so`, `phoenix.dll`, `libphoenix.so`, `libphoenix.dylib`). |
| `crates/runtime` | The QuickJS plugin runtime and `phoenix` API, shared across platforms. |
| `crates/runtime-mobile` | Android and iOS side: UI-op bridge, Settings pages, and UI patches. |
| `crates/runtime-desktop` | Desktop Phoenix side: GPUI hooks, HTTP transport hooks, settings, and tray. |
| `crates/inject` | Windows launcher/installer (`ruby.exe`). |
| `crates/hook`, `crates/sigs`, `crates/sigscan` | Native hooking, build-independent binary locators, and the `sigscan` CLI. |
| `crates/ui-mobile`, `crates/wire` | Mobile (Android/iOS) UI-op and bridge frame codecs. |
| `crates/ui-desktop` | Desktop host primitives (clipboard, URLs, notifications, dashboard). |
| `crates/patcher`, `crates/patcher-web` | Mobile APK/IPA patcher CLI and its wasm build. |
| `crates/stand-in*` | Fake Phoenix gateway library for testing hooks without Discord. |
| `plugins/` | Bundled TypeScript plugins. |
| `scripts/` | Desktop installers and dev build helpers. |
| `web/` | Plugin bundler (`web/tools/bundle-plugins.ts`) and the Frida gadgets. |
| `frida/` | Frida gadget bootstrap. |

## Building

- **Android (Linux with Nix)**:

  ```sh
  nix develop
  nu scripts/phoenix.nu base.apk config.arm64_v8a.apk config.xxhdpi.apk config.en.apk
  ```

  Pass `config.armeabi_v7a.apk` in place of the arm64 split for a 32-bit phone. This builds the loader and patcher, bundles the plugins, patches the splits, and installs them with `adb`.

- **Android (Windows)**: `.\scripts\dev.ps1` (requires `cargo-ndk`, Android SDK/NDK, and a JDK).
- **Windows desktop**: `.\scripts\dev.ps1 -Desktop` for the dev loop, or `cargo build --release -p phoenix-loader -p phoenix-inject` for `phoenix.dll` and the launcher.
- **Linux desktop**: `cargo build --release -p phoenix-loader` (`target/release/libphoenix.so`).
- **macOS desktop**: `cargo build --release -p phoenix-loader` (`target/release/libphoenix.dylib`).
- **iOS**: built on macOS with Xcode for `aarch64-apple-ios` (see `.github/workflows/ios.yml`).
- **Plugins & site assets**: `node web/tools/bundle-plugins.ts` and `nu scripts/site.nu web`.


## Agentic development

If you're working on this repo with a coding agent, point it at [AGENTS.md](AGENTS.md) first. It documents the crate layout, build/debug-shell workflows, signing and privacy rules, and code style conventions.

Not affiliated with Discord.
