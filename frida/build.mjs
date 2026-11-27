// Bundle bootstrap.js (with frida-java-bridge) into web/plugins/frida-bootstrap.js. Run via build.sh.
import { build } from 'esbuild';
import { readFile } from 'node:fs/promises';

// frida-java-bridge 7.1.0 calls GetFieldID on every class it wraps to force initialization; on array classes
// (registerClass wraps Class[] for getInterfaces()) that segfaults in ART on Android 16. Arrays need no init.
const FIX = ['ensureClassInitialized(env, classHandle);', "if (!name.startsWith('[')) ensureClassInitialized(env, classHandle);"];
const patchBridge = {
  name: 'patch-java-bridge',
  setup(b) {
    b.onLoad({ filter: /frida-java-bridge[\\/]lib[\\/]class-factory\.js$/ }, async (args) => {
      const src = await readFile(args.path, 'utf8');
      if (!src.includes(FIX[0])) throw new Error('frida-java-bridge changed: re-check the array-class fix');
      return { contents: src.replace(FIX[0], FIX[1]), loader: 'js' };
    });
  },
};

await build({
  entryPoints: ['bootstrap.js'],
  bundle: true,
  format: 'iife',
  platform: 'neutral',
  mainFields: ['module', 'main'],
  alias: { buffer: '@frida/buffer' },
  minifyWhitespace: true,
  minifySyntax: true,
  legalComments: 'none',
  plugins: [patchBridge],
  outfile: '../web/plugins/frida-bootstrap.js',
  logLevel: 'info',
});
