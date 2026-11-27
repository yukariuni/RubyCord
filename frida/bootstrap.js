// Phoenix Frida bootstrap: the script frida-gadget runs inside Discord (see web/lib/frida.js).
//  - console.* -> logcat (tag FridaPlugins) and a ring buffer shown in the panel
//  - plugins live in <files>/phoenix-plugins/*.js and run at startup; the ones bundled by the website
//    (globalThis.BUNDLED_PLUGINS, prepended by the patcher) are copied there when new or changed
//  - a draggable "JS" button on every activity opens a panel: plugin toggles, add from URL, reload,
//    console and a JS prompt
//  - optional debug shell (panel toggle, off by default): JSON lines over TCP on 127.0.0.1:27043
// Each plugin runs as function(plugin) { ... }; plugin.onUnload(fn) registers cleanup for Reload.
// Reload undoes the Interceptor hooks a plugin made; Java.perform works as usual
// (calls made while the app is still starting are queued until Java can be used).
// Build: frida/build.sh -> web/plugins/frida-bootstrap.js (bundles frida-java-bridge; Frida 17 no longer ships it).
import Java from 'frida-java-bridge';

globalThis.Java = Java;

// ---- console ---------------------------------------------------------------------------------
const libc = Process.getModuleByName('libc.so');
const fn = (mod, name, ret, args) => new NativeFunction(mod.getExportByName(name), ret, args);
const logWrite = fn(Process.getModuleByName('liblog.so'), '__android_log_write', 'int', ['int', 'pointer', 'pointer']);
const TAG = Memory.allocUtf8String('FridaPlugins');
const lines = [];
let onLine = null; // set while the panel is open
const clients = new Set(); // debug shell connections

function show(x) {
  if (typeof x === 'string') return x;
  if (x instanceof Error) return x.stack || String(x);
  try { const s = JSON.stringify(x); return s === undefined ? String(x) : s; } catch (e) { return String(x); }
}
function emit(prio, args) {
  const text = args.map(show).join(' ');
  logWrite(prio, TAG, Memory.allocUtf8String(text));
  const line = (prio >= 6 ? 'E ' : prio === 5 ? 'W ' : '') + text;
  lines.push(line);
  if (lines.length > 500) lines.shift();
  if (onLine) try { onLine(line); } catch (e) { /* panel gone */ }
  for (const c of clients) send(c, { log: line });
}
console.debug = (...a) => emit(3, a);
console.log = console.info = (...a) => emit(4, a);
console.warn = (...a) => emit(5, a);
console.error = (...a) => emit(6, a);

// ---- plugin files --------------------------------------------------------------------------
const cmdline = new Uint8Array(File.readAllBytes('/proc/self/cmdline'));
const pkg = String.fromCharCode(...cmdline.subarray(0, cmdline.indexOf(0))).split(':')[0];
const uid = fn(libc, 'getuid', 'uint', [])();
const FILES = `/data/user/${Math.floor(uid / 100000)}/${pkg}/files`;
const DIR = `${FILES}/phoenix-plugins`;
const STATE = `${DIR}/plugins.json`;
const mkdir = fn(libc, 'mkdir', 'int', ['pointer', 'uint']);
const opendir = fn(libc, 'opendir', 'pointer', ['pointer']);
const readdir = fn(libc, 'readdir', 'pointer', ['pointer']);
const closedir = fn(libc, 'closedir', 'int', ['pointer']);
for (const d of [FILES, DIR]) mkdir(Memory.allocUtf8String(d), 0o700);

const pathOf = (name) => `${DIR}/${name}`;
function listPlugins() {
  const d = opendir(Memory.allocUtf8String(DIR));
  if (d.isNull()) return [];
  const out = [];
  for (let e; !(e = readdir(d)).isNull();) {
    const n = e.add(19).readUtf8String(); // struct dirent (bionic, 64-bit): d_name at 19
    if (n.endsWith('.js')) out.push(n);
  }
  closedir(d);
  return out.sort();
}

let state = { disabled: [], seeded: {} };
try { state = { ...state, ...JSON.parse(File.readAllText(STATE)) }; } catch (e) { /* first run */ }
const saveState = () => File.writeAllText(STATE, JSON.stringify(state));
const hash = (s) => { let h = 5381; for (let i = 0; i < s.length; i++) h = (h * 33 + s.charCodeAt(i)) | 0; return (h >>> 0).toString(16); };

for (const [name, code] of Object.entries(globalThis.BUNDLED_PLUGINS || {})) {
  if (state.seeded[name] === hash(code)) continue; // unchanged since the last copy: keep the user's version
  File.writeAllText(pathOf(name), code);
  state.seeded[name] = hash(code);
  console.log(`[phoenix] installed bundled plugin ${name}`);
}
saveState();

// ---- Java.perform at startup ------------------------------------------------------------------
// Plugins run inside the dlopen of Phoenix's lib, where starting the Java bridge crashes. Until our JS
// thread is up (see later() below), Java.perform/performNow calls are queued and Java.available says yes.
let javaReady = false;
const javaQueue = [];
const javaPerform = Java.perform.bind(Java);
Java.perform = (f) => (javaReady ? javaPerform(f) : javaQueue.push(f));
Java.performNow = ((orig) => (f) => (javaReady ? orig(f) : javaQueue.push(f)))(Java.performNow.bind(Java));
Object.defineProperty(Java, 'available', ((d) => ({ get() { return javaReady ? d.get.call(Java) : true; } }))(
  Object.getOwnPropertyDescriptor(Object.getPrototypeOf(Java), 'available')));

// ---- run / unload ----------------------------------------------------------------------------
const evaluate = typeof Script.evaluate === 'function' ? (name, src) => Script.evaluate(name, src) : (name, src) => (0, eval)(src);
let loaded = [];

// Interceptor's members, bound (its properties are read-only, so plugins get a copy instead of a proxy)
const interceptorApi = {};
for (const o of [Interceptor, Object.getPrototypeOf(Interceptor)])
  for (const k of Object.getOwnPropertyNames(o))
    if (k !== 'constructor' && !(k in interceptorApi)) interceptorApi[k] = typeof Interceptor[k] === 'function' ? Interceptor[k].bind(Interceptor) : Interceptor[k];

function runPlugins() {
  for (const name of listPlugins()) {
    if (state.disabled.includes(name)) continue;
    const cleanup = [];
    const plugin = { name, dir: DIR, onUnload: (f) => cleanup.push(f) };
    // the plugin sees its own Interceptor, so the hooks it makes are undone by Reload
    const hooks = {
      attach(target, cb, data) { const l = Interceptor.attach(target, cb, data); cleanup.push(() => l.detach()); return l; },
      replace(target, impl, data) { Interceptor.replace(target, impl, data); cleanup.push(() => Interceptor.revert(target)); },
    };
    const interceptor = { ...interceptorApi, ...hooks };
    loaded.push({ name, cleanup });
    try {
      evaluate(`/plugins/${name}`, `(function (plugin, Interceptor) {\n${File.readAllText(pathOf(name))}\n})`)(plugin, interceptor);
    } catch (e) {
      console.error(`${name}: ${e.stack || e}`);
    }
  }
  console.log(`[phoenix] plugins running: ${loaded.map((p) => p.name).join(', ') || 'none'}`);
}

function unloadPlugins() {
  for (const p of loaded) for (const f of p.cleanup) try { f(); } catch (e) { console.error(`${p.name} onUnload: ${e.stack || e}`); }
  loaded = [];
  Interceptor.flush();
  // ponytail: timers and Java method replacements are not tracked; plugins undo those in plugin.onUnload
}

runPlugins();
// for the console and the debug shell
globalThis.phoenix = { dir: DIR, state, list: listPlugins, loaded: () => loaded.map((p) => p.name), reload() { unloadPlugins(); runPlugins(); } };

// ---- in-app panel (Java) ---------------------------------------------------------------------
// Everything below waits for the app: the button is a TextView added to each resumed activity's
// decor view (no overlay permission). The panel is built only when the button is tapped.
// While the gadget loads us, timers fire on the main thread inside the linker (dlopen of Phoenix's lib);
// starting the Java bridge there crashes, so wait until they run on Frida's own JS thread.
(function later() {
  if (Process.getCurrentThreadId() === Process.id) return setTimeout(later, 50);
  javaReady = true;
  if (state.shell) startShell();
  if (!Java.available) return console.warn('[phoenix] no Java VM, no panel');
  for (const f of javaQueue.splice(0)) javaPerform(() => { try { f(); } catch (e) { console.error(e.stack || String(e)); } });
  Java.perform(() => { try { installUi(); } catch (e) { console.error(`[phoenix] ui: ${e.stack || e}`); } });
})();

// ---- debug shell ------------------------------------------------------------------------------
// TCP on 127.0.0.1 only. Any app on the phone can reach localhost, so the first line must be the token,
// which is only shown in the panel and in logcat (other apps can't read logcat). Then one JSON per line:
//   -> {"id": 1, "eval": "Process.id"}      <- {"id": 1, "result": "1234"} | {"id": 1, "error": "..."}
//   <- {"log": "..."} for every console line while connected. A line that isn't JSON is evaluated as is.
const SHELL_PORT = 27043;
let shell = null;
const utf8 = (s) => Array.from(unescape(encodeURIComponent(s)), (c) => c.charCodeAt(0));
const writes = new WeakMap(); // one write at a time per connection (Frida streams reject overlapping writes)
const send = (conn, obj) => {
  const next = (writes.get(conn) || Promise.resolve()).then(() => conn.output.writeAll(utf8(JSON.stringify(obj) + '\n')))
    .catch(() => clients.delete(conn));
  writes.set(conn, next);
  return next;
};

function newToken() {
  const b = Memory.alloc(12);
  fn(libc, 'getrandom', 'long', ['pointer', 'size_t', 'uint'])(b, 12, 0);
  return Array.from(new Uint8Array(b.readByteArray(12)), (x) => x.toString(16).padStart(2, '0')).join('');
}

async function evalJs(src) {
  let r = await new Promise((resolve, reject) => {
    const go = () => { try { resolve((0, eval)(src)); } catch (e) { reject(e); } };
    if (Java.available) Java.perform(go); else go();
  });
  if (r && typeof r.then === 'function') r = await r;
  return r;
}

async function serve(conn) {
  let buf = [], authed = false;
  try {
    for (;;) {
      const chunk = new Uint8Array(await conn.input.read(65536));
      if (!chunk.length) break;
      for (const byte of chunk) {
        if (byte !== 10) { buf.push(byte); continue; }
        const line = decodeURIComponent(escape(String.fromCharCode(...buf))).trim();
        buf = [];
        if (!authed) {
          if (line !== state.token) { await send(conn, { error: 'bad token' }); throw new Error('bad token'); }
          authed = true; clients.add(conn); send(conn, { ok: `phoenix debug shell, pid ${Process.id}` });
          continue;
        }
        let req;
        try { req = JSON.parse(line); } catch (e) { req = { eval: line }; }
        try { send(conn, { id: req.id, result: show(await evalJs(req.eval)) }); } catch (e) { send(conn, { id: req.id, error: e.stack || String(e) }); }
      }
    }
  } catch (e) { /* closed */ }
  clients.delete(conn);
  conn.close().catch(() => {});
}

async function startShell() {
  if (shell) return;
  if (!state.token) { state.token = newToken(); saveState(); }
  try {
    shell = await Socket.listen({ family: 'ipv4', host: '127.0.0.1', port: SHELL_PORT });
  } catch (e) { return console.error(`[phoenix] debug shell: ${e.message}`); }
  console.log(`[phoenix] debug shell on 127.0.0.1:${SHELL_PORT}, token ${state.token}`);
  for (const l = shell; shell === l;) {
    let conn;
    try { conn = await l.accept(); } catch (e) { break; }
    serve(conn);
  }
}

async function stopShell() {
  const l = shell;
  shell = null;
  for (const c of clients) c.close().catch(() => {});
  clients.clear();
  if (l) { await l.close(); console.log('[phoenix] debug shell off'); }
}

function installUi() {
  const ActivityThread = Java.use('android.app.ActivityThread');
  const app = ActivityThread.currentApplication();
  if (app === null) return setTimeout(() => Java.perform(installUi), 500);

  const J = (s) => Java.use('java.lang.String').$new(s);
  const C = (argb) => argb | 0;
  const V = (name) => Java.use(name);
  const FrameLayout = V('android.widget.FrameLayout'), FLP = V('android.widget.FrameLayout$LayoutParams');
  const LinearLayout = V('android.widget.LinearLayout'), LLP = V('android.widget.LinearLayout$LayoutParams');
  const TextView = V('android.widget.TextView'), Button = V('android.widget.Button'), EditText = V('android.widget.EditText');
  const CheckBox = V('android.widget.CheckBox'), ScrollView = V('android.widget.ScrollView');
  const GradientDrawable = V('android.graphics.drawable.GradientDrawable'), Typeface = V('android.graphics.Typeface');
  const MATCH = -1, WRAP = -2, BUTTON_TAG = J('phoenix-plugins-button');
  const setText = (v, s) => v.setText.overload('java.lang.CharSequence').call(v, J(s));
  const setHint = (v, s) => v.setHint.overload('java.lang.CharSequence').call(v, J(s));
  const JString = V('java.lang.String');
  const textOf = (v) => JString.valueOf.overload('java.lang.Object').call(JString, v.getText());

  // one listener class each; handlers are looked up by the view's identity hash
  const clicks = new Map(), checks = new Map();
  const Click = Java.registerClass({
    name: 'phoenix.plugins.Click',
    implements: [V('android.view.View$OnClickListener')],
    methods: { onClick(v) { run(clicks.get(v.hashCode())); } },
  });
  const Check = Java.registerClass({
    name: 'phoenix.plugins.Check',
    implements: [V('android.widget.CompoundButton$OnCheckedChangeListener')],
    methods: { onCheckedChanged(v, on) { run(() => checks.get(v.hashCode())(on)); } },
  });
  const run = (f) => { try { if (f) f(); } catch (e) { console.error(`[phoenix] ${e.stack || e}`); } };
  const onClick = (v, f) => { clicks.set(v.hashCode(), f); v.setOnClickListener(Click.$new()); };

  let drag = null;
  const Touch = Java.registerClass({
    name: 'phoenix.plugins.Touch',
    implements: [V('android.view.View$OnTouchListener')],
    methods: {
      onTouch(v, ev) {
        const a = ev.getActionMasked(), x = ev.getRawX(), y = ev.getRawY();
        if (a === 0) drag = { x, y, vx: v.getX(), vy: v.getY(), moved: false };
        else if (drag && a === 2) {
          if (Math.abs(x - drag.x) + Math.abs(y - drag.y) > 24) drag.moved = true;
          if (drag.moved) { v.setX(drag.vx + x - drag.x); v.setY(drag.vy + y - drag.y); }
        } else if (drag && a === 1 && !drag.moved) run(() => openPanel(Java.cast(v.getContext(), V('android.app.Activity'))));
        return true;
      },
    },
  });

  function attach(activity) {
    const decor = Java.cast(activity.getWindow().getDecorView(), FrameLayout);
    if (decor.findViewWithTag(BUTTON_TAG) !== null) return;
    const dp = activity.getResources().getDisplayMetrics().density.value;
    const b = TextView.$new(activity);
    setText(b, 'JS');
    b.setTag(BUTTON_TAG);
    b.setGravity(17);
    b.setTextColor(C(0xffffffff));
    b.setTypeface(Typeface.DEFAULT_BOLD.value);
    const bg = GradientDrawable.$new();
    bg.setShape(1);
    bg.setColor(C(0xdd5865f2));
    b.setBackground(bg);
    b.setElevation(8 * dp);
    decor.addView(b, FLP.$new(Math.round(44 * dp), Math.round(44 * dp)));
    b.setX(8 * dp);
    b.setY(decor.getHeight() > 0 ? decor.getHeight() * 0.4 : 300 * dp);
    b.setOnTouchListener(Touch.$new());
  }

  function openPanel(activity) {
    const decor = Java.cast(activity.getWindow().getDecorView(), FrameLayout);
    const dp = activity.getResources().getDisplayMetrics().density.value, px = (n) => Math.round(n * dp);
    const fg = C(0xffececf1), mut = C(0xff9a9aa6);
    const row = () => { const l = LinearLayout.$new(activity); l.setOrientation(0); l.setGravity(16); return l; };
    const grow = () => LLP.$new(0, WRAP, 1.0);
    const button = (label, f) => { const b = Button.$new(activity); setText(b, label); b.setAllCaps(false); onClick(b, f); return b; };
    const input = (hint) => {
      const e = EditText.$new(activity);
      setHint(e, hint); e.setSingleLine(true); e.setTextColor(fg); e.setHintTextColor(mut); e.setTextSize(14);
      e.setInputType(0x91); // text, visible password: no autocorrect or suggestions in code and URLs
      return e;
    };
    const label = (s) => { const t = TextView.$new(activity); setText(t, s); t.setTextColor(mut); t.setPadding(0, px(10), 0, px(2)); return t; };

    const root = LinearLayout.$new(activity);
    root.setOrientation(1);
    root.setBackgroundColor(C(0xf2111214));
    root.setPadding(px(12), px(36), px(12), px(12));
    root.setClickable(true); // keep touches away from the app below
    root.setElevation(px(16));

    const head = row();
    const title = TextView.$new(activity);
    setText(title, 'Phoenix plugins'); title.setTextColor(fg); title.setTextSize(18); title.setTypeface(Typeface.DEFAULT_BOLD.value);
    head.addView(title, grow());
    head.addView(button('Reload', () => { console.log('[phoenix] reloading plugins'); phoenix.reload(); }));
    head.addView(button('Close', () => { onLine = null; decor.removeView(root); }));
    root.addView(head);

    root.addView(label('Plugins (toggle, then Reload)'));
    const list = LinearLayout.$new(activity);
    list.setOrientation(1);
    root.addView(list);
    const refresh = () => {
      list.removeAllViews();
      const names = listPlugins();
      if (!names.length) { const t = label('none yet'); list.addView(t); }
      for (const name of names) {
        const cb = CheckBox.$new(activity);
        setText(cb, name); cb.setTextColor(fg);
        cb.setChecked(!state.disabled.includes(name));
        checks.set(cb.hashCode(), (on) => {
          state.disabled = state.disabled.filter((n) => n !== name).concat(on ? [] : [name]);
          saveState();
        });
        cb.setOnCheckedChangeListener(Check.$new());
        list.addView(cb);
      }
    };
    refresh();

    const add = row(), url = input('https://…/plugin.js');
    add.addView(url, grow());
    add.addView(button('Add', () => {
      const u = textOf(url).trim();
      setTimeout(() => Java.perform(() => { // network off the main thread
        try {
          const name = download(u);
          console.log(`[phoenix] added ${name}; tap Reload to run it`);
          Java.scheduleOnMainThread(() => { url.setText.overload('java.lang.CharSequence').call(url, J('')); refresh(); });
        } catch (e) { console.error(`[phoenix] add ${u}: ${e.message || e}`); }
      }), 0);
    }));
    root.addView(add);

    root.addView(label('Debug shell'));
    const shellBox = CheckBox.$new(activity), shellInfo = label('');
    setText(shellBox, `On, 127.0.0.1:${SHELL_PORT} (keep off when not debugging)`); shellBox.setTextColor(fg);
    const shellStatus = () => setText(shellInfo, state.shell
      ? `adb forward tcp:${SHELL_PORT} tcp:${SHELL_PORT}, then send this token as the first line: ${state.token}`
      : 'off');
    shellBox.setChecked(!!state.shell);
    checks.set(shellBox.hashCode(), (on) => {
      state.shell = on;
      if (on && !state.token) state.token = newToken();
      saveState();
      shellStatus();
      setTimeout(() => (on ? startShell() : stopShell()), 0);
    });
    shellBox.setOnCheckedChangeListener(Check.$new());
    shellInfo.setTextIsSelectable(true);
    shellStatus();
    root.addView(shellBox);
    root.addView(shellInfo);

    root.addView(label('Console'));
    const repl = row(), code = input('JS to run, e.g. Process.enumerateModules().length');
    code.setTypeface(Typeface.MONOSPACE.value);
    repl.addView(code, grow());
    repl.addView(button('Run', () => {
      const src = textOf(code);
      console.log(`> ${src}`);
      try { const r = (0, eval)(src); if (r !== undefined) console.log(show(r)); } catch (e) { console.error(e.stack || String(e)); }
    }));
    root.addView(repl);

    const scroll = ScrollView.$new(activity);
    const out = TextView.$new(activity);
    out.setTypeface(Typeface.MONOSPACE.value); out.setTextSize(11); out.setTextColor(fg); out.setTextIsSelectable(true);
    setText(out, lines.join('\n') + '\n');
    scroll.addView(out);
    root.addView(scroll, LLP.$new(MATCH, 0, 1.0));
    const bottom = () => scroll.post(scrollDown(scroll));
    decor.addView(root, FLP.$new(MATCH, MATCH));
    bottom();
    onLine = (line) => Java.scheduleOnMainThread(() => { out.append(J(line + '\n')); bottom(); });
  }

  // a Runnable that scrolls to the end once layout is done
  let Scroll = null;
  function scrollDown(scroll) {
    Scroll = Scroll || Java.registerClass({
      name: 'phoenix.plugins.Scroll',
      implements: [V('java.lang.Runnable')],
      fields: { view: 'android.widget.ScrollView' },
      methods: { run() { this.view.value.fullScroll(130); } },
    });
    const r = Scroll.$new();
    r.view.value = scroll;
    return r;
  }

  const Activity = V('android.app.Activity');
  Activity.onResume.implementation = function () {
    this.onResume();
    run(() => attach(this));
  };

  // activities already resumed before we got here
  const records = ActivityThread.currentActivityThread().mActivities.value;
  const Record = V('android.app.ActivityThread$ActivityClientRecord');
  for (let i = 0; i < records.size(); i++) {
    const r = Java.cast(records.valueAt(i), Record);
    if (!r.paused.value) { const a = r.activity.value; Java.scheduleOnMainThread(() => run(() => attach(a))); }
  }
  console.log('[phoenix] panel ready (tap the JS button)');
}

function download(u) {
  if (!/^https?:\/\/[^/]+\/./.test(u)) throw new Error('need an http(s) URL to a .js file');
  const conn = Java.cast(Java.use('java.net.URL').$new(u).openConnection(), Java.use('java.net.HttpURLConnection'));
  conn.setConnectTimeout(15000);
  conn.setReadTimeout(15000);
  const status = conn.getResponseCode();
  if (status !== 200) throw new Error(`HTTP ${status}`);
  const reader = Java.use('java.io.BufferedReader').$new(Java.use('java.io.InputStreamReader').$new(conn.getInputStream(), 'UTF-8'));
  const text = [];
  for (let l; (l = reader.readLine()) !== null;) text.push(l);
  reader.close();
  const src = text.join('\n');
  try { new Function(src); } catch (e) { throw new Error(`not valid JS: ${e.message}`); }
  let name = decodeURIComponent(u.split(/[?#]/)[0].split('/').pop()).replace(/[^\w.-]/g, '_');
  if (!name.endsWith('.js')) name += '.js';
  File.writeAllText(pathOf(name), src);
  return name;
}
