# RubyCord

Patches Discord Phoenix (Discord's native Rust client) on Windows and Linux

## How to use

Every push to `main` builds the two downloads on the **latest** release (Releases page). Download one, quit Discord Phoenix, run it.

**Windows**: run `rubycord-installer.exe`. A window opens with two buttons:
- **Inject** drops the loader next to `Discord.exe` as `rubycord.dll` and patches `Discord.exe` to load it.
- **Un-Inject** restores `Discord.exe` to its exact original bytes and removes the loader.

Quit Discord Phoenix before either button (patching its exe needs it closed). The window shows whether Discord Phoenix was found and whether RubyCord is injected.
A Discord update replaces Discord.exe, so Inject again after one.

**Linux**: `chmod +x rubycord-installer-linux && ./rubycord-installer-linux`. It finds Discord Phoenix's AppImage (or pass its path as an argument) and adds a **RubyCord** entry to your app menu.

Notes:
- Windows: after a Discord update (which replaces `Discord.exe`), press Inject again.
- To update, download the new file and run it again.
- On Linux, run `./rubycord-installer-linux` and pick 1 (Inject) or 2 (Un-Inject), or pass `--uninstall`.
- To start Discord Phoenix without the plugins, in case a Discord update breaks the runtime, run `%LOCALAPPDATA%\RubyCord\rubycord.exe --no-runtime`.

### Building it yourself (Windows)

```powershell
$env:RUSTFLAGS = "-C target-feature=+crt-static"
cargo build --release -p phoenix-loader
$env:RUBYCORD_LOADER_DLL = "$PWD\target\release\phoenix.dll"
cargo build --release -p phoenix-inject
$env:RUBYCORD_LAUNCHER_EXE = "$PWD\target\release\phoenix-inject.exe"
cargo build --release -p rubycord-installer   # target\release\rubycord-installer.exe
```
`.\scripts\dev.ps1 -Desktop` builds, bundles the plugins and restarts Discord with RubyCord in one step (see AGENTS.md).

### Plugins

- Plugins are TypeScript in `plugins/*/src/index.ts`. Bundle them with `node web/tools/bundle-plugins.ts` (set `ESBUILD` if esbuild is not on `PATH`), or install one from a URL or file in Settings → RubyCord → Plugins.

RubyCord has no website or update server. Update checks and the plugin catalog stay off until `ASSETS` in `crates/runtime/src/update.rs` names one.

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
| `crates/inject` | Windows launcher/installer (`rubycord.exe`). |
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

- **Windows desktop**: `.\scripts\dev.ps1 -Desktop` for the dev loop, 
or `cargo build --release -p phoenix-loader -p phoenix-inject` for `phoenix.dll` and the launcher.
- **Linux desktop**: `cargo build --release -p phoenix-loader` (`target/release/libphoenix.so`).
- **macOS desktop**: `cargo build --release -p phoenix-loader` (`target/release/libphoenix.dylib`).
- **Plugins & site assets**: `node web/tools/bundle-plugins.ts` and `nu scripts/site.nu web`.



Not affiliated with Discord.
