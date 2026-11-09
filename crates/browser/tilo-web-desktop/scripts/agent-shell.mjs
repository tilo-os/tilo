// The tilo.sh happy path, shot end to end with a real agent.
//
// One run: a local hosted Tilo (a `tilo-relay room` host: its own user root, a
// listed workspace whose door lets a person in, the records served), the site
// in front of it (the desktop bundle, with the `entry.json` a deployment
// writes and the host's routes behind it at one origin, as an ingress puts
// them), a first visit in headless Chrome, and a real agent: the code the tab
// copies is typed into the `tilo` CLI here (`tilo login <code> --site
// <origin>`, on a scratch HOME), which makes this computer a device of the
// person, and `tilo mcp` then drives the workspace's agent surface over stdio,
// the way Claude Code does, its calls a second or two apart.
//
// For each width x colour scheme it:
// 1. opens the site as a first visit, presses "Set up with your agent" and reads
//    the code out of the prompt the tab copied;
// 2. logs in with it, which the tab follows step by step until the screen says
//    the agent is in the workspace and leaves;
// 3. starts `tilo mcp` (clientInfo "Claude Code") and calls the agent surface:
//    list the apps, search the workspace, then whatever the workspace offers --
//    make a note and write in it, set a sheet cell, run a command in the
//    Terminal -- and what it cannot do is reported, not hidden;
// 4. opens who is here on the agent (the bar's People, or the Console's Agents
//    where the workspace opens one), shoots `landing` (the person's workspace
//    just after the agent connected), `presence-with-agent` (who is here,
//    pinned out over the windows, the agent in it) and `following-mid-edit`
//    (the agent's window followed, an action landing in it), and records the
//    follow through a few of the agent's actions;
// 5. writes what each step did, what the product did not do, and why it stopped
//    where it did, under OUT (`summary.json`, `run.log`, `browser/`).
//
// Two things are the harness's own, not the deployment's: the host's packet
// relay is plain http here (a deployment's ingress publishes an https one), so
// Chrome gets --webtransport-developer-mode; and the certificate is made per
// run and trusted with `TILO_TEST_CA_FILE` by the CLI.
//
// usage: node agent-shell.mjs --bundle DIR [--out DIR] [--relay BIN] [--tilo BIN]
//          [--widths 1280,390] [--themes light,dark] [--[no-]video] [--build]
//   --bundle  build.sh --bundle desktop output (with --build and no --bundle:
//             built into $CARGO_TARGET_DIR/agent-shell/bundle, --profile quick)
//   --relay   tilo-relay with features web,agents (default: target/debug, then
//             target/release, below $CARGO_TARGET_DIR)
//   --tilo    the tilo CLI (the same directories)
//   --out     the shots, recordings and the run's logs (default:
//             $CARGO_TARGET_DIR/look/agent-shell, which a build machine copies
//             back)
//   --work    what the run keeps for itself: certificates, the host's state,
//             the agent's device key, the throwaway HOMEs (default:
//             $CARGO_TARGET_DIR/agent-shell-work, which is not fetched)
//   --build   cargo build the relay and the CLI, and build the bundle
// Needs Chrome (CHROME, or Playwright's cache), node, and openssl. The
// recording is the page shot on a clock and encoded in a second tab
// (`MediaRecorder`), so no video tool has to be installed where it runs.
// Exits non-zero when the flow breaks, saying where; what the product does not
// do yet is listed, and does not fail the run.
//
// On a build machine, one command; only the shots are copied back:
//
//   CHROME=/usr/lib64/chromium-browser/chromium-browser \
//     node crates/browser/tilo-web-desktop/scripts/agent-shell.mjs --build
//
// then target/look/agent-shell/shots and .../video hold them. The bundle a run
// built stays in the builder's target dir: a later run of the same script
// passes `--bundle $CARGO_TARGET_DIR/agent-shell/bundle` and shoots again in a
// minute, with no bundle build.
import {spawn, spawnSync} from 'node:child_process';
import {randomBytes} from 'node:crypto';
import {appendFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync} from 'node:fs';
import {createServer, request as httpsRequest} from 'node:https';
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
const target = process.env.CARGO_TARGET_DIR ?? join(repo, 'target');
const out = resolve(option('--out', join(target, 'look/agent-shell')));
// What the run keeps for itself (certificates, the host's state, the agent's
// device key, throwaway HOMEs): beside the shots, so fetching the shots back
// carries none of it.
const work = resolve(option('--work', join(target, 'agent-shell-work')));
mkdirSync(work, {recursive: true, mode: 0o700});
const shotsDir = join(out, 'shots');
const videoDir = join(out, 'video');
const browserDir = join(out, 'browser');
for (const dir of [shotsDir, videoDir, browserDir]) mkdirSync(dir, {recursive: true});
const widths = option('--widths', '1280,390').split(',').map(Number).filter(Boolean);
const themes = option('--themes', 'light,dark').split(',').filter(Boolean);
const withVideo = !flag('--no-video');

const t0 = Date.now();
const logLines = [];
const log = (...parts) => {
  const line = `+${((Date.now() - t0) / 1000).toFixed(1)}s ${parts.join(' ')}`;
  logLines.push(line);
  console.log(line);
};

// ---------------------------------------------------------------------------
// Builds (opt-in): the relay with its agent features, the CLI, and the bundle.

const cargo = args => {
  log('cargo', args.join(' '));
  const env = {...process.env};
  delete env.CARGO_TARGET_DIR;
  const result = spawnSync('cargo', args, {cwd: repo, env, stdio: 'inherit'});
  if (result.status !== 0) throw new Error(`cargo ${args.join(' ')} failed`);
};
if (flag('--build')) {
  cargo(['build', '-p', 'tilo-relay', '--features', 'web,agents', '--bin', 'tilo-relay']);
  cargo(['build', '-p', 'tilo-cli', '--bin', 'tilo']);
}
let bundle = option('--bundle');
if (!bundle && flag('--build')) {
  bundle = join(target, 'agent-shell/bundle');
  log('build.sh', bundle);
  // `quick` (the wasm-dev profile): a debug module of the whole desktop is
  // past wasm-bindgen's function limit, and nothing here measures speed. A
  // bundle an earlier run built is brought up to date step by step rather
  // than made again: only what changed is compiled, bound and split.
  const kept = existsSync(join(bundle, 'pkg/tilo_browser_runtime.js'));
  const result = spawnSync('bash', [join(here, '../../tilo-browser-runtime/scripts/build.sh'), bundle,
    '--profile', 'quick', '--bundle', 'desktop', ...(kept ? ['--update'] : [])],
    {cwd: repo, stdio: 'inherit'});
  if (result.status !== 0) throw new Error('the desktop bundle build failed');
}
if (!bundle || !existsSync(join(bundle, 'pkg/tilo_browser_runtime.js'))) {
  throw new Error('--bundle DIR (build.sh --bundle desktop output) is required');
}
bundle = resolve(bundle);
const firstOf = (name, dirs) => dirs.map(dir => join(dir, name)).find(existsSync);
const relayBin = resolve(option('--relay', firstOf('tilo-relay', [join(target, 'debug'), join(target, 'release')]) ?? 'tilo-relay'));
const tiloBin = resolve(option('--tilo', firstOf('tilo', [join(target, 'debug'), join(target, 'release')]) ?? 'tilo'));
for (const bin of [relayBin, tiloBin]) if (!existsSync(bin)) throw new Error(`missing ${bin}; pass --build, or --relay and --tilo`);
// The builder's build container carries Chromium where CI's CHROME points.
if (!process.env.CHROME && existsSync('/usr/lib64/chromium-browser/chromium-browser')) {
  process.env.CHROME = '/usr/lib64/chromium-browser/chromium-browser';
}

// ---------------------------------------------------------------------------
// Processes: each in its own group, logged under OUT, stopped at the end.

const children = [];
function detached(name, command, args, {env = {}, cwd = repo, stdin = 'ignore', logTo = null} = {}) {
  const child = spawn(command, args, {
    cwd, env: {...process.env, ...env}, detached: true,
    stdio: [stdin === 'pipe' ? 'pipe' : 'ignore', 'pipe', 'pipe'],
  });
  const entry = {name, child, stdout: '', stderr: '', exited: false};
  // What a program said, kept even when it outlives the step that started it:
  // a host that stops mid-run is only explained by its own last lines.
  const keep = (which, chunk) => {
    if (!logTo) return;
    try { appendFileSync(join(logTo, `${name}.${which}.log`), chunk); } catch { /* A log is a courtesy. */ }
  };
  child.stdout.on('data', chunk => { entry.stdout += chunk; keep('out', chunk); });
  child.stderr.on('data', chunk => { entry.stderr += chunk; keep('err', chunk); });
  child.on('exit', code => { entry.exited = true; entry.code = code; keep('out', `\n[exit ${code}]\n`); });
  children.push(entry);
  return entry;
}
async function stop(entry) {
  if (!entry || entry.exited) return;
  entry.child.stdin?.end();
  try { process.kill(-entry.child.pid, 'SIGTERM'); } catch {}
  for (let i = 0; i < 50 && !entry.exited; i++) await sleep(100);
  if (!entry.exited) try { process.kill(-entry.child.pid, 'SIGKILL'); } catch {}
}
async function until(fn, ms, interval = 100) {
  const end = Date.now() + ms;
  for (;;) {
    let value;
    // A page mid-navigation answers nothing: that is not an answer yet.
    try { value = await fn(); } catch { value = undefined; }
    if (value) return value;
    if (Date.now() > end) return undefined;
    await sleep(interval);
  }
}
/** A port nothing holds, and no other of this run's: a port is free again the
 * moment it is closed, so asking twice can answer with the same one. */
const ports = new Set();
async function freePort() {
  for (;;) {
    const port = await new Promise((ok, fail) => {
      const server = netServer().listen(0, '127.0.0.1', () => {
        const {port} = server.address();
        server.close(() => ok(port));
      });
      server.on('error', fail);
    });
    if (!ports.has(port)) {
      ports.add(port);
      return port;
    }
  }
}

/** The directories an environment names, made, so a program started on it has somewhere to write. */
function homeDirs(home) {
  const dirs = {HOME: home, XDG_DATA_HOME: join(home, 'data'), XDG_STATE_HOME: join(home, 'state'),
    XDG_CONFIG_HOME: join(home, 'config'), XDG_CACHE_HOME: join(home, 'cache'), XDG_RUNTIME_DIR: join(home, 'run')};
  for (const dir of Object.values(dirs)) mkdirSync(dir, {recursive: true, mode: 0o700});
  return dirs;
}

// ---------------------------------------------------------------------------
// One certificate for both origins: the host and the site are 127.0.0.1 on two
// ports, so a deployment's `--tls files:` pair serves them both, and the CLI
// trusts it (`TILO_TEST_CA_FILE`, a test's own trust).

const tlsDir = join(work, 'tls');
mkdirSync(tlsDir, {recursive: true, mode: 0o700});
const certPath = join(tlsDir, 'cert.pem');
const keyPath = join(tlsDir, 'key.pem');
// A leaf certificate, made again each run: it is this run's own.
rmSync(certPath, {force: true});
rmSync(keyPath, {force: true});
if (!existsSync(certPath) || !existsSync(keyPath)) {
  const made = spawnSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-sha256', '-days', '13', '-nodes',
    '-keyout', keyPath, '-out', certPath, '-subj', '/CN=127.0.0.1',
    '-addext', 'subjectAltName=IP:127.0.0.1,IP:::1,DNS:localhost',
    '-addext', 'basicConstraints=critical,CA:FALSE',
    '-addext', 'keyUsage=critical,digitalSignature,keyEncipherment',
    '-addext', 'extendedKeyUsage=serverAuth'], {stdio: ['ignore', 'ignore', 'pipe']});
  if (made.status !== 0) throw new Error(`openssl could not make the test certificate: ${String(made.stderr).slice(-400)}`);
}

// ---------------------------------------------------------------------------
// The site: the desktop bundle, with the entry configuration the person's
// browser reads (`entry.json`: the home workspace, joined through its host's
// door, and the site's directory).

const TYPES = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.mjs': 'text/javascript',
  '.json': 'application/json', '.wasm': 'application/wasm', '.png': 'image/png', '.css': 'text/css',
  '.svg': 'image/svg+xml', '.ttf': 'font/ttf', '.otf': 'font/otf', '.woff2': 'font/woff2'};
let hostOrigin = '';
let entryJson = '{}';
const sitePort = await freePort();
const hostPort = await freePort();
// The relay the host runs for the pages it serves: a page has no UDP socket,
// so it reaches the workspace's live plane only through one.
const relayPort = await freePort();
const plainPort = await freePort();
const siteLog = line => {
  try { appendFileSync(join(browserDir, 'site.log'), `${new Date().toISOString()} ${line}\n`); } catch {}
};
const site = createServer({cert: readFileSync(certPath), key: readFileSync(keyPath)}, (request, response) => {
  const path = normalize(decodeURIComponent(new URL(request.url, 'https://x').pathname));
  if (path === '/entry.json') {
    const body = Buffer.from(entryJson);
    response.writeHead(200, {'content-type': 'application/json', 'cache-control': 'no-cache', 'content-length': body.length});
    response.end(body);
    return;
  }
  let file = join(bundle, path);
  if (file.startsWith(bundle) && existsSync(file) && statSync(file).isDirectory()) file = join(file, 'index.html');
  if (file.startsWith(bundle) && existsSync(file)) {
    const body = readFileSync(file);
    siteLog(`${request.method} ${path} ok`);
    response.writeHead(200, {
      'content-type': TYPES[extname(file)] ?? 'application/octet-stream',
      'cache-control': 'no-store', 'content-length': body.length,
    });
    response.end(body);
    return;
  }
  // Everything else is the host behind this site, as an ingress puts it there:
  // the page and its host answer at one origin, so a pairing link names the
  // site it was copied from.
  siteLog(`${request.method} ${path} -> the host`);
  const upstream = httpsRequest({host: '127.0.0.1', port: hostPort, path: request.url, method: request.method,
    headers: {...request.headers, host: `127.0.0.1:${hostPort}`}, rejectUnauthorized: false}, answer => {
    response.writeHead(answer.statusCode ?? 502, answer.headers);
    answer.pipe(response);
  });
  upstream.on('error', error => {
    log('site: the host did not answer', String(error.message ?? error));
    response.writeHead(502).end();
  });
  request.pipe(upstream);
});
await new Promise(ok => site.listen(sitePort, '127.0.0.1', ok));
const siteOrigin = `https://127.0.0.1:${sitePort}`;

// ---------------------------------------------------------------------------
// The host: a persistent public workspace, as a hosted Tilo runs, listing it so
// the site's door lets the person in, and serving its records, so the agent's
// actions run there.

const rootFile = join(tlsDir, 'root.hex');
const roomIdFile = join(tlsDir, 'room-id');
// A run starts the host clean: what an interrupted run left behind is a
// workspace whose owner root its secret store never got.
for (const stale of [join(work, 'room-state'), roomIdFile, rootFile]) rmSync(stale, {recursive: true, force: true});
writeFileSync(rootFile, `${randomBytes(32).toString('hex')}\n`, {mode: 0o600});
const hostHome = join(work, 'host-home');
const secretsDir = join(work, 'secrets');
mkdirSync(secretsDir, {recursive: true, mode: 0o700});
const OPERATOR_LINE = 'tilo-room-operator ';
// A host's first start seeds the workspace's apps, and a seed that lands
// before the workspace's channel keys are ready stops it: a fresh state and
// another start is the whole fix.
let started = null;
let lastHost = null;
for (let attempt = 1; started === null; attempt += 1) {
  if (attempt > 3) throw new Error(`the host did not start in 3 tries: ${lastHost?.stderr.slice(-1200) ?? ''}`);
  const host = detached('host', relayBin, ['room', '--state', join(work, 'room-state'),
    '--persistent', 'tilo', '--workspace-name', 'Tilo', '--root-file', rootFile, '--room-id-file', roomIdFile,
    '--web', `127.0.0.1:${hostPort}`, '--tls', `files:${certPath},${keyPath}`,
    '--web-plain', `127.0.0.1:${plainPort}`, '--public-origin', siteOrigin,
    '--packet-relay', `127.0.0.1:${relayPort}`,
    '--allowed-origin', siteOrigin, '--listing-description', 'A workspace for a person and their agent',
    '--default-role', 'editor', '--room-records',
    // The apps the workspace opens with: the ones the agent drives, which the
    // person's desktop shows too. The Terminal's sessions run on the host,
    // beside the workspace's records.
    '--seed-app', 'so.tilo.notes', '--seed-app', 'so.tilo.sheets', '--seed-app', 'so.tilo.chat',
    '--seed-app', 'so.tilo.terminal'],
  {env: {...homeDirs(hostHome), TILO_RELAY_LOG: process.env.AGENT_SHELL_RELAY_LOG ?? 'tilo_relay=info,warn',
    // A machine with no keychain to unlock keeps its secrets in files here.
    TILO_SECRETS_DIR: secretsDir}, stdin: 'pipe', logTo: browserDir});
  lastHost = host;
  let stopped = null;
  host.child.once('exit', code => { stopped = code; });
  const heard = await until(() => {
    const line = host.stdout.split('\n')
      .find(candidate => candidate.startsWith(OPERATOR_LINE) && candidate.includes('"started"'));
    if (line) return JSON.parse(line.slice(OPERATOR_LINE.length));
    if (stopped !== null) return {stopped: `the host stopped (exit ${stopped}): ${host.stderr.slice(-1500)}`};
  }, 240000, 200);
  if (heard?.stopped) {
    log(`host attempt ${attempt}: ${heard.stopped}`);
    await stop(host);
    rmSync(join(work, 'room-state'), {recursive: true, force: true});
    rmSync(roomIdFile, {force: true});
    continue;
  }
  if (!heard) throw new Error(`the host did not start: ${host.stderr.slice(-1200)}`);
  if (heard.room) started = heard;
  else log('host attempt', attempt, 'governs no workspace yet');
}
hostOrigin = siteOrigin;
const roomId = started.room;
if (!roomId) throw new Error(`the host governs no workspace: ${JSON.stringify(started)}`);
log('host', started.origin, 'as', hostOrigin, 'workspace', roomId);
entryJson = JSON.stringify({
  home: {name: 'Tilo', host: {origin: hostOrigin}, admission: `${hostOrigin}/peer/v1/directory/${roomId}/`},
  directory: `${hostOrigin}/peer/v1/directory`,
});

// ---------------------------------------------------------------------------
// The page, as a test drives it: its semantics (what a person can see and
// press), typed keys, and shots of the whole viewport.

const WALK = `(() => {
  const out = [];
  const rect = b => (Array.isArray(b) ? {x0: b[0], y0: b[1], x1: b[2], y1: b[3]}
    : b ? {x0: b.x0 ?? b.x ?? 0, y0: b.y0 ?? b.y ?? 0, x1: b.x1 ?? (b.x ?? 0) + (b.width ?? 0), y1: b.y1 ?? (b.y ?? 0) + (b.height ?? 0)} : null);
  const node = n => { out.push({role: n.role ?? '', name: n.name ?? '', description: n.description ?? '', value: n.value ?? '', rect: rect(n.rect ?? n.bounds)}); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  return out;
})()`;

async function semantics(tab) {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await sleep(350);
  return tab.evaluate(WALK).catch(() => []);
}
// A control is found by the name it is announced under; where a name is empty
// (a bar chip named by its glyph) its spoken description is what a reader
// hears, so that is looked at too. A `role` narrows it to the kind of node a
// place offers: a search result, not the same word elsewhere on the desk.
const speaks = (node, pattern) => pattern instanceof RegExp
  ? pattern.test(node.name) || pattern.test(node.description)
  : node.name === pattern || node.description === pattern;
const matching = (nodes, pattern, role = null) => nodes.find(node => speaks(node, pattern) && (!role || role.test(node.role)));
const names = nodes => nodes.map(node => node.name || node.description).filter(Boolean).slice(0, 48).join(' | ');
async function press(tab, pattern, {what = null} = {}) {
  const node = matching(await semantics(tab), pattern);
  if (!node?.rect) return null;
  const {x0, y0, x1, y1} = node.rect;
  const x = (x0 + x1) / 2, y = (y0 + y1) / 2;
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
  await tab.send('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: 'left', clickCount: 1});
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: 'left', clickCount: 1});
  if (what) log(`pressed ${what}: ${node.name || node.description}`);
  return node;
}
async function key(tab, name, code, keyCode, modifiers = 0) {
  await tab.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', key: name, code, windowsVirtualKeyCode: keyCode, modifiers});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: name, code, windowsVirtualKeyCode: keyCode, modifiers});
}
async function typeText(tab, text) {
  for (const ch of text) {
    await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
    await sleep(30);
  }
}
/** What the page's own search or a menu left covering the desk, closed: a shot
 * is of the desk, not of the box that was open over it. */
async function dismissOverlays(tab) {
  const covering = /search apps, windows, documents|command palette|^results$|type to search/i;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const nodes = await semantics(tab);
    if (!nodes.some(node => covering.test(node.name))) return;
    await key(tab, 'Escape', 'Escape', 27);
    await sleep(400);
  }
}

/**
 * `name`'s shot of the whole viewport, once what covers the desk is closed and
 * what it is of is on screen: `expect` names the subject, by the same names a
 * reader finds it under. A shot of something else is a failed check, not a
 * picture nobody looked at.
 */
async function shot(tab, combo, name, {expect = null} = {}) {
  await dismissOverlays(tab);
  if (expect) {
    const nodes = await semantics(tab);
    const missing = expect.filter(pattern => !nodes.some(node => speaks(node, pattern)));
    check(`${combo}: ${name} shows ${expect.map(String).join(' and ')}`, missing.length === 0,
      missing.length ? `not on screen: ${missing.map(String).join(', ')}; the desk shows ${names(nodes)}` : '');
  }
  const png = await tab.screenshot();
  if (!png) throw new Error(`${combo}: the page answered no screenshot for ${name}`);
  writeFileSync(join(shotsDir, `${combo}-${name}.png`), png);
  log(`${combo}: shot ${name}`);
}

/** The mean luminance (0 dark … 1 light) of what `tab` shows, read in a page
 * of its own: whether a shot of the shell is a light one or a dark one. */
async function luminance(scratch, tab) {
  const shot = await tab.send('Page.captureScreenshot', {format: 'png', captureBeyondViewport: false});
  if (!shot.result?.data) return null;
  return scratch.evaluate(`new Promise(resolve => {
    const image = new Image();
    image.onload = () => {
      const canvas = document.createElement('canvas');
      canvas.width = 96;
      canvas.height = Math.max(1, Math.round(96 * image.height / image.width));
      const context = canvas.getContext('2d');
      context.drawImage(image, 0, 0, canvas.width, canvas.height);
      const {data} = context.getImageData(0, 0, canvas.width, canvas.height);
      let sum = 0;
      for (let i = 0; i < data.length; i += 4) sum += 0.2126 * data[i] + 0.7152 * data[i + 1] + 0.0722 * data[i + 2];
      resolve(sum / (data.length / 4) / 255);
    };
    image.onerror = () => resolve(null);
    image.src = "data:image/png;base64,${shot.result.data}";
  })`);
}

// ---------------------------------------------------------------------------
// The agent: `tilo login` with the code the tab copied, then `tilo mcp` over
// stdio, called the way an agent calls it.

const agentHome = join(work, 'agent');
const deviceDir = join(agentHome, 'device');
const agentEnv = {...homeDirs(agentHome), TILO_DEVICE_DIR: deviceDir, TILO_TEST_CA_FILE: certPath, TILO_SITE: siteOrigin};
mkdirSync(deviceDir, {recursive: true, mode: 0o700});

/** `tilo login <code> --site <origin> --agent "Claude Code"`: the prompt's command. */
async function login(code) {
  const entry = detached('login', tiloBin, ['login', code, '--site', siteOrigin, '--agent', 'Claude Code'],
    {env: agentEnv, logTo: browserDir});
  const done = await until(() => entry.exited || undefined, 240000, 200);
  if (!done) { await stop(entry); throw new Error(`tilo login ${code} did not finish: ${entry.stdout.slice(-500)}${entry.stderr.slice(-500)}`); }
  if (entry.code !== 0) throw new Error(`tilo login failed (exit ${entry.code}): ${entry.stdout.slice(-800)}${entry.stderr.slice(-800)}`);
  return entry;
}

/** An MCP client on `tilo mcp`, as the agent's own client is. */
function mcpClient() {
  const child = spawn(tiloBin, ['mcp'], {env: {...process.env, ...agentEnv}, stdio: ['pipe', 'pipe', 'pipe']});
  const waiting = new Map();
  let id = 0, said = '', closed = false;
  createInterface({input: child.stdout}).on('line', line => {
    said += `${line}\n`;
    let answer;
    try { answer = JSON.parse(line); } catch { return; }
    const resolve = waiting.get(answer.id);
    if (resolve) { waiting.delete(answer.id); resolve(answer); }
  });
  child.stderr.on('data', chunk => { said += chunk; });
  child.on('exit', () => {
    closed = true;
    for (const [key, resolve] of waiting) { waiting.delete(key); resolve({error: {message: `tilo mcp stopped: ${said.slice(-400)}`}}); }
  });
  const send = (message, timeoutMs = 180000) => new Promise((resolve, reject) => {
    if (closed) { reject(new Error(`tilo mcp is not running: ${said.slice(-400)}`)); return; }
    const requestId = ++id;
    waiting.set(requestId, resolve);
    child.stdin.write(`${JSON.stringify({jsonrpc: '2.0', id: requestId, ...message})}\n`);
    setTimeout(() => {
      if (!waiting.has(requestId)) return;
      waiting.delete(requestId);
      reject(new Error(`tilo mcp did not answer ${message.method} in ${timeoutMs / 1000}s: ${said.slice(-400)}`));
    }, timeoutMs).unref?.();
  });
  return {
    child,
    said: () => said,
    async initialize() {
      const answer = await send({method: 'initialize', params: {
        protocolVersion: '2025-06-18', capabilities: {},
        clientInfo: {name: 'Claude Code', version: '2.1.0'},
      }});
      if (answer.error) throw new Error(`initialize: ${answer.error.message}`);
      child.stdin.write(`${JSON.stringify({jsonrpc: '2.0', method: 'notifications/initialized'})}\n`);
      return answer.result;
    },
    async call(name, args = {}) {
      const answer = await send({method: 'tools/call', params: {name, arguments: args}});
      if (answer.error) return {failed: true, text: answer.error.message ?? JSON.stringify(answer.error)};
      const result = answer.result ?? {};
      const text = (result.content ?? []).map(part => part.text ?? '').join('\n').trim();
      return {failed: result.isError === true, text};
    },
    async close() {
      child.stdin.end();
      const done = await until(() => closed || undefined, 20000, 100);
      if (!done) child.kill('SIGTERM');
    },
  };
}

/** POST `body` as JSON to `url`, trusting this run's certificate (a test's own). */
async function postJson(url, body) {
  return new Promise(ok => {
    const request = httpsRequest(url, {method: 'POST', headers: {'content-type': 'application/json'},
      rejectUnauthorized: false}, response => {
      let text = '';
      response.on('data', chunk => { text += chunk; });
      response.on('end', () => ok({status: response.statusCode, body: text.slice(0, 400)}));
    });
    request.on('error', error => ok({status: 0, body: String(error.message ?? error)}));
    request.end(JSON.stringify(body));
  });
}

/** The lines of a `tilo_ls` answer, without its cursor line. */
const listingLines = text => text.split('\n').map(line => line.trim())
  .filter(line => line && !/^cursor|^next:|^…/i.test(line));
/** A listing's rows: each names its app, then two spaces and what it is. What
 * the answer says in sentences ("none: no app runs here", "the person decides
 * which apps an agent may open") is not a row. */
const listingRows = text => listingLines(text).filter(line => /\S {2,}\S/.test(line));
/** Every `path#action` an answer names. */
const actionPaths = text => [...new Set(text.match(/[\w.-][\w .\/-]*#[\w-]+/g) ?? [])];
const firstAction = (text, wanted, unwanted = /delete|remove|clear|reset/i) =>
  actionPaths(text).find(path => wanted.test(path) && !unwanted.test(path));

/**
 * What the agent does next, discovered from the workspace as an agent would:
 * its apps, then each app's own actions. Required: writing in the note the
 * person has open. Optional: a command in the Terminal, where the workspace
 * has it (a workspace is its person's: it has what it has).
 */
async function planActions(agent, apps) {
  const ask = async (tool, args) => {
    const answer = await agent.call(tool, args);
    log(`agent ${tool} ${JSON.stringify(args)} -> ${answer.failed ? 'failed' : 'ok'} ${answer.text.slice(0, 600)}`);
    return answer;
  };
  const steps = [];
  const app = pattern => apps.find(name => pattern.test(name));
  const ACTION = /#([\w-]+)\s*$/;
  // The desktop's own service answers without an app: asking the person's
  // page what it shows is an action like any other, and the first one an
  // agent does here.
  const desktop = await ask('tilo_ls', {path: 'desktop'});
  const windows = firstAction(desktop.text, /#(list-windows|windows)/i) ?? 'desktop#list-windows';
  steps.push({label: 'listing the windows on the person\'s screen', tool: 'tilo_do', args: {path: windows}});
  const notes = app(/^notes/i);
  if (notes) {
    // The note the person has open: the app's own listing names it, and the
    // agent picks the first, as a person reading the list would.
    const listed = await ask('tilo_ls', {path: notes});
    let note = (listingRows(listed.text)[0] ?? '').split(/\s{2,}|\s#/)[0].trim();
    if (note && !note.includes('/')) note = `${notes}/${note}`;
    if (note) {
      const noteActions = await ask('tilo_ls', {path: note, kind: 'actions'});
      const write = (firstAction(noteActions.text, /#(append|write|insert|add-text|set-text|insert-text|type|edit)/i)
        ?? `${note}#append-paragraph`).match(ACTION)?.[1] ?? 'append-paragraph';
      // One invocation writing three paragraphs: the person following sees
      // the text land line by line, which is what `following-mid-edit` catches.
      steps.push({label: 'writing in the note', tool: 'tilo_do', args: {calls: [
        {path: `${note}#${write}`, args: {text: 'Launch notes, from Claude Code.'}},
        {path: `${note}#${write}`, args: {text: 'Each line lands in the workspace as I write it.'}},
        {path: `${note}#${write}`, args: {text: 'Following me puts this window in front of you.'}},
      ]}});
      steps.push({label: 'writing the next paragraph', tool: 'tilo_do',
        args: {path: `${note}#${write}`, args: {text: 'A second invocation, a moment later.'}}});
      steps.push({label: 'reading the note back', tool: 'tilo_read', args: {path: note}});
    } else {
      log('agent: the Notes app lists no note to write in');
    }
  }
  const terminal = app(/^terminal/i) ?? app(/^ush/i);
  if (terminal) {
    const listed = await ask('tilo_ls', {path: terminal});
    const run = firstAction(listed.text, /#(run|exec|command|type|send)/i);
    if (run) {
      steps.push({label: 'running a command in the Terminal', tool: 'tilo_do',
        args: {path: run, args: {command: 'echo hello from claude code'}}});
    }
  }
  // The shot of the follow mid-edit comes while the first write is in flight.
  const at = steps.findIndex(step => /writing/.test(step.label));
  steps.shootAt = at < 0 ? steps.length - 1 : at;
  return steps;
}

// ---------------------------------------------------------------------------
// Following the agent, as a person does: the palette finds them by name and
// opens the panel on them, where following is one step; the panel also pins
// out into the overlay that stays on screen.

/** The roster, open: the top bar's people button ("Everyone here"), else the
 * palette's people search. A press lands on the panel the button opens; one
 * that lands before the bar has settled is pressed again. */
async function openPeople(tab, label) {
  const roster = /people here now|who is here/i;
  const shown = () => until(async () => (await semantics(tab)).some(node => roster.test(node.name) || roster.test(node.description)), 2500);
  // Who is here, as this desktop offers it: the bar's button beside the faces
  // of everyone here, or the Console's Agents where the workspace opens one.
  for (const pattern of [/^everyone here$/i, /^people$/i, /^agents$/i]) {
    for (let attempt = 0; attempt < 2; attempt += 1) {
      if (!await press(tab, pattern, {what: 'the roster'})) break;
      if (await shown()) return null;
    }
  }
  const mac = await tab.evaluate('/Mac|iPhone|iPad/.test(navigator.platform ?? navigator.userAgent ?? "")');
  await key(tab, 'k', 'KeyK', 75, mac ? 4 : 2);
  const searching = await until(async () => (await semantics(tab)).some(node => /search|type to search|search or run/i.test(node.name)), 10000);
  if (!searching) return `no People control, and the search chord opened no search: ${names(await semantics(tab))}`;
  await typeText(tab, label);
  const hit = await until(async () => matching(await semantics(tab), new RegExp(label, 'i')), 10000);
  if (!hit) return `typing ${label} in search found nothing: ${names(await semantics(tab))}`;
  await key(tab, 'Enter', 'Enter', 13);
  await sleep(1500);
  return null;
}

async function followAgent(tab, label) {
  const opened = await openPeople(tab, label);
  if (opened) return opened;
  const panel = await until(async () => {
    const nodes = await semantics(tab);
    return nodes.some(node => /people here now|who is here/i.test(node.name)) ? nodes : undefined;
  }, 10000);
  if (!panel) return `the roster did not open: ${names(await semantics(tab))}`;
  const follow = new RegExp(`^follow ${label}`, 'i');
  if (!matching(panel, follow) && !matching(panel, /^follow$/i)) return `no Follow for ${label}: ${names(panel)}`;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    // Following is the banner's business: the control says so once it took.
    if (await press(tab, follow, {what: `following ${label}`}) ?? await press(tab, /^follow$/i, {what: `following ${label}`})) {
      const following = await until(async () => (await semantics(tab)).some(node => new RegExp(`stop following ${label}|following ${label}`, 'i').test(node.name)), 3000);
      if (following) return null;
    } else {
      return `the Follow control for ${label} has no bounds`;
    }
  }
  return `following ${label} did not take`;
}

async function pinOverlay(tab) {
  // The pin is announced as "Keep on screen"; its description is the longer
  // "Keep who is here on screen" a reader hears beside it.
  const pin = /^(keep on screen|keep who is here on screen)$/i;
  let pinned = await press(tab, pin, {what: 'pinning who is here'});
  if (!pinned) {
    // The roster closed in the meantime: open it again, then pin.
    await openPeople(tab, 'Claude Code');
    pinned = await press(tab, pin, {what: 'pinning who is here'});
  }
  if (!pinned) return `no "Keep on screen" control: ${names(await semantics(tab))}`;
  await sleep(1500);
  return null;
}

// ---------------------------------------------------------------------------
// The note the agent writes in. A person's own workspace opens with the apps
// they left open; a fresh one has none, so the person opens Notes the way the
// palette does and makes the note in it.

/** The desktop's own search, opened with its chord and asked for `words`. */
async function palette(tab, words) {
  const mac = await tab.evaluate('/Mac|iPhone|iPad/.test(navigator.platform ?? navigator.userAgent ?? "")');
  for (let attempt = 0; attempt < 2; attempt += 1) {
    await key(tab, 'k', 'KeyK', 75, mac ? 4 : 2);
    const open = await until(async () => (await semantics(tab)).some(node => /search apps, windows, documents/i.test(node.name)), 8000);
    if (open) break;
  }
  await typeText(tab, words);
  // The result, not the same word elsewhere on the desk the palette covers.
  const result = /listitem|menuitem|treeitem|option|button/i;
  const hit = await until(async () => matching(await semantics(tab), new RegExp(`^${words}$`, 'i'), result), 8000);
  if (!hit) return `the palette found no ${words}: ${names(await semantics(tab))}`;
  await key(tab, 'Enter', 'Enter', 13);
  await sleep(2500);
  return null;
}

async function openNote(tab) {
  // A window of Notes with a note in it is what following needs: it is the
  // window the person's view goes to, and where the agent's writing lands.
  const withNote = async () => (await semantics(tab)).some(node => /^saved$/i.test(node.name));
  const windowed = async () => (await semantics(tab)).some(node => /^notes$/i.test(node.name));
  if (await withNote()) return null;
  if (!await windowed()) {
    // The desk's own app list first ("Open an app"), the palette after.
    if (await press(tab, /^open an app$/i, {what: 'the app list'})) {
      await press(tab, /^notes$/i, {what: 'Notes'});
      await until(windowed, 6000);
    }
    if (!await windowed()) {
      const opened = await palette(tab, 'Notes');
      if (opened) return `Notes did not open: ${opened}`;
    }
  }
  if (!await withNote() && !await press(tab, /^new note$/i, {what: 'a new note'})) {
    return `Notes offers no note to make: ${names(await semantics(tab))}`;
  }
  const noted = await until(withNote, 8000);
  if (!noted) return `Notes made no note: ${names(await semantics(tab))}`;
  await sleep(800);
  return null;
}

// ---------------------------------------------------------------------------
// The recording: the page shot on a clock, each frame drawn into a canvas in a
// second tab and encoded there (`MediaRecorder`), so the run needs no video
// tool on the machine that shot it. Frames are kept as PNGs when the browser
// refuses to encode, so a recording that failed is still in the output.

const FPS = 8;
/** Draws the frames it is given into a canvas and records it: a page of its
 * own, so nothing it does reaches the shots. */
const RECORDING_TAB = `(() => {
  const canvas = document.createElement('canvas');
  canvas.width = 2; canvas.height = 2;
  document.body.style.margin = '0';
  document.body.appendChild(canvas);
  const context = canvas.getContext('2d');
  const stream = canvas.captureStream(0);
  const track = stream.getVideoTracks()[0];
  let recorder = null;
  let frames = 0;
  globalThis.feed = frame => new Promise(resolve => {
    const image = new Image();
    image.onload = () => {
      if (canvas.width !== image.width || canvas.height !== image.height) {
        canvas.width = image.width;
        canvas.height = image.height;
      }
      context.drawImage(image, 0, 0);
      track.requestFrame();
      frames += 1;
      resolve(true);
    };
    image.onerror = () => resolve(false);
    image.src = frame;
  });
  globalThis.begin = bits => {
    recorder = new MediaRecorder(stream, {mimeType: 'video/webm;codecs=vp8', videoBitsPerSecond: bits});
    globalThis.chunks = [];
    recorder.ondataavailable = event => { if (event.data.size) globalThis.chunks.push(event.data); };
    recorder.start(1000);
    return Boolean(recorder);
  };
  globalThis.finish = () => new Promise(resolve => {
    recorder.onstop = async () => {
      const blob = new Blob(globalThis.chunks, {type: 'video/webm'});
      const reader = new FileReader();
      reader.onloadend = () => resolve({webm: String(reader.result).split(',')[1] ?? '', frames});
      reader.readAsDataURL(blob);
    };
    recorder.stop();
  });
  return true;
})()`;

async function record(browser, tab, name, {width, height}, body) {
  if (!withVideo) { await body(); return null; }
  const dir = join(videoDir, `${name}-frames`);
  rmSync(dir, {recursive: true, force: true});
  let recorder = null;
  try {
    recorder = await browser.tab();
    await recorder.navigate('about:blank');
    await recorder.evaluate(RECORDING_TAB);
    await recorder.evaluate(`begin(4000000)`);
  } catch (error) {
    log(`${name}: this browser cannot record (${error.message ?? error}); the shots are what it keeps`);
    recorder = null;
  }
  // The clock the frames are shot on: the page is asked for one, drawn, and
  // only then asked for the next, so a slow frame makes a longer step rather
  // than a queue of stale ones.
  const frames = [];
  let stop = false;
  const tick = async () => {
    while (!stop) {
      try {
        // The page's own pixels, one CSS pixel per pixel: a recording is
        // watched, not read, so a JPEG of the viewport is what is drawn.
        const shot = await tab.send('Page.captureScreenshot', {format: 'jpeg', quality: 82,
          captureBeyondViewport: false, clip: {x: 0, y: 0, width, height, scale: 1}});
        const jpeg = shot.result?.data;
        if (jpeg && recorder) {
          frames.push(Date.now());
          await recorder.evaluate(`feed("data:image/jpeg;base64,${jpeg}")`);
        }
      } catch { /* A page mid-navigation answers nothing: the next frame catches it. */ }
      await sleep(Math.max(0, 1000 / FPS - 40));
    }
  };
  const taking = tick();
  try {
    await body();
  } finally {
    stop = true;
    await taking;
  }
  if (!recorder) return {frames: frames.length, file: null, framesDir: null};
  try {
    const {webm, frames: drawn} = await recorder.evaluate('finish()');
    await recorder.close().catch(() => {});
    if (!webm) return {frames: drawn ?? frames.length, file: null, framesDir: null};
    const file = join(videoDir, `${name}.webm`);
    writeFileSync(file, Buffer.from(webm, 'base64'));
    return {frames: drawn ?? frames.length, bytes: statSync(file).size, file, framesDir: null};
  } catch (error) {
    log(`${name}: the recording could not be written (${error.message ?? error})`);
    await recorder.close().catch(() => {});
    return {frames: frames.length, file: null, framesDir: null};
  }
}

// ---------------------------------------------------------------------------

const checks = [];
const notes = [];
const check = (id, pass, detail = '') => {
  checks.push({id, pass: !!pass, detail: String(detail).slice(0, 1200)});
  log(`${pass ? 'PASS' : 'FAIL'} ${id}${detail ? ` - ${String(detail).slice(0, 300)}` : ''}`);
};
/** Something the product does not do today: the run goes on, and the report
 * says it. */
const note = (id, pass, detail = '') => {
  notes.push({id, pass: !!pass, detail: String(detail).slice(0, 1200)});
  log(`${pass ? 'NOTE ok' : 'NOTE'} ${id}${detail ? ` - ${String(detail).slice(0, 300)}` : ''}`);
};

const browser = await launch({args: ['--ignore-certificate-errors',
  // A build container runs as root, where Chromium refuses its own sandbox.
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
  // The host's packet relay is plain http (the browser's own WebTransport
  // would be https behind an ingress): this lets the page dial it as it is.
  '--webtransport-developer-mode',
  // SwiftShader's WebGPU: a host GPU in headless Chrome can lose its device mid-run.
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader']});
// A page of its own, for reading what a shot shows (the shell's tone).
const scratch = await browser.tab();
await scratch.navigate('about:blank');

try {
  for (const width of widths) {
    for (const theme of themes) {
      const combo = `${width}-${theme}`;
      const height = width < 500 ? 844 : 800;
      const mobile = width < 500;
      // A fresh profile each time: every shot is of a first visit.
      const context = await browser.context();
      const tab = await browser.tab({context, firstRun: true});
      let agent = null;
      await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 2, mobile});
      await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: theme}]});
      await tab.send('Browser.grantPermissions', {origin: siteOrigin, browserContextId: context,
        permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
      try {
        // 1. The first screen, and the code in the prompt it copies.
        await tab.navigate(`${siteOrigin}/`);
        await tab.waitFor(`document.getElementById('first-run') && getComputedStyle(document.getElementById('first-run')).display !== 'none'`,
          {timeout: 180000});
        await sleep(600);
        await shot(tab, combo, 'first-screen');
        // The button waits for the page to offer setup (it can make codes once
        // it is in the site's workspace): press it once the page is in.
        const usable = await until(() => tab.evaluate(`document.documentElement.dataset.entry === 'open'`), 240000, 300);
        if (!usable) {
          log(`${combo}: the page never got into the home workspace: entry=${await tab.evaluate('document.documentElement.dataset.entry ?? null')}`,
            `status=${await tab.evaluate('document.getElementById("status")?.textContent ?? ""')}`);
          log(`${combo}: console`, tab.log.slice(-12).join(' | '));
        }
        const button = await tab.evaluate(`(() => {
          const rect = document.getElementById('first-run-agent').getBoundingClientRect();
          return {x: rect.x + rect.width / 2, y: rect.y + rect.height / 2};
        })()`);
        for (const type of ['mousePressed', 'mouseReleased']) {
          await tab.send('Input.dispatchMouseEvent', {type, x: button.x, y: button.y, button: 'left', clickCount: 1});
        }
        const asked = await until(async () => {
          const state = await tab.evaluate(`(() => ({
            run: document.documentElement.dataset.firstRun ?? null,
            prompt: document.getElementById('first-run-prompt')?.textContent ?? '',
            problem: document.getElementById('first-run-problem')?.textContent ?? '',
            steps: document.getElementById('first-run-steps')?.textContent ?? '',
          }))()`);
          if (/tilo login [A-Za-z0-9-]{4,}/.test(state.prompt)) return state;
          if (state.run === 'failed' || state.problem) throw new Error(`the first screen failed: ${state.problem} (steps: ${state.steps})`);
        }, 90000, 300);
        if (!asked) {
          // Why the screen made no code: the state it shows, and what the
          // host answers a pairing link this page makes.
          log(`${combo}: the first screen made no code`, JSON.stringify(await tab.evaluate(`(() => ({
            run: document.documentElement.dataset.firstRun ?? null,
            problem: document.getElementById('first-run-problem')?.textContent ?? '',
            steps: document.getElementById('first-run-steps')?.textContent ?? '',
          }))()`)));
          const link = await tab.evaluate(`globalThis.tiloIdentity?.connectAgent?.().then(made => made.link, error => 'connectAgent failed: ' + error)`);
          log(`${combo}: the page's pairing link`, typeof link === 'string' ? `${link.slice(0, 60)}…${link.slice(-40)}` : JSON.stringify(link));
          const refusal = typeof link === 'string' && link.startsWith('https://') ? await postJson(`${hostOrigin}/peer/v1/login-code`, {link}) : null;
          log(`${combo}: the host answered the link with`, JSON.stringify(refusal));
          throw new Error(`the first screen made no code: ${refusal ? JSON.stringify(refusal) : 'no pairing link'}`);
        }
        const prompt = asked.prompt;
        log(`${combo}: the tab copied “${prompt}”`);
        const code = (prompt.match(/tilo login ([A-Za-z0-9-]{4,})/) ?? [])[1];
        if (!code) throw new Error(`the prompt carried no code: ${prompt}`);
        const copied = await tab.evaluate(`Promise.race([navigator.clipboard.readText().catch(() => null),
          new Promise(resolve => setTimeout(() => resolve(null), 3000))])`);
        check(`${combo}: the click copied the prompt with the code`,
          copied === null || copied.includes(`tilo login ${code}`),
          copied === null ? 'the clipboard was not readable here' : copied.slice(0, 300));
        await shot(tab, combo, 'waiting-for-agent');

        // 2. The real agent, as the prompt tells it.
        const loggedIn = await login(code);
        check(`${combo}: tilo login made this computer a device of the person`, /Logged in:/.test(loggedIn.stdout),
          loggedIn.stdout.split('\n')[0] ?? '');
        const followed = await until(async () => {
          const state = await tab.evaluate(`document.documentElement.dataset.firstRun ?? null`);
          return state === 'connected' || state === null ? state ?? 'left' : undefined;
        }, 90000, 200);
        check(`${combo}: the tab followed the agent into the workspace`, followed === 'connected' || followed === 'left', String(followed));
        const PAGE = `(() => ({
          entry: document.documentElement.dataset.entry ?? null,
          firstRun: document.documentElement.dataset.firstRun ?? null,
          // The entry page keeps the screen's markup (a link may show it):
          // what matters is whether it is shown.
          screen: Boolean(document.getElementById('first-run')) &&
            getComputedStyle(document.getElementById('first-run')).display !== 'none',
          mount: Boolean(globalThis.tiloMount),
          status: document.getElementById('status')?.textContent ?? '',
        }))()`;
        let said = 0;
        const landed = await until(async () => {
          const page = await tab.evaluate(PAGE);
          // Where the page stands while it lands: a reload, or a screen that
          // stays, reads as nothing happening.
          if (Date.now() - said > 5000) { said = Date.now(); log(`${combo}: the page`, JSON.stringify(page)); }
          return page.entry === 'open' && !page.screen && page.mount ? page : undefined;
        }, 240000, 300);
        if (!landed) throw new Error(`the person's workspace did not open: ${JSON.stringify(await tab.evaluate(PAGE))}`);
        // The screen leaves for the person's own desktop (a site that gives
        // one); the agent is in the site's workspace, so go back to it, as
        // the workspace menu does.
        const where = await tab.evaluate('location.hash');
        if (!/#\/.*\/w\//.test(where)) {
          log(`${combo}: the person's own desktop (${where || '/'}); opening the workspace where the agent is`);
          await tab.navigate(`${siteOrigin}/`);
          const back = await until(async () => {
            const page = await tab.evaluate(PAGE);
            return page.entry === 'open' && page.mount ? page : undefined;
          }, 240000, 300);
          if (!back) throw new Error(`the workspace did not open again: ${JSON.stringify(await tab.evaluate(PAGE))}`);
        }
        await sleep(2000);
        log(`${combo}: in the workspace`, await tab.evaluate(`(globalThis.tiloMount?.state()?.workspaceName ?? "") + " " + location.hash`));

        // 3. The agent's MCP connection: present in the workspace from now on.
        agent = mcpClient();
        const initialized = await agent.initialize();
        log(`${combo}: tilo mcp initialized`, JSON.stringify(initialized?.serverInfo ?? initialized));
        const listed = await agent.call('tilo_ls');
        const rows = listingRows(listed.text);
        const apps = rows.map(line => (line.match(/^[\w.-]+/) ?? [''])[0]).filter(Boolean);
        note(`${combo}: the agent lists the workspace's apps`, !listed.failed && apps.length > 0, listed.text.slice(0, 500));
        log(`${combo}: apps`, apps.join(' '));
        log(`${combo}: the page's people now`, await tab.evaluate(`JSON.stringify(globalThis.tiloMount?.state()?.here ?? null)`).catch(() => 'null'));
        log(`${combo}: the page's members now`, await tab.evaluate(`globalThis.tiloPeople?.() ?? 'no people hook'`).catch(() => 'null'));

        // 4. The note the agent writes in, open on the person's screen: the
        //    window following brings forward is one they already have.
        const noteProblem = await openNote(tab);
        note(`${combo}: the person has a note open for the agent to write in`, !noteProblem, noteProblem ?? '');
        await shot(tab, combo, 'note-open');

        // 5. Following it, its card open beside the person: `landing`.
        const followProblem = await followAgent(tab, 'Claude Code');
        note(`${combo}: the person can follow the agent`, !followProblem, followProblem ?? '');
        const roster = await semantics(tab);
        note(`${combo}: who is here shows the agent present`, roster.some(node => /claude code/i.test(node.name)), names(roster));
        await sleep(1500);
        await shot(tab, combo, 'landing', {expect: [/^everyone here$/i, /claude code/i]});
        // A light scheme the shell does not draw is said with what it drew:
        // the page takes its theme from the person's settings, and only the
        // reduced-motion and contrast queries from the browser.
        const drew = await luminance(scratch, tab);
        note(`${combo}: the shell draws this scheme`, drew !== null && (theme === 'light' ? drew > 0.5 : drew < 0.5),
          `prefers-color-scheme is ${theme} in this tab (emulated before load); the shot's mean luminance is ${drew === null ? 'unreadable' : drew.toFixed(2)}, so the shell drew its own theme`);

        // 5. A few of the agent's actions, with the person following: the
        //    recording, and `following-mid-edit` from the first write.
        const steps = await planActions(agent, apps);
        const pacing = async () => {
          for (const [index, step] of steps.entries()) {
            const sending = agent.call(step.tool, step.args);
            if (index === steps.shootAt) {
              // The edit is in flight: the agent's window in front of the
              // person, the text landing in it.
              const editing = await until(async () => (await semantics(tab)).some(node => /claude|notes/i.test(node.name)), 6000, 200);
              await sleep(700);
              await shot(tab, combo, 'following-mid-edit', {expect: [/notes|claude/i]});
              log(`${combo}: mid-edit shot ${editing ? 'with the agent in front' : 'but no agent window showed'}`);
            }
            const answer = await sending;
            step.answer = answer;
            log(`${combo}: agent ${step.label} -> ${answer.failed ? 'failed' : 'ok'} ${answer.text.slice(0, 300)}`);
            await sleep(1400);
          }
        };
        const video = await record(browser, tab, combo, {width, height}, pacing);
        const ran = steps.filter(step => step.answer && !step.answer.failed);
        note(`${combo}: the agent's actions ran in the workspace`, ran.length >= 2,
          steps.map(step => `${step.label}: ${step.answer ? (step.answer.failed ? 'failed' : 'ok') : 'not run'}`).join('; '));
        if (video) log(`${combo}: recording`, JSON.stringify(video));

        // 6. Who is here, pinned on screen, with the agent in it.
        const pinProblem = await pinOverlay(tab);
        note(`${combo}: who is here pins out over the windows`, !pinProblem, pinProblem ?? '');
        await shot(tab, combo, 'presence-with-agent', {expect: [/^everyone here$/i, /claude code/i]});

        // A window filling the desk: the bar's own controls and the reading it shows stand apart at
        // every width, the phone's included.
        if (await press(tab, /^maximize$/i, {what: 'maximizing the window'})) {
          await sleep(900);
          await shot(tab, combo, 'maximized');
          await press(tab, /^(restore|maximize)$/i, {what: 'restoring the window'});
          await sleep(600);
        }
        const overlay = await semantics(tab);
        note(`${combo}: the overlay lists the agent`, overlay.some(node => /claude code/i.test(node.name)), names(overlay));
        const exceptions = tab.log.filter(line => line.startsWith('EXC') || /panic/i.test(line));
        check(`${combo}: no exceptions in the page`, exceptions.length === 0, exceptions.join(' | '));

        writeFileSync(join(browserDir, `${combo}-semantics.json`), JSON.stringify(overlay, null, 1));
        writeFileSync(join(browserDir, `${combo}-state.json`),
          JSON.stringify(await tab.evaluate('globalThis.tiloMount?.state?.() ?? null').catch(() => null), null, 1));
        writeFileSync(join(browserDir, `${combo}-console.log`), tab.log.join('\n'));
        writeFileSync(join(browserDir, `${combo}-channels.txt`), await tab.evaluate('globalThis.tiloChannels?.() ?? ""').catch(() => ''));
        writeFileSync(join(browserDir, `${combo}-mcp.log`), agent.said());
      } catch (error) {
        check(`${combo}: ran to the end`, false, String(error?.stack ?? error));
        await tab.screenshot().then(png => png && writeFileSync(join(browserDir, `${combo}-failed.png`), png)).catch(() => {});
        writeFileSync(join(browserDir, `${combo}-console.log`), tab.log.join('\n'));
        writeFileSync(join(browserDir, `${combo}-mcp.log`), agent?.said() ?? '');
        log(`${combo}: stopped`, String(error?.message ?? error));
      } finally {
        await agent?.close().catch(() => {});
        await tab.close().catch(() => {});
      }
    }
  }
} finally {
  for (const entry of [...children].reverse()) await stop(entry);
  site.close();
  await browser.close().catch(() => {});
  writeFileSync(join(out, 'summary.json'), JSON.stringify({started: new Date(t0).toISOString(), out, bundle,
    relay: relayBin, tilo: tiloBin, site: siteOrigin, host: hostOrigin, workspace: roomId, checks, notes}, null, 2));
  writeFileSync(join(out, 'run.log'), logLines.join('\n'));
}
const failures = checks.filter(entry => !entry.pass);
console.log(`\n${checks.length - failures.length}/${checks.length} checks passed; shots under ${shotsDir}`);
if (notes.length) console.log(`what the product does not do today:\n${notes.map(entry => `- ${entry.pass ? '(now it does) ' : ''}${entry.id}: ${entry.detail}`).join('\n')}`);
if (failures.length) {
  console.error(`${failures.length} failed:\n${failures.map(failure => `- ${failure.id}: ${failure.detail}`).join('\n')}`);
  process.exit(1);
}
