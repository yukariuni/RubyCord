// Example Frida plugin. Runs inside Discord at startup, while Phoenix's native library is being loaded.
// console.log ends up in logcat (adb logcat -s FridaPlugins). API: https://frida.re/docs/javascript-api/
const host = Process.findModuleByName('liblibdiscord_wasm_host.so');
console.log(`[example] Frida ${Frida.version} in pid ${Process.id}; host lib ${host ? `at ${host.base}, ${host.size} bytes` : 'not loaded'}`);
if (host) {
  const exports = host.enumerateExports();
  console.log(`[example] ${exports.length} exports, first few:`);
  for (const e of exports.slice(0, 5)) console.log(`[example]   ${e.type} ${e.name} @ ${e.address}`);
}
