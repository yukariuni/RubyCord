// Bundles plugins/*/src/index.ts into web/plugins/<name>.js the way scripts/phoenix.nu does (esbuild IIFE,
// es2023, the leading `// @` lines kept as a banner) and lists them in web/plugins/index.json for the site.
// usage: node web/tools/bundle-plugins.ts   (esbuild on PATH, or ESBUILD=path/to/esbuild)
// The bundles are not committed: CI runs this for the `site` release that ruby-tools/site deploys.
// It also writes web/version.json, what the site ships, for the runtime's update check (crates/runtime/src/update.rs):
// the loader's baked version and each plugin bundle's CRC-32.
// The native artifacts next to them come from `nu scripts/site.nu web`, which CI also runs:
//   libphoenix-{arm64,armv7}.so and libshadowhook_nothing-{arm64,armv7}.so. The site job adds
//   libphoenix-ios.dylib from the macOS ios workflow.
import { spawnSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { crc32 } from 'node:zlib';

const root = fileURLToPath(new URL('../../', import.meta.url));
const out = fileURLToPath(new URL('../plugins/', import.meta.url));
const esbuild = process.env.ESBUILD ?? 'esbuild';
interface Listed {
  name: string;
  description: string;
  warning?: string;
}

const index: Listed[] = [];
for (const name of readdirSync(`${root}plugins`).sort()) {
  const entry = `${root}plugins/${name}/src/index.ts`;
  if (!existsSync(entry)) continue;
  const lines = readFileSync(entry, 'utf8').split(/\r?\n/);
  const header = lines.slice(0, Math.max(0, lines.findIndex((l) => !l.startsWith('// @')))).join('\n');
  // `// @unlisted` plugins, like the API demo, ship only in dev builds from scripts/phoenix.nu.
  if (/^\/\/ @unlisted$/m.test(header)) continue;
  const r = spawnSync(esbuild, [entry, '--bundle', '--format=iife', '--target=es2023', '--log-level=warning', `--banner:js=${header}`, `--outfile=${out}${name}.js`],
    { stdio: 'inherit' });
  if (r.status !== 0) process.exit(r.status ?? 1);
  const warning = header.match(/^\/\/ @warning (.*)$/m)?.[1] ?? '';
  index.push({
    name,
    description: header.match(/^\/\/ @description (.*)$/m)?.[1] ?? '',
    ...(warning ? { warning } : {}),
  });
}
writeFileSync(`${out}index.json`, JSON.stringify(index, null, 2) + '\n');
// crates/runtime/src/update.rs marks the version as `phoenix-version:<version>;`. An older loader has none: `0`
// is older than every build, so nobody is told to update to it.
// Without the Android loader (a desktop-only build, `scripts/dev.ps1 -Desktop`) there is no version to ship.
const so = `${out}libphoenix-arm64.so`;
if (existsSync(so)) {
  const loader = readFileSync(so, 'latin1').match(/phoenix-version:([\w.-]+);/)?.[1] ?? '0';
  const plugins = Object.fromEntries(index.map(({ name }) => [name, crc32(readFileSync(`${out}${name}.js`))]));
  writeFileSync(fileURLToPath(new URL('../version.json', import.meta.url)), JSON.stringify({ loader, plugins }, null, 2) + '\n');
} else {
  console.log('no libphoenix-arm64.so, leaving web/version.json as it is');
}
console.log(`bundled ${index.map((p) => p.name).join(', ')}`);
