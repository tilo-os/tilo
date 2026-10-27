// Collaboration end to end: native macOS apps, headless Chrome pages and a
// host agent in one room of a local `tilo-relay dev` host.
//
// For each app it starts the native shell in the room (TILO_COLLAB_HOST), two
// Chrome pages (two certified devices), later a third, and an agent spawned
// from a page, then checks:
//   - every side opens the same record (the native seed shows in the pages,
//     the pages name the same record);
//   - edits cross native -> browser, browser -> native and browser <-> browser,
//     with the time until the other side shows them;
//   - named cursors and carets cross both ways;
//   - a page that joins late shows everything written before;
//   - the agent's edit shows on native and in the pages, with its cursor;
//   - a reloaded page keeps its device and room membership: no new
//     certification, the room's content, and its edits still reach the others.
// It writes summary.json and summary.md under --out and stops everything it
// started.
//
// The native side is observed and edited from outside the process, through
// the macOS accessibility API (collab-e2e-ax.swift, compiled with swiftc on
// first use): the text the window shows, AXValue edits and caret moves. Its
// pointer moves come from a TILO_NATIVE_SCENARIO timeline. The terminal that
// runs this needs Accessibility permission (System Settings > Privacy).
//
// usage: node collab-e2e.mjs --bundle DIR [--apps notes,sheets,scratchpad]
//          [--out DIR] [--relay BIN] [--bin-dir DIR] [--no-agent] [--build]
//   --bundle   build.sh --bundle desktop output (with --build and no
//              --bundle: built into OUT/bundle with --profile dev)
//   --relay    tilo-relay with features web,agents (default: target/release,
//              else target/debug)
//   --bin-dir  tilo-runtime-mcp and the app binaries (default: target/debug)
//   --build    cargo build the relay, runtime and apps first
// Exits 1 when a check fails. macOS only.
import {execFileSync, spawn, spawnSync} from 'node:child_process';
import {createReadStream, existsSync, mkdirSync, readFileSync, statSync, writeFileSync, chmodSync, openSync} from 'node:fs';
import {createServer} from 'node:http';
import {createServer as netServer} from 'node:net';
import {createInterface} from 'node:readline';
import {dirname, extname, join, normalize, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '../../../..');
const argv = process.argv.slice(2);
const option = (name, fallback) => { const i = argv.indexOf(name); return i < 0 ? fallback : argv[i + 1]; };
const flag = name => argv.includes(name);
const stamp = new Date().toISOString().replace(/[-:]/g, '').slice(0, 15);
const out = resolve(option('--out', join(repo, 'target/collab-e2e', stamp)));
const binDir = resolve(option('--bin-dir', join(repo, 'target/debug')));
const withAgent = !flag('--no-agent');
const appNames = option('--apps', 'notes,sheets,scratchpad').split(',').filter(Boolean);
mkdirSync(out, {recursive: true});

const t0 = Date.now();
const logLines = [];
const log = (...parts) => {
  const line = `+${((Date.now() - t0) / 1000).toFixed(1)}s ${parts.join(' ')}`;
  logLines.push(line);
  console.log(line);
};

// What the native window shows is read through accessibility; what a page
// shows through its accessibility tree (the DOM mirror of the frame's
// semantics). `nativeEdit` finds the AX node native writes in (AXValue);
// an app whose editor does not take AXValue writes through its input
// timeline instead (`nativeTyping`). `webEdit` finds the semantics node a
// page types in; `slot` numbers the writers (1 page A, 2 page B, 3 the
// reloaded page) for apps where each writes in its own place.
const APPS = {
  notes: {
    id: 'so.tilo.notes', bin: 'notes', dir: 'apps/notes',
    // Notes imports a markdown file named on its command line as a note.
    seedArgs: (dir, text) => { const file = join(dir, 'seed.md'); writeFileSync(file, `# ${text}\n\nBody line.\n`); return [file]; },
    nativeEdit: (nodes, seed) => nodes.find(n => n.role === 'AXTextArea' && (n.value ?? '').includes(seed)),
    webEdit: (nodes, seed) => nodes.find(n => n.bounds && `${n.name} ${n.value}`.includes(seed) && /text/i.test(n.role)) ??
      nodes.find(n => n.bounds && `${n.name} ${n.value}`.includes(seed)),
    agentTask: text => `Create a new note titled "${text}"`,
  },
  sheets: {
    id: 'so.tilo.sheets', bin: 'sheets', dir: 'apps/sheets',
    // Native types in A1 (seed) and A5 (live edit); pages in A2, A3, A4.
    nativeTyping: {seed: 'A1', live: 'A5'},
    cell: (x, y) => ({x: 56 + 108 * x + 54, y: 184 + 26 * y + 13}),
    webEdit: (nodes, _seed, slot) => nodes.find(n => n.bounds && new RegExp(`\\bA${slot + 1}\\b`).test(n.name)),
    agentTask: text => `In Sheets, edit cell "${text}"`,
  },
  scratchpad: {
    id: 'so.tilo.scratchpad', bin: 'scratchpad', dir: 'apps/scratchpad',
    nativeEdit: nodes => nodes.find(n => n.role === 'AXTextArea') ?? nodes.find(n => n.role === 'AXTextField'),
    webEdit: nodes => nodes.find(n => n.bounds && /text/i.test(n.role) && /thought|note/i.test(`${n.name} ${n.value}`)) ??
      nodes.find(n => n.bounds && /textinput|textarea|textfield|multiline/i.test(n.role)),
    agentTask: text => `In Scratchpad, edit note "${text}"`,
    // The editor fills the window: where to click when the page exposes no
    // positioned semantics yet.
    webClick: [600, 420],
  },
};
// Steps of the native input timeline (100 ms each) at which a timeline-typing
// app writes its seed and its live edit, counted from its first frame.
const SEED_STEP = 30, LIVE_STEP = Number(option('--native-live-step', 900));
for (const name of appNames) if (!APPS[name]) throw new Error(`unknown app ${name}; known: ${Object.keys(APPS).join(', ')}`);

// ---------------------------------------------------------------------------
// Processes: each in its own process group, logged under OUT, stopped at exit.

const children = [];
function detached(name, command, args, {env = {}, cwd = repo} = {}) {
  const stdout = join(out, `${name}.out`), stderr = join(out, `${name}.err`);
  const child = spawn(command, args, {
    cwd, env: {...process.env, ...env}, detached: true,
    stdio: ['ignore', openSync(stdout, 'w'), openSync(stderr, 'w')],
  });
  child.unref();
  writeFileSync(join(out, `${name}.pid`), `${child.pid}\n`);
  const entry = {name, child, stdout, stderr, exited: false};
  child.on('exit', code => { entry.exited = true; entry.code = code; });
  children.push(entry);
  return entry;
}
async function stop(entry) {
  if (!entry || entry.exited) return;
  try { process.kill(-entry.child.pid, 'SIGTERM'); } catch {}
  for (let i = 0; i < 30 && !entry.exited; i++) await sleep(100);
  if (!entry.exited) try { process.kill(-entry.child.pid, 'SIGKILL'); } catch {}
}
const read = file => (existsSync(file) ? readFileSync(file, 'utf8') : '');
async function until(fn, ms, interval = 50) {
  const end = Date.now() + ms;
  for (;;) {
    const value = await fn().catch(() => undefined);
    if (value) return value;
    if (Date.now() > end) return undefined;
    await sleep(interval);
  }
}
function freePort() {
  return new Promise((ok, fail) => {
    const server = netServer().listen(0, '127.0.0.1', () => { const {port} = server.address(); server.close(() => ok(port)); });
    server.on('error', fail);
  });
}

// ---------------------------------------------------------------------------
// Builds (opt-in).

const cargo = args => {
  log('cargo', args.join(' '));
  const env = {...process.env};
  delete env.CARGO_TARGET_DIR;
  const result = spawnSync('cargo', args, {cwd: repo, env, stdio: 'inherit'});
  if (result.status !== 0) throw new Error(`cargo ${args.join(' ')} failed`);
};
if (flag('--build')) {
  cargo(['build', '-p', 'tilo-relay', '--bin', 'tilo-relay', '--features', 'web,agents']);
  cargo(['build', '-p', 'shell', '--bin', 'tilo-runtime-mcp', ...appNames.flatMap(a => ['-p', APPS[a].bin, '--bin', APPS[a].bin])]);
}
let bundle = option('--bundle');
if (!bundle && flag('--build')) {
  bundle = join(out, 'bundle');
  const result = spawnSync('bash', [join(repo, 'crates/browser/tilo-browser-runtime/scripts/build.sh'), bundle, '--profile', 'dev', '--bundle', 'desktop'], {cwd: repo, stdio: 'inherit'});
  if (result.status !== 0) throw new Error('desktop bundle build failed');
}
if (!bundle || !existsSync(join(bundle, 'pkg/tilo_browser_runtime.js'))) throw new Error('--bundle DIR (build.sh --bundle desktop output) is required');
bundle = resolve(bundle);
const relayBin = resolve(option('--relay', [join(repo, 'target/release/tilo-relay'), join(repo, 'target/debug/tilo-relay')].find(existsSync) ?? 'tilo-relay'));
const runtimeBin = join(binDir, 'tilo-runtime-mcp');
for (const bin of [relayBin, runtimeBin, ...appNames.map(a => join(binDir, APPS[a].bin))]) {
  if (!existsSync(bin)) throw new Error(`missing ${bin}; build it or pass --build`);
}

// The accessibility client.
const axSource = join(here, 'collab-e2e-ax.swift');
const axBin = join(out, 'collab-e2e-ax');
execFileSync('swiftc', ['-O', axSource, '-o', axBin], {stdio: 'inherit'});
// A locked screen presents no native window: its apps start but never draw,
// answer accessibility or run their input timeline.
if (/"CGSSessionScreenIsLocked"\s*=\s*Yes/.test(execFileSync('ioreg', ['-n', 'Root', '-d1'], {encoding: 'utf8'}))) {
  throw new Error('the screen is locked: native windows present no frames; unlock it and run again');
}

// ---------------------------------------------------------------------------
// The host, the native runtime daemon and the page server.

const summary = {started: new Date().toISOString(), repo, revision: '', out, bundle, relay: relayBin, binDir, apps: {}};
try {
  summary.revision = execFileSync('git', ['rev-parse', '--short', 'HEAD'], {cwd: repo, encoding: 'utf8'}).trim() +
    (execFileSync('git', ['status', '--porcelain', '--untracked-files=no'], {cwd: repo, encoding: 'utf8'}).trim() ? ' (dirty)' : '');
} catch {}

const HARNESS = `<!doctype html>
<meta charset="utf-8">
<title>collab e2e</title>
<style>html,body{margin:0;height:100%;background:#111}#surface{position:relative;width:1200px;height:800px}</style>
<div id="surface"></div>
<script>
globalThis.__console = [];
for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
  const original = console[level].bind(console);
  console[level] = (...args) => {
    if (globalThis.__console.length < 5000) globalThis.__console.push(new Date().toISOString().slice(11, 23) + ' [' + level + '] ' + args.map(String).join(' ').slice(0, 800));
    original(...args);
  };
}
</script>
<script type="module">
try {
  const q = new URLSearchParams(location.search);
  const wasm = await import('./pkg/tilo_browser_runtime.js');
  const {connectPeer} = await import('./peer.mjs');
  const {openDeviceCustody, idbStorage} = await import('./device-custody.mjs');
  await wasm.default();
  const profile = q.get('profile');
  const custody = await openDeviceCustody({profile, storage: idbStorage()});
  const peer = globalThis.peer = await connectPeer({
    origin: q.get('origin'), userPublicKey: q.get('user'), label: q.get('label'), room: q.get('room'),
    custody, profile, displayName: q.get('label'), runtime: wasm,
  });
  globalThis.tiloMount = await peer.mountDesktop(document.getElementById('surface'), {apps: [q.get('app')]});
  globalThis.harnessReady = true;
} catch (error) { globalThis.harnessError = String(error?.stack ?? error); }
</script>`;
const TYPES = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.mjs': 'text/javascript', '.json': 'application/json',
  '.wasm': 'application/wasm', '.png': 'image/png', '.css': 'text/css', '.ttf': 'font/ttf', '.otf': 'font/otf', '.woff2': 'font/woff2'};
const staticPort = await freePort();
const pages = createServer((request, response) => {
  const path = normalize(decodeURIComponent(new URL(request.url, 'http://x').pathname));
  if (path === '/collab-e2e.html') { response.writeHead(200, {'content-type': TYPES['.html'], 'cache-control': 'no-store'}); response.end(HARNESS); return; }
  const file = join(bundle, path);
  if (!file.startsWith(bundle) || !existsSync(file) || statSync(file).isDirectory()) { response.writeHead(404).end(); return; }
  response.writeHead(200, {'content-type': TYPES[extname(file)] ?? 'application/octet-stream', 'cache-control': 'no-store'});
  createReadStream(file).pipe(response);
}).listen(staticPort, '127.0.0.1');
const pagesOrigin = `http://127.0.0.1:${staticPort}`;

const relayPort = await freePort();
const relayState = join(out, 'relay-state');
const relay = detached('relay', relayBin, ['dev', '--state', relayState, '--web', `127.0.0.1:${relayPort}`,
  '--allowed-origin', pagesOrigin, ...(withAgent ? ['--agents', '--agent-model', 'scripted'] : [])],
{env: {TILO_RELAY_LOG: process.env.COLLAB_E2E_RELAY_LOG ?? 'tilo_relay=info,warn'}});
const hostLine = await until(async () => read(relay.stdout).split('\n').find(l => l.startsWith('{')), 120000, 200);
if (!hostLine) throw new Error(`the relay did not start: ${read(relay.stderr).slice(-600)}`);
const host = JSON.parse(hostLine);
summary.host = {origin: host.origin, userPublicKey: host.userPublicKey, agents: withAgent};
log('host', host.origin, 'pages', pagesOrigin);

// A native profile whose identity is the dev host's user root: the host
// treats its device as one of the user's, like the pages it certifies. Each
// app gets its own profile, so its own room (a page restores its room's
// desktop, which would reopen the previous app's windows).
const NATIVE_LABEL = 'Mac native';
async function startNative(name) {
  const profile = join(out, name, 'native-profile');
  const seed = readFileSync(join(relayState, 'dev-identity.bin')).subarray(0, 32);
  const identity = join(profile, 'config', 'tilo', 'identity');
  mkdirSync(identity, {recursive: true, mode: 0o700});
  for (const dir of [profile, join(profile, 'config'), join(profile, 'config', 'tilo'), identity]) chmodSync(dir, 0o700);
  writeFileSync(join(identity, 'primary.json'), JSON.stringify({
    display_name: NATIVE_LABEL, avatar_color: [64, 128, 224],
    threshold_identity: {single_device: true, group_public_key: [...Buffer.from(host.userPublicKey, 'hex')],
      threshold: 1, total_shares: 1, share_index: 1, secret_bytes: [...seed]},
    created_at: new Date().toISOString(), schema_version: 1,
  }), {mode: 0o600});
  const daemon = detached(`${name}-daemon`, runtimeBin, ['--profile', profile, '--daemon'], {env: {
    TILO_COLLAB_HOST: host.irohAddr, TILO_RUNTIME_PROFILE: profile,
    RUST_LOG: process.env.COLLAB_E2E_DAEMON_LOG ?? 'info,iroh=warn,shell::shell::network=debug',
  }});
  if (!await until(async () => /capnp broker: bound/.test(read(daemon.stderr)), 300000, 250)) {
    throw new Error(`the runtime daemon did not start: ${read(daemon.stderr).slice(-600)}`);
  }
  const room = await until(async () => JSON.parse(read(join(profile, 'workspace.json')) || 'null')?.workspaces?.[0]?.room, 60000, 250);
  if (!room) throw new Error('the native profile has no room');
  log(`[${name}] native daemon up; room ${room}`);
  return {daemon, profile, room};
}

// ---------------------------------------------------------------------------
// Pages.

const NODES = `(() => {
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  const out = [];
  const node = n => { out.push({role: n.role, name: n.name ?? '', value: n.value ?? '', bounds: n.bounds ?? null}); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  if (root) walk(root);
  return out;
})()`;

async function openPage(name, app, room, profileName = `${name}-${app.bin}`) {
  const browser = await launch({args: ['--ignore-certificate-errors', '--use-angle=metal']});
  const tab = await browser.tab();
  await tab.send('Emulation.setDeviceMetricsOverride', {width: 1240, height: 860, deviceScaleFactor: 1, mobile: false});
  await tab.send('Accessibility.enable');
  const url = `${pagesOrigin}/collab-e2e.html?${new URLSearchParams({origin: host.origin, user: host.userPublicKey, room, label: name, profile: profileName, app: app.id})}`;
  const page = {name, browser, tab, url};
  await ready(page, () => tab.navigate(url));
  return page;
}
async function ready(page, go) {
  const start = Date.now();
  await go();
  await sleep(300);
  await page.tab.waitFor('globalThis.harnessReady || globalThis.harnessError', {timeout: 180000});
  const error = await page.tab.evaluate('globalThis.harnessError ?? null');
  if (error) throw new Error(`${page.name}: ${error}`);
  page.readyMs = Date.now() - start;
  log(page.name, 'ready in', page.readyMs, 'ms');
}
const pageText = async page => (await page.tab.send('Accessibility.getFullAXTree')).result.nodes
  .map(n => [n.name?.value, n.value?.value].filter(Boolean).join(' ')).join('\n');
async function pageNodes(page) {
  await page.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()');
  await sleep(500);
  return page.tab.evaluate(NODES);
}
async function click(page, x, y) {
  for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
    await page.tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
  }
}
async function key(page, name) {
  const code = {End: 35, Enter: 13, Escape: 27}[name];
  const event = {key: name, code: name, windowsVirtualKeyCode: code, ...(name === 'Enter' ? {text: '\r'} : {})};
  await page.tab.send('Input.dispatchKeyEvent', {type: 'keyDown', ...event});
  await page.tab.send('Input.dispatchKeyEvent', {type: 'keyUp', ...event});
}
async function typeText(page, text) {
  for (const ch of text) {
    await page.tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch});
    await page.tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
    await sleep(15);
  }
}
async function movePointer(page, ms = 2500) {
  const end = Date.now() + ms;
  for (let i = 0; Date.now() < end; i++) {
    await page.tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 500 + (i % 30) * 9, y: 300 + (i % 15) * 7});
    await sleep(80);
  }
}
// The members' pointers the desktop's frame draws.
const cursorLabels = page => page.tab.evaluate("(globalThis.tiloMount?.pointers?.() ?? []).map(p => ({label: p.label, kind: p.agent ? 'agent' : 'person'}))");
const remoteState = page => page.tab.evaluate('(globalThis.peer?.live?.remoteState() ?? []).map(r => ({label: r.participant?.label, kind: r.participant?.kind, pointer: !!r.pointer, focus: r.focus ?? null, selection: r.selection ?? null}))');
// The records the page's peer names for `appId`, from its snapshot.
async function pageRecords(page, appId) {
  const snapshot = await page.tab.evaluate('globalThis.peer?.getSnapshot() ?? null');
  const found = new Set();
  const visit = value => {
    if (!value || typeof value !== 'object') return;
    if ((value.app ?? value.appId) === appId && typeof (value.record ?? value.recordId) === 'string') found.add(value.record ?? value.recordId);
    for (const child of Object.values(value)) visit(child);
  };
  visit(snapshot);
  return [...found];
}
async function closePage(page) {
  if (!page) return;
  try { writeFileSync(join(page.dir, `${page.name}-final.png`), await page.tab.screenshot()); } catch {}
  try { writeFileSync(join(page.dir, `${page.name}-console.log`), [...page.tab.log, ...await page.tab.evaluate('globalThis.__console ?? []')].join('\n')); } catch {}
  try { writeFileSync(join(page.dir, `${page.name}-snapshot.json`), JSON.stringify(await page.tab.evaluate('globalThis.peer?.getSnapshot() ?? null'), null, 1)); } catch {}
  try { writeFileSync(join(page.dir, `${page.name}-channels.txt`), await page.tab.evaluate('globalThis.peer?.channelDiagnostics?.() ?? ""')); } catch {}
  await page.browser.close().catch(() => {});
}

// ---------------------------------------------------------------------------
// The native app.

function axClient(pid) {
  const child = spawn(axBin, [String(pid)], {stdio: ['pipe', 'pipe', 'inherit']});
  const lines = createInterface({input: child.stdout});
  const waiting = [];
  lines.on('line', line => waiting.shift()?.(JSON.parse(line)));
  // An app that does not answer accessibility (busy, or a locked screen that
  // never presents its window) must fail its checks, not stall the run: a
  // late answer is dropped with the request it belongs to.
  const ask = command => new Promise(ok => {
    let done = false;
    const answer = value => { if (!done) { done = true; ok(value); } };
    waiting.push(answer);
    child.stdin.write(`${JSON.stringify(command)}\n`);
    setTimeout(() => answer({ok: false, nodes: [], timeout: true}), 5000);
  });
  let last = [];
  return {
    async dump() { const answer = await ask({cmd: 'dump'}); last = answer.nodes ?? []; return last; },
    async text() { return (await this.dump()).map(n => [n.title, n.value, n.desc].filter(Boolean).join(' ')).join('\n'); },
    set: (node, value) => ask({cmd: 'set', i: node.i, value}),
    select: (node, loc) => ask({cmd: 'select', i: node.i, loc, len: 0}),
    focus: node => ask({cmd: 'focus', i: node.i}),
    press: node => ask({cmd: 'press', i: node.i}),
    close: () => child.kill(),
  };
}

// Pointer moves over the content area, every 100 ms for ten minutes, and for
// a timeline-typing app its two writes. Coordinates are physical pixels of
// the content view (2x on a Retina display; the 32 pt title bar is not in
// it), where the AX frames used here include the title bar.
function nativeScenario(file, app, texts) {
  const physical = ({x, y}) => ({x: x * 2, y: (y - 32) * 2});
  const steps = Array.from({length: 6000}, (_, i) => ({kind: 'hover', x: 700 + (i % 40) * 12, y: 500 + (i % 20) * 10}));
  const write = (at, cell, text) => {
    const [, col, row] = cell.match(/^([A-Z])(\d+)$/);
    const typing = [{kind: 'click', ...physical(app.cell(col.charCodeAt(0) - 65, Number(row) - 1))},
      ...[...text].map(value => ({kind: 'character', value})), {kind: 'key', key: 'enter'}];
    steps.splice(at, typing.length, ...typing);
  };
  if (app.nativeTyping) {
    write(SEED_STEP, app.nativeTyping.seed, texts.seed);
    write(LIVE_STEP, app.nativeTyping.live, texts.live);
  }
  writeFileSync(file, JSON.stringify({name: 'collab-e2e native', interval_us: 100000, steps}));
}

// Presence the native daemon received from a member with this label, since
// byte `from` of its log: {pointer, caret} flags.
function nativeSawPresence(daemon, label, from = 0) {
  const lines = read(daemon.stderr).slice(from).split('\n')
    .filter(l => l.includes('live presence from a workspace member') && (l.includes(`label=${label}`) || l.includes(`label="${label}"`)));
  const text = lines.join('\n');
  return {any: lines.length > 0, pointer: /Pointer|Cursor|pointer|cursor/.test(text), caret: /Caret|Selection|caret|selection|Focus/.test(text), sample: lines.at(-1)?.slice(0, 400)};
}

// ---------------------------------------------------------------------------
// One app.

async function runApp(name) {
  const app = APPS[name];
  const dir = join(out, name);
  mkdirSync(dir, {recursive: true});
  const nonce = Math.random().toString(36).slice(2, 7).toUpperCase().replace(/[^A-Z]/g, 'Q');
  const T = {mac: `MAC${nonce}`, web: `WEBA${nonce}`, webB: `WEBB${nonce}`, mac2: `MACTWO${nonce}`, agent: `AGENT${nonce}`, reload: `RELOAD${nonce}`};
  const result = {app: app.id, texts: T, checks: [], latencyMs: {}, notes: []};
  const check = (id, pass, detail = '', ms) => {
    result.checks.push({id, pass: !!pass, ...(ms === undefined ? {} : {ms}), detail: String(detail).slice(0, 500)});
    log(`[${name}] ${pass ? 'PASS' : 'FAIL'} ${id}${ms === undefined ? '' : ` ${ms} ms`} ${detail ? `- ${String(detail).slice(0, 200)}` : ''}`);
  };
  let native, ax, a, b, c;
  const {daemon, profile, room} = await startNative(name);
  result.room = room;
  const daemonFrom = 0;
  try {
    if (process.env.COLLAB_E2E_PAGES_FIRST) {
      a = await openPage('Browser-A', app, room); a.dir = dir;
      b = await openPage('Browser-B', app, room); b.dir = dir;
    }
    // 1. Native app in the room.
    const scenario = join(dir, 'scenario.json');
    nativeScenario(scenario, app, {seed: T.mac, live: T.mac2});
    native = detached(`${name}-native`, join(binDir, app.bin), app.seedArgs?.(dir, `Mac note ${T.mac}`) ?? [], {cwd: join(repo, app.dir), env: {
      TILO_RUNTIME_PROFILE: profile, TILO_RUNTIME_BINARY: runtimeBin, TILO_COLLAB_HOST: host.irohAddr,
      TILO_WORKSPACE_ID: room, TILO_PARTICIPATION: 'author', TILO_NATIVE_SCENARIO: scenario,
      TILO_PROFILE_OUT: join(dir, 'native-profile.jsonl'),
      XDG_DATA_HOME: join(dir, 'native-data'), RUST_LOG: 'info,iroh=warn',
    }});
    ax = axClient(native.child.pid);
    const nativeUp = await until(async () => (await ax.dump()).length > 5, 120000, 500);
    if (!nativeUp) throw new Error(`native ${name} shows no window: ${read(native.stderr).slice(-400)}`);
    log(`[${name}] native window up (pid ${native.child.pid})`);
    // When a timeline-typing app's live edit first shows natively.
    let nativeTypedAt = 0;
    const watch = app.nativeTyping && setInterval(async () => {
      if (!nativeTypedAt && (await ax.text().catch(() => '')).includes(T.mac2)) nativeTypedAt = Date.now();
      if (nativeTypedAt) clearInterval(watch);
    }, 250);
    let target;
    if (app.nativeTyping) {
      target = {role: 'input timeline', title: app.nativeTyping.seed};
    } else if (app.seedArgs) {
      target = await until(async () => app.nativeEdit(await ax.dump(), T.mac), 60000, 250);
    } else {
      target = await until(async () => app.nativeEdit(await ax.dump(), T.mac), 60000, 250);
      if (target) await ax.set(target, `Mac note ${T.mac}`);
    }
    check('native opens the record and writes', target && await until(async () => (await ax.text()).includes(T.mac), 30000),
      target ? `${target.role} ${target.title ?? ''}` : `no editable node: ${(await ax.dump()).map(n => n.role).join(',').slice(0, 300)}`);
    writeFileSync(join(dir, 'native-ax-1.json'), JSON.stringify(await ax.dump(), null, 1));

    // 2. Two pages join: the native record replays to them.
    a ??= await openPage('Browser-A', app, room); a.dir = dir;
    b ??= await openPage('Browser-B', app, room); b.dir = dir;
    const joined = Date.now();
    const seenA = await until(async () => (await pageText(a)).includes(T.mac), 60000);
    const replayMs = Date.now() - joined;
    const seenB = await until(async () => (await pageText(b)).includes(T.mac), 30000);
    check('native -> browsers (replay on join)', seenA && seenB, `A ${!!seenA} B ${!!seenB}`, replayMs);
    const [recordsA, recordsB] = [await pageRecords(a, app.id), await pageRecords(b, app.id)];
    result.records = {A: recordsA, B: recordsB};
    check('same record on every side', seenA && seenB && recordsA.some(r => recordsB.includes(r)),
      `A ${recordsA.join(',')} B ${recordsB.join(',')}; native content shows in both`);
    writeFileSync(join(dir, 'A-1-joined.png'), await a.tab.screenshot());
    writeFileSync(join(dir, 'A-1-channels.txt'), await a.tab.evaluate('globalThis.peer?.channelDiagnostics?.() ?? ""'));

    // Each phase records its own failure and the next one still runs.
    const phase = async (id, body) => {
      try { await body(); } catch (error) { check(id, false, String(error?.message ?? error)); }
    };
    const pageHas = (page, text) => until(async () => (await pageText(page)).includes(text), 30000);
    const nativeHas = text => until(async () => (await ax.text()).includes(text), 30000);
    const typeInto = async (page, text, slot) => {
      let nodes = await pageNodes(page);
      // Without the native note, later writers use the note page A made.
      let node = app.webEdit(nodes, T.mac, slot) ?? (slot > 1 ? app.webEdit(nodes, T.web, slot) : undefined);
      // Without the native note (it did not replay), a page writes in a note
      // of its own so the other checks still run.
      const create = !node && nodes.find(n => n.bounds && n.name === 'New note');
      if (create) {
        await click(page, (create.bounds.x0 + create.bounds.x1) / 2, (create.bounds.y0 + create.bounds.y1) / 2);
        await sleep(1500);
        nodes = await pageNodes(page);
        node = nodes.find(n => n.bounds && /text/i.test(n.role) && n.name !== 'Note title');
        result.notes.push(`${page.name} made its own note: the native note was not there`);
      }
      if (!node && app.webClick) node = {bounds: {x0: app.webClick[0] - 10, x1: app.webClick[0] + 10, y0: app.webClick[1] - 10, y1: app.webClick[1] + 10}};
      if (!node) throw new Error(`${page.name}: nothing to type into (${nodes.filter(n => n.bounds).map(n => `${n.role}:${n.name}`).join(' | ').slice(0, 400)})`);
      const bx = node.bounds;
      await click(page, Math.max(bx.x0 + 4, bx.x1 - 6), (bx.y0 + bx.y1) / 2);
      await sleep(300);
      if (app.nativeTyping) {
        await typeText(page, text);
        await key(page, 'Enter');
      } else {
        await key(page, 'End');
        await typeText(page, ` ${text}`);
      }
      return Date.now();
    };

    // 3. Page A types; B and native show it.
    await phase('browser A -> browser B', async () => {
      const typed = await typeInto(a, T.web, 1);
      check('browser A -> browser B', await pageHas(b, T.web), '', Date.now() - typed);
      check('browser A -> native', await nativeHas(T.web), '', Date.now() - typed);
    });

    // 4. Page B types; A and native show it.
    await phase('browser B -> browser A', async () => {
      const typed = await typeInto(b, T.webB, 2);
      check('browser B -> browser A', await pageHas(a, T.webB), '', Date.now() - typed);
      check('browser B -> native', await nativeHas(T.webB), '', Date.now() - typed);
    });

    // 5. Native edits (accessibility, or its input timeline); both pages show it.
    await phase('native -> browser A (live)', async () => {
      let nativeEdited = 0;
      if (app.nativeTyping) {
        // The timeline writes at LIVE_STEP: time the pages from when the
        // native window shows it.
        const already = (await ax.text()).includes(T.mac2);
        if (already) result.notes.push('the native live edit landed before this phase: its times are upper bounds');
        if (await until(async () => (await ax.text()).includes(T.mac2), LIVE_STEP * 100 + 60000, 50)) nativeEdited = already ? nativeTypedAt : Date.now();
      } else {
        const current = app.nativeEdit(await ax.dump(), T.mac);
        if (current) {
          const value = current.value ?? '';
          await ax.focus(current);
          const answer = await ax.set(current, `${value} ${T.mac2}`);
          nativeEdited = Date.now();
          await ax.select(current, `${value} ${T.mac2}`.length);
          if (!answer.ok) result.notes.push(`AXValue set answered ${JSON.stringify(answer)}`);
        }
      }
      const toA = nativeEdited && await pageHas(a, T.mac2);
      check('native -> browser A (live)', toA, nativeEdited ? '' : 'the native edit did not show natively', nativeEdited ? Date.now() - nativeEdited : undefined);
      const toB = nativeEdited && await until(async () => (await pageText(b)).includes(T.mac2), 15000);
      check('native -> browser B (live)', toB, '', nativeEdited ? Date.now() - nativeEdited : undefined);
    });

    // 6. Named cursors and carets.
    await phase('cursors and carets', async () => {
      await movePointer(a, 2500);
      check('cursor browser A -> browser B', await until(async () => (await cursorLabels(b)).some(m => m.label.startsWith('Browser-A')), 5000),
        JSON.stringify(await cursorLabels(b)));
      const aSeesNative = await until(async () => (await cursorLabels(a)).some(m => m.label.startsWith(NATIVE_LABEL)), 10000);
      check('cursor native -> browsers', aSeesNative && await until(async () => (await cursorLabels(b)).some(m => m.label.startsWith(NATIVE_LABEL)), 5000),
        JSON.stringify(await cursorLabels(a)));
      check('cursor browser -> native', await until(async () => nativeSawPresence(daemon, 'Browser-A', daemonFrom).pointer || undefined, 10000),
        nativeSawPresence(daemon, 'Browser-A', daemonFrom).sample);
      check('caret native -> browser', await until(async () => (await remoteState(a)).some(r => r.label === NATIVE_LABEL && (r.focus || r.selection)), 8000),
        JSON.stringify(await remoteState(a)));
      const browserCaret = await until(async () => nativeSawPresence(daemon, 'Browser-B', daemonFrom).caret || undefined, 8000);
      const browserCaretOnA = (await remoteState(a)).some(r => r.label === 'Browser-B' && (r.focus || r.selection));
      check('caret browser -> native and browser', browserCaret && browserCaretOnA, `native ${!!browserCaret}; A sees B's ${browserCaretOnA}`);
      writeFileSync(join(dir, 'B-2-cursors.png'), await b.tab.screenshot());
    });

    // 7. The agent edits; native and pages show it, with its cursor.
    if (withAgent) await phase('agent edit -> native and browsers', async () => {
      const label = 'E2E agent';
      const grant = await a.tab.evaluate(`peer.agents.requestGrant(${JSON.stringify({label, rooms: [room], apps: [app.id], expiresInSecs: 3600})})`);
      const grantId = grant?.grant ?? grant;
      const approved = await until(async () => {
        const status = await a.tab.evaluate('peer.agents.status()');
        return (status.grants ?? []).some(g => JSON.stringify(g).includes(String(grantId)) && /approved/i.test(JSON.stringify(g)));
      }, 30000, 500);
      const task = app.agentTask(T.agent);
      const spawned = approved && await a.tab.evaluate(`peer.agents.spawn(${JSON.stringify({grant: grantId, task})})`);
      const started = Date.now();
      let agentCursor = null, agentState = null;
      await until(async () => {
        if (!agentCursor) {
          const seen = (await cursorLabels(b)).find(m => m.kind === 'agent') ?? (await b.tab.evaluate('peer.agents.cursors()'))[0];
          if (seen) agentCursor = seen;
        }
        const status = await a.tab.evaluate('peer.agents.status()');
        const agent = (status.agents ?? []).find(x => JSON.stringify(x).includes(label)) ?? status.agents?.[0];
        const run = agent?.tasks?.at(-1);
        if (run && !/queued|running|planning|working/i.test(String(run.state))) agentState = run;
        return agentState && agentCursor;
      }, 120000, 150);
      result.agent = {grant: grantId, approved: !!approved, task, spawned, state: agentState, cursor: agentCursor};
      const onA = await until(async () => (await pageText(a)).includes(T.agent), 20000);
      const onB = await until(async () => (await pageText(b)).includes(T.agent), 10000);
      const onN = await until(async () => (await ax.text()).includes(T.agent), 20000);
      check('agent edit -> native and browsers', onA && onB && onN,
        `task ${JSON.stringify(agentState?.state ?? agentState)}; A ${!!onA} B ${!!onB} native ${!!onN}`, Date.now() - started);
      check('agent cursor on browsers', agentCursor, JSON.stringify(agentCursor));
      check('agent cursor reaches native', nativeSawPresence(daemon, label, daemonFrom).any, nativeSawPresence(daemon, label, daemonFrom).sample ?? 'no presence from the agent in the daemon log');
    });

    // 8. A late page shows everything written before: what page A shows
    //    now (an agent's edit may replace earlier text).
    let expected = [];
    await phase('late join sees all edits', async () => {
      const late = Date.now();
      c = await openPage('Browser-C', app, room); c.dir = dir;
      const textA = await pageText(a);
      expected = Object.values(T).filter(t => textA.includes(t));
      const lateAll = await until(async () => { const text = await pageText(c); return expected.every(t => text.includes(t)); }, 60000);
      const lateText = await pageText(c);
      check('late join sees all edits', lateAll && expected.length > 0,
        `expected ${expected.join(',') || 'nothing (page A shows none of the texts)'}; missing: ${expected.filter(t => !lateText.includes(t)).join(',') || 'none'}`, Date.now() - late);
      writeFileSync(join(dir, 'C-late.png'), await c.tab.screenshot());
    });

    // 9. Page B reloads: same device, still a member.
    await phase('reloaded page edits reach browser and native', async () => {
      const certifications = () => (read(relay.stdout).match(/certifying a browser device=Browser-B\b/g) ?? []).length;
      const before = certifications();
      const reloaded = Date.now();
      await ready(b, () => b.tab.reload());
      const shows = await until(async () => { const text = await pageText(b); return expected.every(t => text.includes(t)); }, 60000);
      check('reload keeps membership (no new certificate, content back)', before >= 1 && certifications() === before && shows && expected.length > 0,
        `certifications ${before} -> ${certifications()}; content ${!!shows} (${expected.length} texts)`, Date.now() - reloaded);
      const typed = await typeInto(b, T.reload, 3);
      const toA = await pageHas(a, T.reload);
      const toN = await nativeHas(T.reload);
      check('reloaded page edits reach browser and native', toA && toN, `A ${!!toA} native ${!!toN}`, Date.now() - typed);
      await movePointer(b, 2000);
      check('reloaded page cursor reaches browser', await until(async () => (await cursorLabels(a)).some(m => m.label.startsWith('Browser-B')), 5000),
        JSON.stringify(await cursorLabels(a)));
    });
  } catch (error) {
    result.error = String(error?.stack ?? error);
    check('run completed', false, result.error);
  } finally {
    try { writeFileSync(join(dir, 'native-ax-final.json'), JSON.stringify(await ax?.dump(), null, 1)); } catch {}
    try { execFileSync('screencapture', ['-x', '-o', '-l', windowId(native?.child.pid), join(dir, 'native-final.png')], {stdio: 'ignore'}); } catch {}
    for (const page of [a, b, c]) await closePage(page);
    ax?.close();
    await stop(native);
    await stop(daemon);
  }
  result.pass = result.checks.every(x => x.pass);
  for (const x of result.checks) if (x.ms !== undefined) result.latencyMs[x.id] = x.ms;
  return result;
}

function windowId(pid) {
  return execFileSync('osascript', ['-l', 'JavaScript', '-e', `ObjC.import('CoreGraphics');
const list = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, 0)));
const w = list.filter(w => w.kCGWindowOwnerPID == ${Number(pid)} && w.kCGWindowLayer == 0)[0];
w ? String(w.kCGWindowNumber) : '';`], {encoding: 'utf8'}).trim();
}

// ---------------------------------------------------------------------------

function markdown() {
  const ids = [...new Set(Object.values(summary.apps).flatMap(r => r.checks.map(x => x.id)))];
  const names = Object.keys(summary.apps);
  const cell = (r, id) => {
    const x = r.checks.find(y => y.id === id);
    return x ? `${x.pass ? 'pass' : '**FAIL**'}${x.ms === undefined ? '' : ` (${x.ms} ms)`}` : '-';
  };
  return [
    `# Collaboration E2E ${summary.started}`,
    '',
    `Tree ${summary.revision} (${repo}); host ${summary.host?.origin}; one room per app; agent ${withAgent ? 'scripted' : 'off'}.`,
    '',
    `| Check | ${names.join(' | ')} |`,
    `| --- | ${names.map(() => '---').join(' | ')} |`,
    ...ids.map(id => `| ${id} | ${names.map(n => cell(summary.apps[n], id)).join(' | ')} |`),
    `| **result** | ${names.map(n => (summary.apps[n].pass ? 'pass' : '**FAIL**')).join(' | ')} |`,
    '',
    'Times: until the other side showed the text, from the last key (or the accessibility edit, the agent spawn, the page load).',
    '',
    ...names.flatMap(n => summary.apps[n].checks.filter(x => !x.pass).map(x => `- ${n} / ${x.id}: ${x.detail}`)),
    '',
  ].join('\n');
}

let exitCode = 1;
const teardown = async () => {
  for (const entry of [...children].reverse()) await stop(entry);
  pages.close();
};
process.on('SIGINT', async () => { await teardown(); process.exit(130); });
try {
  for (const name of appNames) {
    log(`== ${name}`);
    summary.apps[name] = await runApp(name);
  }
  exitCode = Object.values(summary.apps).every(r => r.pass) ? 0 : 1;
} finally {
  summary.finished = new Date().toISOString();
  summary.pass = exitCode === 0;
  writeFileSync(join(out, 'summary.json'), JSON.stringify(summary, null, 2));
  writeFileSync(join(out, 'summary.md'), markdown());
  writeFileSync(join(out, 'run.log'), logLines.join('\n'));
  if (!flag('--keep')) await teardown();
  console.log(`\n${markdown()}\nsummary: ${join(out, 'summary.md')}`);
}
process.exit(exitCode);
