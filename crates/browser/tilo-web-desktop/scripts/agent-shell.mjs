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
// recording is Chrome's screencast, encoded by ffmpeg where it is installed
// and otherwise by the browser itself, so no video tool has to be installed.
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
import {X509Certificate, createHash, randomBytes} from 'node:crypto';
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
// A run is a first visit. The host's state keeps what an earlier run did: the workspace
// remembers that the person was following the agent and what the agent wrote there, and a run
// starting from it shoots the last run's desk — already following, so no Follow to press, with
// a note the person never opened. `--keep-work` runs against what was left.
if (!flag('--keep-work')) {
  rmSync(work, {recursive: true, force: true});
  rmSync(out, {recursive: true, force: true});
}
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
// The run's log is written line by line as it goes: a run that stops anywhere — a host that never
// starts, a step that throws, a run killed from outside — still leaves what it did.
const runLog = join(out, 'run.log');
writeFileSync(runLog, '');
const log = (...parts) => {
  const line = `+${((Date.now() - t0) / 1000).toFixed(1)}s ${parts.join(' ')}`;
  logLines.push(line);
  console.log(line);
  try { appendFileSync(runLog, `${line}\n`); } catch { /* The console has it. */ }
};

const checks = [];
const notes = [];
/** Each agent call the run made, with every answer it got: the first (`ok`, `pending`, `failed`)
 * and each `tilo_status` follow-up of one answered pending, by the step it was for. */
const calls = [];
/** What stopped the run before its end, when something did. */
let stoppedBy = null;
/** A value the run may not have reached yet: `null` until it did. */
const known = get => { try { return get() ?? null; } catch { return null; } };
/** `summary.json`, as the run stands: written after every check and once more as it ends, so it is
 * there however the run ends. */
const writeSummary = () => {
  try {
    writeFileSync(join(out, 'summary.json'), JSON.stringify({started: new Date(t0).toISOString(), out,
      bundle: known(() => bundle), relay: known(() => relayBin), tilo: known(() => tiloBin),
      site: known(() => siteOrigin), host: known(() => hostOrigin), workspace: known(() => roomId),
      stoppedBy, checks, notes, calls}, null, 2));
  } catch { /* The log has it. */ }
};
process.on('uncaughtExceptionMonitor', error => {
  stoppedBy = String(error?.stack ?? error);
  log('stopped:', String(error?.message ?? error));
});
process.on('exit', () => {
  writeSummary();
  // A run stopped before its own clean-up leaves no host or browser behind.
  for (const entry of known(() => children) ?? []) {
    if (!entry.exited) try { process.kill(-entry.child.pid, 'SIGKILL'); } catch {}
  }
});
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.once(signal, () => {
    stoppedBy = signal;
    log(`stopped by ${signal}`);
    process.exit(1);
  });
}
const check = (id, pass, detail = '') => {
  checks.push({id, pass: !!pass, detail: String(detail).slice(0, 1200)});
  log(`${pass ? 'PASS' : 'FAIL'} ${id}${detail ? ` - ${String(detail).slice(0, 300)}` : ''}`);
  writeSummary();
};
/** Something the product does not do today: the run goes on, and the report
 * says it. */
const note = (id, pass, detail = '') => {
  notes.push({id, pass: !!pass, detail: String(detail).slice(0, 1200)});
  log(`${pass ? 'NOTE ok' : 'NOTE'} ${id}${detail ? ` - ${String(detail).slice(0, 300)}` : ''}`);
  writeSummary();
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
  // than made again: only what changed is compiled, bound and split. What a
  // run stopped part way through building (no `build.json` yet) is this
  // script's own directory, and is built again from empty.
  const kept = existsSync(join(bundle, 'build.json'));
  if (!kept) rmSync(bundle, {recursive: true, force: true});
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
/** How many frames the page has presented so far, or `null` before it mounted. */
const presentedFrames = tab => tab.evaluate('globalThis.tiloMount?.state?.()?.frames ?? null').catch(() => null);
/** The frames a page draws before what a press asked of it shows: the press's own frame, the one
 * laying out what it opened, and one more for what that brought in. */
const DRAWN_FRAMES = 3;
/** The frames a window a press opened takes to show what it holds: the press's frame, the window
 * standing as it opens, the app loading and drawing, and its first read of what it shows. A wait of
 * {@link DRAWN_FRAMES} on a page drawing a frame every few seconds gave up while the window still
 * loaded, and the card came a few frames later. */
const WINDOW_FRAMES = 8;
/** The longest a press waits for the page, however slowly it draws. */
const DRAWN_CAP_MS = 90000;
/**
 * `fn`'s answer about what the page shows, waited for as long as the page takes to draw it: at
 * least `ms`, and until the page has presented {@link DRAWN_FRAMES} frames since the wait began.
 * A page answers what a person did on its next frames, and on a machine with every core busy a
 * software-drawn page makes one every few seconds: a wait measured in wall time alone gave up
 * before the frame that showed the answer, and a press tried again then undid the one that had
 * worked (a second Follow is a Stop following). Measured in frames, a slow page is waited for and a
 * fast one gives up as soon as before.
 */
async function untilDrawn(tab, fn, ms, interval = 150, frames = DRAWN_FRAMES) {
  const began = Date.now();
  const first = await presentedFrames(tab);
  for (;;) {
    let value;
    try { value = await fn(); } catch { value = undefined; }
    if (value) return value;
    const waited = Date.now() - began;
    if (waited > DRAWN_CAP_MS) return undefined;
    if (waited > ms) {
      const now = await presentedFrames(tab);
      if (first === null || now === null || now - first >= frames) return undefined;
    }
    await sleep(interval);
  }
}
/** Until the page has stopped drawing: no frame presented for `quiet` ms, or `ms` passed. A page
 * draws only while something on it moves, so a notice fading in is still drawing and one that
 * stands is not. */
async function atRest(tab, {quiet = 1500, ms = 20000} = {}) {
  const deadline = Date.now() + ms;
  let last = await presentedFrames(tab);
  let since = Date.now();
  while (Date.now() < deadline) {
    await sleep(250);
    const now = await presentedFrames(tab);
    if (now !== last) { last = now; since = Date.now(); continue; }
    if (Date.now() - since >= quiet) return;
  }
}
/** Until the page has presented `frames` more frames than now, or `ms` passed: what a press
 * changed is drawn by then, however slowly the page draws. */
async function afterFrames(tab, frames = 1, ms = 30000) {
  const first = await presentedFrames(tab);
  if (first === null) { await sleep(400); return; }
  await until(async () => ((await presentedFrames(tab)) ?? first) - first >= frames || undefined, ms, 150);
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
// Where pages reach the host: WebTransport over UDP. The site in front of the host carries only
// TCP, as a deployment's ingress does, so the host takes its sessions at a port of its own and
// names that origin to the pages, as a deployment's TILO_ROOMS_LINK does.
const linkPort = await freePort();
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
const host = detached('host', relayBin, ['room', '--state', join(work, 'room-state'),
  '--persistent', 'tilo', '--workspace-name', 'Tilo', '--root-file', rootFile, '--room-id-file', roomIdFile,
  '--web', `127.0.0.1:${hostPort}`, '--tls', `files:${certPath},${keyPath}`,
  '--web-plain', `127.0.0.1:${plainPort}`, '--public-origin', siteOrigin,
  '--packet-relay', `127.0.0.1:${relayPort}`,
  '--link-listen', `127.0.0.1:${linkPort}`, '--link-origin', `https://127.0.0.1:${linkPort}`,
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
let stopped = null;
host.child.once('exit', code => { stopped = code; });
const started = await until(() => {
  const line = host.stdout.split('\n')
    .find(candidate => candidate.startsWith(OPERATOR_LINE) && candidate.includes('"started"'));
  if (line) return JSON.parse(line.slice(OPERATOR_LINE.length));
  if (stopped !== null) return {stopped: `the host stopped (exit ${stopped}): ${host.stderr.slice(-1500)}`};
}, 240000, 200);
// A start that stopped is said as it stopped: the host explains itself, and a
// second start would only hide what it said.
if (started?.stopped) throw new Error(started.stopped);
if (!started) throw new Error(`the host did not start: ${host.stderr.slice(-1200)}`);
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

const WALK_FN = `() => {
  const out = [];
  const rect = b => (Array.isArray(b) ? {x0: b[0], y0: b[1], x1: b[2], y1: b[3]}
    : b ? {x0: b.x0 ?? b.x ?? 0, y0: b.y0 ?? b.y ?? 0, x1: b.x1 ?? (b.x ?? 0) + (b.width ?? 0), y1: b.y1 ?? (b.y ?? 0) + (b.height ?? 0)} : null);
  const node = n => { out.push({role: n.role ?? '', name: n.name ?? '', description: n.description ?? '', value: n.value ?? '', rect: rect(n.rect ?? n.bounds)}); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  return out;
}`;
const WALK = `(${WALK_FN})()`;

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
/** The agent's cursor, as the frame publishes it: the mark it draws where it acts, named with
 * what the agent is doing there ("Claude Code: Appending paragraph"), or, for the caret it says
 * it holds in the text, that it writes there ("Claude Code is writing here"). The agent's face
 * stands in the top bar, named after them alone, so the words after the name tell the cursor
 * from it. */
const agentCursor = nodes => nodes.find(node => node.role === 'Image'
  // The caret is named as the agent's tag reads ("Claude Code · Agent is writing here").
  && /^claude code(\s*·\s*agent)?(\s*:|\s+is writing here$)/i.test(node.name)
  // The mark the text draws at the caret stands where it is drawn; a node with no bounds is
  // the agent at work with nothing drawn in text, which is not the cursor on what it writes.
  && node.rect && node.rect.x1 - node.rect.x0 > 0 && node.rect.y1 - node.rect.y0 > 0);
/** Watch, from inside the page, for the agent's cursor in any frame the page draws: the mark
 * stands for the second and a half a write stays lit, and a page on a loaded machine draws a frame
 * every few seconds, so a read over the protocol (each one a request and a wait for the next
 * frame's semantics) can fall between the frames that drew it. The page keeps the first frame's
 * semantics that held the cursor until the run reads them. */
const watchAgentCursor = tab => tab.evaluate(`(() => {
  clearInterval(globalThis.__agentCursorWatch);
  globalThis.__agentCursorSeen = null;
  const walk = ${WALK_FN};
  const cursorOf = ${agentCursor.toString()};
  globalThis.__agentCursorWatch = setInterval(() => {
    try {
      globalThis.tiloMount?.requestSemantics?.();
      const nodes = walk();
      if (cursorOf(nodes)) {
        globalThis.__agentCursorSeen = nodes;
        clearInterval(globalThis.__agentCursorWatch);
      }
    } catch { /* A frame mid-rebuild answers on the next tick. */ }
  }, 60);
  return true;
})()`);
/** The semantics of the first frame that drew the agent's cursor since `watchAgentCursor`. */
const seenAgentCursor = tab => tab.evaluate('globalThis.__agentCursorSeen').catch(() => null);
const stopWatchingAgentCursor = tab => tab.evaluate('clearInterval(globalThis.__agentCursorWatch)').catch(() => {});
/** Where a control the page publishes for assistive technology stands, read
 * from the page's semantic mirror: the elements it lays over what it draws,
 * which carry the bounds of the node they speak for. A role's own rows and
 * buttons (who is here) are published that way, with no widget of their own
 * to read a rect from. `null` when the page lays out nothing by that name. */
const mirrorRect = async (tab, pattern) => tab.evaluate(`(() => {
  const wanted = new RegExp(${JSON.stringify(pattern.source)}, ${JSON.stringify(pattern.flags)});
  for (const element of document.querySelectorAll('[aria-label], [aria-description]')) {
    const name = element.getAttribute('aria-label') ?? '';
    const description = element.getAttribute('aria-description') ?? '';
    if (!wanted.test(name) && !wanted.test(description)) continue;
    const rect = element.getBoundingClientRect();
    if (!(rect.width >= 1 && rect.height >= 1)) continue;
    return {name, description, rect: {x0: rect.x, y0: rect.y, x1: rect.right, y1: rect.bottom}};
  }
  return null;
})()`).catch(() => null);
/** Press the control `pattern` calls: by the place the page draws it, or, for a
 * control only the assistive tree carries, by where the page's own semantic
 * mirror lays it over that drawing. */
async function press(tab, pattern, {what = null} = {}) {
  let node = matching(await semantics(tab), pattern);
  if (node && !node.rect) node = await mirrorRect(tab, pattern) ?? node;
  if (!node?.rect) return null;
  const {x0, y0, x1, y1} = node.rect;
  const x = (x0 + x1) / 2, y = (y0 + y1) / 2;
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
  await tab.send('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: 'left', clickCount: 1});
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: 'left', clickCount: 1});
  if (what) log(`pressed ${what}: ${node.name || node.description} (${node.role}) at ${Math.round(x)},${Math.round(y)}`);
  return node;
}
/** The pointer moves by a pixel and back over the middle of the page: an input
 * the page never ignores, for the frame a press's own wake should have drawn.
 * Nothing is pressed, so nothing on the desk changes. */
async function nudge(tab) {
  const middle = await tab.evaluate(`[Math.round(innerWidth / 2), Math.round(innerHeight / 2)]`)
    .catch(() => [100, 100]);
  for (const [x, y] of [[middle[0], middle[1]], [middle[0] + 1, middle[1]]]) {
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
  }
}

/** A click at a point on the page, where no control is named: what the desk
 * or a window under it takes, as a person's finger lands there. */
async function clickAt(tab, x, y) {
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
  await tab.send('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: 'left', clickCount: 1});
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: 'left', clickCount: 1});
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
async function shot(tab, combo, name, {expect = null, read = null} = {}) {
  // A shot of a moment, its tree (`read`) taken just before: closing what covers the desk and
  // reading the tree again would outlast the moment the shot is of.
  if (!read) await dismissOverlays(tab);
  if (expect) {
    const nodes = read ?? await semantics(tab);
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
    // What a fresh `tilo mcp` answers first is where an agent's return is spent when it is slow:
    // its entry into the workspace, or the call itself. The page shows it only after both.
    const askedAt = Date.now();
    const name = message.params?.name ?? message.method;
    waiting.set(requestId, answer => {
      log(`tilo mcp ${name} answered in ${((Date.now() - askedAt) / 1000).toFixed(1)} s`);
      resolve(answer);
    });
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
/** The actions an answer lists under a path, as `#name(params)  What it does`:
 * the form an app's own listing uses for what the path publishes. */
const actionRows = text => [...text.matchAll(/^#([\w-]+)(?:\(([^)]*)\))?[ \t]*(.*)$/gm)].map(([, name, params = '', label = '']) =>
  ({name, label, params: params.split(',').map(param => param.trim().split(':')[0]).filter(Boolean)}));
/** The resources an answer lists, by the name a path takes: a row is
 * `Notebook  notes.notebook  [26 actions]`, and the app's own head row
 * (`notes  (so.tilo.notes)`) names no resource. */
const resourceNames = text => text.split('\n').map(line => line.trim())
  .filter(line => line && !line.startsWith('#'))
  .map(line => line.split(/\s{2,}/))
  .filter(columns => columns.length >= 2 && !columns[1].startsWith('('))
  .map(columns => columns[0].replace(/\/$/, ''));
const firstAction = (text, wanted, unwanted = /delete|remove|clear|reset/i) =>
  actionPaths(text).find(path => wanted.test(path) && !unwanted.test(path));
/** How a tool's answer reads: an action still running (past the tool's wait,
 * or waiting for the person to confirm it) is `pending`, not `ok`. */
const verdict = answer => answer.failed ? 'failed' : /^pending\b/i.test(answer.text) ? 'pending' : 'ok';

/** What the agent learns of an action the host answered as pending: a host that took longer than
 * its admission wait says so with the invocation's id, and the agent asks `tilo_status` with it
 * until it says how it went, as the answer tells it to. */
async function settled(agent, answer, followed = () => {}) {
  const deadline = Date.now() + 120000;
  // The host marks a pending answer an error, as it is no outcome yet: its words say pending.
  const pending = said => /^pending \S+:/i.test(said.text);
  while (pending(answer) && Date.now() < deadline) {
    const id = (answer.text.match(/^pending (\S+):/i) ?? [])[1];
    if (!id) break;
    await sleep(1000);
    answer = await agent.call('tilo_status', {invocation_id: id});
    followed(answer);
  }
  // A batch answers each call on a line of its own ("[1] path: ok", "[2] path: pending ID: …"):
  // each call still pending is asked after until it is done, and the batch is answered as its
  // calls came out.
  if (/^\[\d+\] /m.test(answer.text) && /: pending \S+:/.test(answer.text)) {
    const lines = await Promise.all(answer.text.split('\n').map(async line => {
      const id = (line.match(/^\[\d+\] .*?: pending (\S+):/) ?? [])[1];
      if (!id) return {line, failed: false};
      const head = line.slice(0, line.indexOf(': pending '));
      const done = await settled(agent, {failed: true, text: `pending ${id}: not finished yet`}, followed);
      const ok = verdict(done) === 'ok';
      return {line: `${head}: ${ok ? 'ok' : done.text.split('\n')[0]}`, failed: !ok};
    }));
    const failed = lines.some(entry => entry.failed) || /: (failed|refused)\b/i.test(answer.text);
    answer = {...answer, failed, text: lines.map(entry => entry.line).join('\n')};
  }
  return answer;
}

/**
 * What the agent does next, discovered from the workspace as an agent would:
 * its apps, then each app's own actions. Required: writing in the note the
 * person has open. Optional: a command in the Terminal, where the workspace
 * has it (a workspace is its person's: it has what it has).
 */
async function planActions(agent, apps) {
  // What the agent reads its next step off: an action still pending is waited for, as an agent
  // asks after it, and a listing the host could not answer in time is asked again, a few times. On
  // a loaded machine making a note answered "pending", the listing after it timed out, and the run
  // wrote in the notebook rather than the note it had just made.
  const ask = async (tool, args) => {
    let answer = await settled(agent, await agent.call(tool, args));
    for (let retry = 0; retry < 3 && tool === 'tilo_ls' && answer.failed && /did not answer/i.test(answer.text); retry += 1) {
      log(`agent ${tool} ${JSON.stringify(args)} timed out; asking again`);
      answer = await agent.call(tool, args);
    }
    log(`agent ${tool} ${JSON.stringify(args)} -> ${verdict(answer)} ${answer.text.slice(0, 600)}`);
    calls.push({step: 'planning', tool, args, answers: [verdict(answer)], answer: verdict(answer),
      said: answer.text.slice(0, 300)});
    return answer;
  };
  const steps = [];
  const app = pattern => apps.find(name => pattern.test(name));
  /** The action to write with, as the note publishes it: the app's own name
   * for it, so the call takes the path and action the listing gave. */
  const writing = rows => rows.find(row => /^(append|write|insert|add-text|set-text|insert-text|edit)/.test(row.name));
  /** The actions `path` publishes, asked where they are published: an app's
   * actions live on the resource it names, and a listing above mixes both. */
  const actionsOf = async path => actionRows((await ask('tilo_ls', {path, kind: 'actions'})).text);
  // The desktop's own service answers without an app: asking the person's
  // page what it shows is an action like any other, and the first one an
  // agent does here (a host that draws no desk has none to list).
  const desktop = await ask('tilo_ls', {path: 'desktop'});
  if (!desktop.failed) {
    const windows = firstAction(desktop.text, /#(list-windows|windows)/i) ?? 'desktop#list-windows';
    steps.push({label: 'listing the windows on the person\'s screen', tool: 'tilo_do', args: {path: windows}});
  }
  const notes = app(/^notes/i);
  let note = null;
  if (notes) {
    // The note to write in: the app's listing names its notebooks, a
    // notebook's names its pages — the agent reads down them as a person
    // reading the app's sidebar would, and writes where the deepest one is.
    let listed = await ask('tilo_ls', {path: notes});
    if (/not running|start it with/i.test(listed.text)) {
      // The app is installed and stopped: the listing says how to start it,
      // and the agent does that before reading what is in it — which is also
      // what puts the app's window on the person's screen.
      steps.push({label: 'opening the note app', tool: 'tilo_do', args: {path: `${notes}#open`}});
      const opened = await ask('tilo_do', {path: `${notes}#open`});
      log('agent: opened the note app', verdict(opened), opened.text.slice(0, 300));
      await sleep(1500);
      listed = await ask('tilo_ls', {path: notes});
    }
    let notebooks = resourceNames(listed.text);
    if (notebooks.length === 0) {
      // A Notes app with nothing in it yet: the agent makes the notebook
      // with the app's own action, as a person pressing "New note" would.
      const make = (await actionsOf(notes)).find(row => /^(new-note|new|create)/.test(row.name));
      if (make) {
        const made = await ask('tilo_do', {path: `${notes}#${make.name}`, args: {title: 'Launch notes'}});
        log('agent: made a notebook', verdict(made), made.text.slice(0, 300));
        listed = await ask('tilo_ls', {path: notes});
        notebooks = resourceNames(listed.text);
      }
    }
    const notebook = notebooks[0];
    if (notebook) {
      const path = `${notes}/${notebook}`;
      // The note it writes in: the one it made, else the notebook's first,
      // else one it makes now — the notebook's own "new-note".
      let pages = resourceNames((await ask('tilo_ls', {path})).text);
      if (pages.length === 0) {
        const make = (await actionsOf(path)).find(row => /^(new-note|new-child|create-page|new)/.test(row.name));
        if (make) {
          const made = await ask('tilo_do', {path: `${path}#${make.name}`, args: {title: 'Launch notes'}});
          log('agent: made a note', verdict(made), made.text.slice(0, 300));
          pages = resourceNames((await ask('tilo_ls', {path})).text);
        }
      }
      note = pages.length ? `${path}/${pages[0]}` : path;
      // The person's own "New note" leaves a blank note named "Untitled", and that is the one the
      // agent is given: it names it for what it is about to write, as a person would before
      // writing, with the note's own rename. Its path is its title, so the agent reads the
      // notebook again for where the note now is.
      if (pages.length && /^untitled$/i.test(pages[0])) {
        const rename = (await actionsOf(note)).find(row => /^rename$/.test(row.name));
        if (rename) {
          const renamed = await ask('tilo_do', {path: `${note}#${rename.name}`, args: {title: 'Launch notes'}});
          log('agent: named the note', verdict(renamed), renamed.text.slice(0, 300));
          const again = resourceNames((await ask('tilo_ls', {path})).text);
          const named = again.find(page => /^launch notes$/i.test(page));
          if (named) note = `${path}/${named}`;
        }
      }
    }
    if (note) {
      // The text lives in the note's body, a part of the note rather than the
      // note: the write action is asked for at the part the note publishes,
      // which is where its own listing says the text goes.
      let writeAt = note;
      let noteActions = await actionsOf(note);
      let write = writing(noteActions);
      if (!write) {
        for (const part of resourceNames((await ask('tilo_ls', {path: note})).text).slice(0, 4)) {
          const at = `${note}/${part}`;
          const rows = await actionsOf(at);
          const found = writing(rows);
          if (found) { writeAt = at; noteActions = rows; write = found; break; }
        }
      }
      if (write) {
        // One invocation writing three paragraphs: the person following sees
        // the text land line by line, which is what `following-mid-edit` catches.
        steps.push({label: 'writing in the note', tool: 'tilo_do', args: {calls: [
          {path: `${writeAt}#${write.name}`, args: {text: 'Launch notes, from Claude Code.'}},
          {path: `${writeAt}#${write.name}`, args: {text: 'Each line lands in the workspace as I write it.'}},
          {path: `${writeAt}#${write.name}`, args: {text: 'Following me puts this window in front of you.'}},
        ]}});
        steps.push({label: 'writing the next paragraph', tool: 'tilo_do',
          args: {path: `${writeAt}#${write.name}`, args: {text: 'A second invocation, a moment later.'}}});
        // The recording is of the agent writing on while the person follows: a few more lines,
        // each its own invocation a moment apart, as an agent working through a list writes.
        for (const [index, text] of [
          'Ship the hosted page with the agent invite.',
          'Check the phone layout once more.',
          'Write the release note last.',
        ].entries()) {
          steps.push({label: `writing line ${index + 1} of the plan`, tool: 'tilo_do',
            args: {path: `${writeAt}#${write.name}`, args: {text}}});
        }
        steps.push({label: 'reading the note back', tool: 'tilo_read', args: {path: note}});
      } else {
        log('agent: the note publishes no action to write with', noteActions.map(row => row.name).join(' '));
      }
    } else {
      log('agent: the Notes app offers no note to write in', listed.text.slice(0, 300));
    }
  }
  const terminal = app(/^terminal/i) ?? app(/^ush/i);
  if (terminal) {
    // Where the Terminal publishes `run`: on the session it names, or on the
    // app when the session is the app's own record.
    const sessions = resourceNames((await ask('tilo_ls', {path: terminal})).text);
    const places = [sessions[0] ? `${terminal}/${sessions[0]}` : null, terminal].filter(Boolean);
    let run = null;
    for (const place of places) {
      const row = (await actionsOf(place)).find(candidate => /^(run|exec|command|send)$/.test(candidate.name));
      if (row) { run = `${place}#${row.name}`; break; }
    }
    if (run) {
      steps.push({label: 'running a command in the Terminal', tool: 'tilo_do',
        args: {path: run, args: {command: 'echo hello from claude code'}}});
    } else {
      log('agent: the Terminal publishes no action to run a command with', places.join(' '));
    }
  }
  // The shot of the follow mid-edit comes while the first write is in flight.
  // The title the note it writes in stands under, as its window names it.
  steps.noteTitle = note ? note.split('/').at(-1) : null;
  const at = steps.findIndex(step => /writing/.test(step.label));
  steps.shootAt = at < 0 ? steps.length - 1 : at;
  return steps;
}

// ---------------------------------------------------------------------------
// Following the agent, as a person does: the palette finds them by name and
// opens the panel on them, where following is one step; the panel also pins
// out into the overlay that stays on screen.

/** The roster's own node: the list the panel draws who is here in. The bar's
 * faces carry the panel's description ("Who is here, where they are …"), so
 * the panel is found by a node the panel itself draws, never by words the
 * button shares with it. */
const rosterOf = nodes => nodes.find(node => /people here now/i.test(node.name));

/** A person's card, open where the roster stands: what a face in the bar opens. It is announced
 * as theirs ("Claude Code’s profile"), whatever controls it offers that person. */
const personCardOf = nodes => matching(nodes, /^.+[’']s profile$/i);

/** The roster, open, from the bar as a person opens it. The bar announces who is here as
 * "Everyone here": its people button, or, where the faces fit, the faces themselves, which stand
 * for it. A face opens its person's card in the roster's place, and pressed again goes from that
 * card to everyone here. Only the bar opens who is here: the Console's Agents page and a search
 * result that reads like a person open their windows instead, and following from one of those
 * finds no Follow at all. */
async function openPeople(tab) {
  if (rosterOf(await semantics(tab))) return null;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    if (!await press(tab, /^everyone here$/i, {what: 'the roster'})) {
      return `the bar offers no people button: ${names(await semantics(tab))}`;
    }
    const began = Date.now();
    const opened = await untilDrawn(tab, async () => {
      const nodes = await semantics(tab);
      return rosterOf(nodes) ? 'roster' : personCardOf(nodes) ? 'card' : undefined;
    }, 4000);
    const took = Date.now() - began;
    if (opened === 'roster') {
      log(`the roster opened ${took} ms after the press`);
      return null;
    }
    if (opened === 'card') {
      log(`the face opened its person's card ${took} ms after the press; pressed again, it opens everyone here`);
    } else {
      // What the press was on and what stands about people now: a press that opened nothing
      // says what it landed on.
      const nodes = await semantics(tab);
      const about = nodes.filter(node => /profile|people|follow|everyone|claude code|card/i.test(`${node.name} ${node.description ?? ''}`))
        .map(node => `${node.role} "${node.name}"${node.description ? ` (${node.description.slice(0, 60)})` : ''} at ${JSON.stringify(node.rect)}`);
      log(`pressing the people button opened nothing after ${took} ms; about people the page shows: ${about.join('; ')}`);
    }
  }
  return `the people button opened no roster: ${names(await semantics(tab))}`;
}

/** Who is here, closed: the button that opens it closes it, as a person's own
 * press does. */
async function closePeople(tab) {
  if (!rosterOf(await semantics(tab))) return null;
  await press(tab, /^everyone here$/i, {what: 'closing who is here'});
  const closed = await untilDrawn(tab, async () => !rosterOf(await semantics(tab)), 4000, 150);
  if (!closed) return `who is here stayed open: ${names(await semantics(tab))}`;
  // Where the bar's faces stand for everyone here, the press landed on a face, which opens its
  // person's card in the roster's place: the person puts that away too, with Escape, before
  // reading on, or the card stands over the work they follow.
  if (personCardOf(await semantics(tab))) {
    await key(tab, 'Escape', 'Escape', 27);
    const away = await untilDrawn(tab, async () => !personCardOf(await semantics(tab)), 4000, 150);
    if (!away) return `the card a face opened stayed up: ${names(await semantics(tab))}`;
  }
  return null;
}

/** Whether the note following brought forward is the window in front, by the
 * frame's own tree: the windows are published in the order they are drawn, so
 * the last tab bar naming a page is the window in front, and its page is the
 * name a person reads at the top of it. */
const noteInFront = nodes => {
  const bars = nodes.filter(node => typeof node.role === 'string' && /^tabbar$/i.test(node.role) && node.value);
  return /^launch notes$/i.test(bars.at(-1)?.value ?? '');
};

async function followAgent(tab, label) {
  const opened = await openPeople(tab);
  if (opened) return opened;
  // The workspace can carry following from an earlier session: the row then offers the way back
  // rather than Follow, and the person's view is already the agent's.
  if (await untilDrawn(tab, async () => matching(await semantics(tab), new RegExp(`^stop following ${label}`, 'i')), 3000, 150)) {
    log(`${label} is already the person's view: the roster offers Stop following`);
    return null;
  }
  const follow = new RegExp(`^follow ${label}`, 'i');
  /** The panel with a row to follow in it: what the press that opened it owes. */
  const filled = async (wait) => {
    const began = Date.now();
    const nodes = await untilDrawn(tab, async () => {
      const seen = await semantics(tab);
      return rosterOf(seen) && (matching(seen, follow) ?? matching(seen, /^follow$/i)) ? seen : undefined;
    }, wait, 150);
    return {nodes, took: Date.now() - began};
  };
  const first = await filled(3000);
  let panel = first.nodes;
  if (panel) {
    log(`the roster had its rows ${first.took} ms after the press`);
  } else {
    // The panel stands but its rows have not been drawn: a move of the pointer asks for the
    // frame that draws them, which is what the person's next touch would do.
    log(`the roster opened with no rows to follow: ${names(await semantics(tab))}`);
    await nudge(tab);
    const {nodes, took} = await filled(5000);
    log(`after the pointer moved, the roster's rows came ${took} ms later`);
    panel = nodes;
  }
  if (!panel) {
    const seen = await semantics(tab);
    if (!rosterOf(seen)) return `the roster did not open: ${names(seen)}`;
    return `no Follow for ${label}: ${names(seen)}`;
  }
  for (let attempt = 0; attempt < 3; attempt += 1) {
    // Following is the banner's business: the control says so once it took.
    if (await press(tab, follow, {what: `following ${label}`}) ?? await press(tab, /^follow$/i, {what: `following ${label}`})) {
      const following = await untilDrawn(tab, async () => (await semantics(tab)).some(node => new RegExp(`stop following ${label}|following ${label}`, 'i').test(node.name)), 3000);
      if (following) return null;
    } else {
      return `the Follow control for ${label} has no bounds`;
    }
  }
  return `following ${label} did not take`;
}

async function pinOverlay(tab) {
  // The pin is announced as "Keep on screen"; its description is the longer
  // "Keep who is here on screen" a reader hears beside it. What the press must
  // do is what the name says: who is here stays over the windows. Whether the
  // overlay also offers a control to put it back is asked separately, below.
  const pin = /^(keep on screen|keep who is here on screen)$/i;
  const kept = /^put back in the bar$/i;
  const pinned = async () => {
    const nodes = await semantics(tab);
    return nodes.some(node => speaks(node, kept)) || rosterOf(nodes) ? nodes : undefined;
  };
  let pressed = await press(tab, pin, {what: 'pinning who is here'});
  if (!pressed) {
    // The roster closed in the meantime: open it again, then pin.
    await openPeople(tab);
    pressed = await press(tab, pin, {what: 'pinning who is here'});
  }
  if (!pressed) return `no "Keep on screen" control: ${names(await semantics(tab))}`;
  await sleep(1200);
  if (!await untilDrawn(tab, pinned, 4000, 150)) {
    // Whether the press was lost or only not drawn: a pointer move asks for the frame the press
    // itself should have drawn.
    await nudge(tab);
    if (await untilDrawn(tab, pinned, 4000, 150)) {
      return '"Keep on screen" kept it only once the pointer moved: the press drew no frame of its own';
    }
    // Pressed as a reader presses it, through the page's accessible mirror rather than the pointer.
    const mirrored = await tab.evaluate(`(() => {
      for (const element of document.querySelectorAll('[aria-label]')) {
        if (/^keep on screen$/i.test(element.getAttribute('aria-label'))) { element.click(); return true; }
      }
      return false;
    })()`).catch(() => false);
    const read = mirrored && await untilDrawn(tab, pinned, 4000, 150);
    return `"Keep on screen" did not keep it from the pointer${read ? ', but did from its accessible button' : mirrored ? ' nor from its accessible button' : ' (no accessible button to press)'}: ${names(await semantics(tab))}`;
  }
  return null;
}

/** The control that puts the pinned overlay back in the bar, when the overlay
 * offers one: `null` while who is here is kept, `undefined` when there is no
 * such control to press, and the nodes it left behind after pressing it. */
async function unpinned(tab) {
  const nodes = await semantics(tab);
  const back = nodes.some(node => speaks(node, /^put back in the bar$/i));
  if (!back) return undefined;
  await press(tab, /^put back in the bar$/i, {what: 'putting who is here back in the bar'});
  // Back in the bar, the panel it was pinned as is gone, and so is the control only the pinned
  // panel offers. A list still standing over the desk is the press having done nothing.
  const left = await untilDrawn(tab, async () => {
    const seen = await semantics(tab);
    const pinned = seen.some(node => speaks(node, /^put back in the bar$/i));
    return !pinned && !rosterOf(seen) ? seen : undefined;
  }, 8000, 150);
  return left ? nodes : null;
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
    const open = await untilDrawn(tab, async () => (await semantics(tab)).some(node => /search apps, windows, documents/i.test(node.name)), 8000);
    if (open) break;
  }
  await typeText(tab, words);
  // The result, not the same word elsewhere on the desk the palette covers.
  const result = /listitem|menuitem|treeitem|option|button/i;
  // A loaded machine draws the page a frame every few seconds: the results come a few frames
  // after the words are typed.
  const hit = await untilDrawn(tab, async () => matching(await semantics(tab), new RegExp(`^${words}$`, 'i'), result), 20000);
  if (!hit) return `the palette found no ${words}: ${names(await semantics(tab))}`;
  await key(tab, 'Enter', 'Enter', 13);
  await sleep(2500);
  return null;
}

/** The desktop's search, opened with its chord and dismissed by a press on the desk beside its
 * card. A palette still up after the press is the answer: the press went under it instead. */
async function pressBesideSearch(tab, {height}) {
  const mac = await tab.evaluate('/Mac|iPhone|iPad/.test(navigator.platform ?? navigator.userAgent ?? "")');
  // The field is named "Search", and what it covers is its description: either is what a reader
  // hears of it.
  const showing = async () => (await semantics(tab)).some(node => speaks(node, /search apps, windows, documents/i));
  await key(tab, 'k', 'KeyK', 75, mac ? 4 : 2);
  if (!await untilDrawn(tab, showing, 8000)) return `the search chord opened no search: ${names(await semantics(tab))}`;
  const searchOpen = () => tab.evaluate('globalThis.tiloMount?.state()?.searchOpen ?? null');
  // The desk's bottom-left corner: the card stands centred over the top of it.
  const x = 40, y = height - 40;
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
  await tab.send('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: 'left', clickCount: 1});
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: 'left', clickCount: 1});
  if (!await untilDrawn(tab, async () => !await showing(), 8000)) {
    return `the search stayed up after a press beside it: ${names(await semantics(tab))}`;
  }
  const state = await untilDrawn(tab, async () => (await searchOpen()) === false, 8000);
  return state ? null : `the page still says search is open: ${JSON.stringify(await tab.evaluate('globalThis.tiloMount?.state() ?? null')).slice(0, 300)}`;
}

async function openNote(tab) {
  // A window of Notes with a note in it is what following needs: it is the
  // window the person's view goes to, and where the agent's writing lands.
  const withNote = async () => (await semantics(tab)).some(node => /^saved$/i.test(node.name));
  // A Notes window, not the launcher's entry for the app: the window publishes itself as the
  // group of what it shows, and its title bar is the only place "New note" stands.
  const windowed = async () => (await semantics(tab)).some(node => node.role === 'Group' && /^notes$/i.test(node.name));
  if (await withNote()) return null;
  if (!await windowed()) {
    // The desk's own app list first ("Open an app"), the palette after.
    if (await press(tab, /^open an app$/i, {what: 'the app list'})) {
      // The list's own Notes, once a frame drew the list: pressed sooner, the press found the
      // welcome card's Notes, a verb that waits on the app starting in the workspace and opened
      // its window minutes later, beside the one the agent's note had opened meanwhile.
      const listed = await untilDrawn(tab, async () => {
        const nodes = await semantics(tab);
        const results = nodes.findIndex(node => /^results$/i.test(node.name));
        return results < 0 ? undefined : nodes.slice(results + 1).find(node => /^notes$/i.test(node.name) && node.rect);
      }, 6000);
      if (listed) {
        const {x0, y0, x1, y1} = listed.rect;
        await clickAt(tab, (x0 + x1) / 2, (y0 + y1) / 2);
        log(`pressed Notes in the app list at ${Math.round((x0 + x1) / 2)},${Math.round((y0 + y1) / 2)}`);
        await untilDrawn(tab, windowed, 6000);
      } else {
        log(`the app list drew no Notes: ${names(await semantics(tab))}`);
      }
    }
    if (!await windowed()) {
      const opened = await palette(tab, 'Notes');
      if (opened) return `Notes did not open: ${opened}`;
    }
  }
  // A window that just opened says "Loading" until the app has drawn: an empty Notes offers
  // "New note" (in its title bar and as its empty state's action) once it has, which on a loaded
  // machine is a few seconds after the window stands. The press waits for it rather than trying
  // once while the app still loads.
  const offered = await until(async () => {
    const nodes = await semantics(tab);
    return nodes.some(node => /^saved$/i.test(node.name)) || matching(nodes, /^new note$/i) ? true : undefined;
  }, 30000, 400);
  if (!offered) return `Notes offers no note to make 30 s after it opened: ${names(await semantics(tab))}`;
  if (!await withNote() && !await press(tab, /^new note$/i, {what: 'a new note'})) {
    return `Notes offers no note to make: ${names(await semantics(tab))}`;
  }
  const noted = await untilDrawn(tab, withNote, 8000);
  if (!noted) return `Notes made no note: ${names(await semantics(tab))}`;
  await sleep(800);
  return null;
}

// ---------------------------------------------------------------------------
// The recording: Chrome's own screencast (`Page.startScreencast`), which pushes
// a frame each time the page draws, kept as JPEGs with the time each was drawn.
// A frame asked for on a clock (`Page.captureScreenshot`) waits behind the
// page's own work on a loaded machine, and encoding beside the page took the
// CPU it needed: a whole run came back with one to fourteen frames. Pushed
// frames keep the page's pace, and they are encoded only once the scene is
// over: with ffmpeg (VP9 WebM) where it is installed, else replayed on their
// own timing into a canvas in a tab of their own and recorded there
// (`MediaRecorder`, VP8 WebM), so no video tool has to be installed where the
// run shoots.

/** The widest a recording is drawn: a recording is watched, not read. */
const RECORD_MAX_W = 1280;
/** The frame rate ffmpeg writes at: each pushed frame is held until the next one came. */
const RECORD_FPS = 25;
/** The longest one frame is held: a page that drew nothing for longer is still that frame. */
const RECORD_HOLD_S = 5;
/** Draws the frames it is given into a canvas and records it: a page of its own, so nothing it
 * does reaches the shots. */
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
  // The frame on the canvas once more: a held frame is the same picture at the video's own rate,
  // so a page that drew twice in ten seconds is still ten seconds of video.
  globalThis.again = () => { track.requestFrame(); return true; };
  globalThis.begin = bits => {
    recorder = new MediaRecorder(stream, {mimeType: 'video/webm;codecs=vp8', videoBitsPerSecond: bits});
    globalThis.chunks = [];
    recorder.ondataavailable = event => { if (event.data.size) globalThis.chunks.push(event.data); };
    recorder.start(1000);
    return true;
  };
  globalThis.finish = () => new Promise(resolve => {
    recorder.onstop = () => {
      const blob = new Blob(globalThis.chunks, {type: 'video/webm'});
      const reader = new FileReader();
      reader.onloadend = () => resolve({webm: String(reader.result).split(',')[1] ?? '', frames});
      reader.readAsDataURL(blob);
    };
    recorder.stop();
  });
  return true;
})()`;

/** Encode `frames` (each {file, hold} in seconds) with ffmpeg into `file`; false where there is
 * no ffmpeg or it failed. */
function encodeWithFfmpeg(frames, dir, file, name) {
  const lines = [];
  for (const frame of frames) lines.push(`file '${frame.file}'`, `duration ${frame.hold.toFixed(3)}`);
  // The concat demuxer takes the last frame's duration only when it is listed again.
  lines.push(`file '${frames.at(-1).file}'`);
  const list = join(dir, 'frames.txt');
  writeFileSync(list, `${lines.join('\n')}\n`);
  const encoded = spawnSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'concat', '-safe', '0',
    '-i', list, '-vf', `fps=${RECORD_FPS}`, '-c:v', 'libvpx-vp9', '-b:v', '0', '-crf', '32', '-row-mt', '1',
    '-deadline', 'good', '-cpu-used', '4', '-an', file], {encoding: 'utf8'});
  if (encoded.error?.code === 'ENOENT') return false;
  if (encoded.status !== 0) {
    log(`${name}: ffmpeg could not encode the recording (${encoded.error?.message ?? encoded.stderr?.slice(0, 400)})`);
    return false;
  }
  return true;
}

/** Replay `frames` on their own timing into a canvas in a tab of its own, recorded there. */
async function encodeInBrowser(browser, frames, file, name) {
  let recorder = null;
  try {
    recorder = await browser.tab();
    await recorder.navigate('about:blank');
    await recorder.evaluate(RECORDING_TAB);
    await recorder.evaluate('begin(6000000)');
    const started = Date.now();
    let due = 0;
    for (const frame of frames) {
      const jpeg = readFileSync(frame.file).toString('base64');
      await recorder.evaluate(`feed("data:image/jpeg;base64,${jpeg}")`);
      due += frame.hold * 1000;
      while (started + due - Date.now() > 1000 / RECORD_FPS) {
        await sleep(1000 / RECORD_FPS);
        await recorder.evaluate('again()');
      }
      const wait = started + due - Date.now();
      if (wait > 0) await sleep(wait);
    }
    const {webm} = await recorder.evaluate('finish()');
    if (!webm) return false;
    writeFileSync(file, Buffer.from(webm, 'base64'));
    return true;
  } catch (error) {
    log(`${name}: the browser could not encode the recording (${error.message ?? error})`);
    return false;
  } finally {
    await recorder?.close().catch(() => {});
  }
}

async function record(browser, tab, name, {width, height}, body) {
  if (!withVideo) { await body(); return null; }
  const dir = join(videoDir, `${name}-frames`);
  rmSync(dir, {recursive: true, force: true});
  mkdirSync(dir, {recursive: true});
  // Each frame as Chrome drew it, with the time it was drawn (seconds, the machine's wall clock).
  const frames = [];
  let writing = Promise.resolve();
  const off = browser.on(message => {
    if (message.sessionId !== tab.sessionId || message.method !== 'Page.screencastFrame') return;
    const {data, metadata, sessionId: frame} = message.params;
    const file = join(dir, `${String(frames.length).padStart(5, '0')}.jpg`);
    frames.push({file, at: metadata?.timestamp ?? Date.now() / 1000});
    writing = writing.then(() => writeFileSync(file, Buffer.from(data, 'base64')));
    // Chrome sends the next frame only once this one is acknowledged.
    tab.send('Page.screencastFrameAck', {sessionId: frame}).catch(() => {});
  });
  const scale = Math.min(1, RECORD_MAX_W / width);
  const started = await tab.send('Page.startScreencast', {format: 'jpeg', quality: 85,
    maxWidth: Math.round(width * scale), maxHeight: Math.round(height * scale), everyNthFrame: 1})
    .catch(error => ({error}));
  if (started.error) {
    off();
    log(`${name}: this browser cannot screencast (${started.error.message ?? started.error}); the shots are what it keeps`);
    await body();
    return {frames: 0, file: null, framesDir: null};
  }
  const began = Date.now() / 1000;
  // What the page itself presented over the same stretch: a recording with few frames is either a
  // page that drew few (the renderer is starved) or a screencast that dropped what it drew.
  const presented = () => presentedFrames(tab);
  const drawnBefore = await presented();
  // And where each of those frames spent its time, by the page's own stages: what is left of a
  // slow frame once its stages are added up is the GPU's.
  const RUNTIME = `import(new URL('pkg/tilo_browser_runtime.js', document.baseURI).href)`;
  await tab.evaluate(`(async () => { const r = await ${RUNTIME}; r.configure_frame_metrics(true); r.take_frame_metrics(); return true; })()`)
    .catch(() => {});
  try {
    await body();
  } finally {
    await tab.send('Page.stopScreencast').catch(() => {});
    off();
    await writing;
  }
  const ended = Date.now() / 1000;
  const seconds = +(ended - began).toFixed(1);
  const drawnAfter = await presented();
  const stages = await tab.evaluate(`(async () => {
    const r = await ${RUNTIME};
    const taken = JSON.parse(r.take_frame_metrics());
    r.configure_frame_metrics(false);
    const by = {};
    for (const mount of taken) for (const {stage, ms} of mount.samples ?? []) {
      const entry = by[stage] ??= {n: 0, ms: 0, max: 0};
      entry.n += 1; entry.ms += ms; entry.max = Math.max(entry.max, ms);
    }
    return Object.entries(by).sort((a, b) => b[1].ms - a[1].ms).slice(0, 8)
      .map(([stage, {n, ms, max}]) => stage + ' ' + n + 'x ' + Math.round(ms) + ' ms (max ' + Math.round(max) + ')').join(', ');
  })()`).catch(error => `unread (${error.message ?? error})`);
  log(`${name}: the page's frame stages while recording: ${stages || 'none measured'}`);
  log(`${name}: over ${seconds} s the page presented ${drawnBefore === null || drawnAfter === null ? 'an unknown number of'
    : drawnAfter - drawnBefore} frames and the screencast pushed ${frames.length}`);
  if (!frames.length) return {frames: 0, seconds, file: null, framesDir: dir};
  // Each frame is held for as long as the page showed it: a page that drew nothing new for two
  // seconds is two seconds of that frame, not a skipped stretch. The last stands until the end.
  for (const [index, frame] of frames.entries()) {
    const next = frames[index + 1]?.at ?? ended;
    frame.hold = Math.min(RECORD_HOLD_S, Math.max(0.001, next - frame.at));
  }
  const file = join(videoDir, `${name}.webm`);
  const encoder = encodeWithFfmpeg(frames, dir, file, name) ? 'ffmpeg'
    : await encodeInBrowser(browser, frames, file, name) ? 'browser' : null;
  if (!encoder) {
    log(`${name}: the recording was not encoded; its frames are in ${dir}`);
    return {frames: frames.length, seconds, file: null, framesDir: dir};
  }
  rmSync(dir, {recursive: true, force: true});
  return {frames: frames.length, seconds, encoder, bytes: statSync(file).size, file, framesDir: null};
}

// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// The page's memory, sampled across the run. A page that allocates without
// bound runs out long after the frame that started it — the tab traps with
// `rust_oom`, then never draws again — so a run keeps the number and says
// what it grew to, at every width, rather than hearing about it as a trap.
//
// The wasm module's own memory is what an allocator runs out of; the glue
// creates it while instantiating, so the instantiate calls are wrapped before
// the page's scripts run. The canvas is read beside it: a backing store that
// changes every frame is one way that memory grows.

/** The most wasm memory a run may hold, in bytes. Opening the workspace, the
 * apps and the notes it writes in settles at 79 MB on a 390-wide phone and
 * 115 MB on a 1280 desk, both at twice the pixel density; a whole desktop with
 * its records and pictures is under a third of this. */
const WASM_BUDGET = 384 * 1024 * 1024;
/** How much the heap may grow from the workspace to the end of the flow: the
 * warm-up above is 22 MB on the phone and 48 MB on the desk, so a page that
 * asks for megabytes a second — what allocating without bound looks like — is
 * past it long before the run ends. */
const WASM_GROWTH_ALLOWED = 128 * 1024 * 1024;

const MEMORY_SHIM = `(() => {
  const seen = globalThis.__tiloWasmMemory = {memories: []};
  for (const name of ['instantiate', 'instantiateStreaming']) {
    const original = WebAssembly[name];
    WebAssembly[name] = async function (...args) {
      const result = await original.apply(this, args);
      const memory = (result.instance ?? result).exports?.memory;
      if (memory instanceof WebAssembly.Memory) seen.memories.push(memory);
      return result;
    };
  }
})()`;
const MEMORY = `(() => {
  const canvas = globalThis.tiloMount?.canvas ?? document.querySelector('canvas');
  const state = globalThis.tiloMount?.state?.() ?? null;
  return {
    wasmBytes: (globalThis.__tiloWasmMemory?.memories ?? []).reduce((sum, memory) => sum + memory.buffer.byteLength, 0),
    jsHeap: performance.memory?.usedJSHeapSize ?? null,
    canvas: canvas ? {width: canvas.width, height: canvas.height,
      cssWidth: canvas.clientWidth, cssHeight: canvas.clientHeight, dpr: devicePixelRatio} : null,
    frames: state?.frames ?? null,
    logical: state?.logicalSize ?? null,
  };
})()`;
const megabyte = bytes => (bytes / (1024 * 1024)).toFixed(1);

/** Samples `tab`'s memory every second until the returned function stops it:
 * the shape of a growth matters as much as its peak. */
function watchMemory(tab) {
  const samples = [];
  let stopped = false;
  const sample = async reason => {
    const taken = await tab.evaluate(MEMORY).catch(() => null);
    if (taken) samples.push({at: Math.round((Date.now() - t0) / 1000), reason, ...taken});
    return taken;
  };
  const ticking = (async () => {
    while (!stopped) {
      await sample('tick');
      await sleep(1000);
    }
  })();
  return {
    samples,
    sample,
    /** The largest wasm and canvas numbers seen, and how many distinct canvas
     * sizes: a backing store that keeps changing is named in the detail. */
    async stop() {
      stopped = true;
      await ticking;
      await sample('end');
      const wasm = samples.map(entry => entry.wasmBytes ?? 0);
      const canvases = new Set(samples.map(entry => entry.canvas && `${entry.canvas.width}x${entry.canvas.height}`));
      const frames = samples.map(entry => entry.frames ?? 0);
      return {peak: Math.max(0, ...wasm), first: wasm[0] ?? 0, last: wasm.at(-1) ?? 0,
        presented: [frames[0] ?? 0, frames.at(-1) ?? 0],
        canvases: [...canvases].filter(Boolean), samples};
    },
  };
}

// The test certificate's key, which the browser takes on the host's WebTransport link too: QUIC
// verifies its certificate apart from the HTTPS stack `--ignore-certificate-errors` relaxes.
const testKeySpki = createHash('sha256')
  .update(new X509Certificate(readFileSync(certPath)).publicKey.export({type: 'spki', format: 'der'}))
  .digest('base64');
const browser = await launch({args: ['--ignore-certificate-errors',
  `--ignore-certificate-errors-spki-list=${testKeySpki}`,
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
      let memory = null;
      await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: MEMORY_SHIM});
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
        // The workspace the first screen stands in, which is the one the agent is let into.
        const firstWhere = await tab.evaluate('location.hash');
        log(`${combo}: the first screen is at ${firstWhere || '/'}`);
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
        const SCREEN = `(() => ({
          run: document.documentElement.dataset.firstRun ?? null,
          prompt: document.getElementById('first-run-prompt')?.textContent ?? '',
          problem: document.getElementById('first-run-problem')?.textContent ?? '',
          steps: document.getElementById('first-run-steps')?.textContent ?? '',
        }))()`;
        const asked = await until(async () => {
          const state = await tab.evaluate(SCREEN);
          if (/tilo login [A-Za-z0-9-]{4,}/.test(state.prompt)) return state;
          if (state.run === 'failed' || state.problem) return {failed: state};
        }, 90000, 300);
        let prompt = asked?.prompt ?? '';
        let offered = Boolean(prompt);
        if (!offered) {
          // The screen could not make the code. Say what it said, then make
          // one as the screen would (its pairing link, the host's code): the
          // run goes on, and the report carries the screen's own words.
          const state = asked?.failed ?? await tab.evaluate(SCREEN);
          log(`${combo}: the first screen made no code`, JSON.stringify(state));
          log(`${combo}: the page console`, tab.log.slice(-8).join(' | '));
          const made = await tab.evaluate(`(async () => {
            try { await globalThis.tiloIdentity?.keepStandIn?.(); } catch (error) { /* the link below says why */ }
            const made = await globalThis.tiloIdentity.connectAgent();
            return {link: made.link, expiresAt: made.expiresAt};
          })().then(made => made, error => ({error: String(error?.message ?? error), code: error?.code ?? null}))`);
          log(`${combo}: the page's pairing link`, typeof made.link === 'string'
            ? `${made.link.slice(0, 60)}…${made.link.slice(-40)}` : JSON.stringify(made));
          const answered = typeof made.link === 'string' && made.link.startsWith('https://')
            ? await postJson(`${hostOrigin}/peer/v1/login-code`, {link: made.link}) : null;
          log(`${combo}: the host answered the link with`, JSON.stringify(answered));
          let created = null;
          try { created = answered?.status === 200 ? JSON.parse(answered.body) : null; } catch { created = null; }
          note(`${combo}: the first screen offers the agent's setup command`, false,
            `${state.problem || state.run} (${JSON.stringify(state.steps)}); its pairing link ${created?.code ? `made code ${created.code}` : 'made none'}`);
          if (!created?.code) throw new Error(`the first screen made no code, and neither did its pairing link: ${JSON.stringify({state, made, answered})}`);
          // The screen's own prompt, as it would have copied it.
          prompt = `Sign me in to Tilo: run tilo login ${created.code}, then do what it tells you.`;
        }
        log(`${combo}: the tab copied “${prompt}”`);
        const code = (prompt.match(/tilo login ([A-Za-z0-9-]{4,})/) ?? [])[1];
        if (!code) throw new Error(`the prompt carried no code: ${prompt}`);
        if (offered) {
          const copied = await tab.evaluate(`Promise.race([navigator.clipboard.readText().catch(() => null),
            new Promise(resolve => setTimeout(() => resolve(null), 3000))])`);
          check(`${combo}: the click copied the prompt with the code`,
            copied === null || copied.includes(`tilo login ${code}`),
            copied === null ? 'the clipboard was not readable here' : copied.slice(0, 300));
        }
        await shot(tab, combo, 'waiting-for-agent');

        // 2. The real agent, as the prompt tells it.
        const loggedIn = await login(code);
        check(`${combo}: tilo login made this computer a device of the person`, /Logged in:/.test(loggedIn.stdout),
          loggedIn.stdout.split('\n')[0] ?? '');
        const followed = await until(async () => {
          const state = await tab.evaluate(`document.documentElement.dataset.firstRun ?? null`);
          return state === 'connected' || state === null || state === 'failed' ? state ?? 'left' : undefined;
        }, 90000, 200);
        if (offered) {
          check(`${combo}: the tab followed the agent into the workspace`,
            followed === 'connected' || followed === 'left', String(followed));
        } else {
          // The screen that could not make a code stays as it was: what it
          // shows is the product's, and the run says so rather than passing.
          note(`${combo}: the tab followed the agent into the workspace`, false,
            `the first screen stayed ${followed}, having made no code`);
        }
        if (!offered) {
          // The screen made no code, so it never leaves by itself: its own
          // "Skip for now" takes the person into the workspace.
          await tab.evaluate(`document.getElementById('first-run-skip')?.click()`);
          await sleep(1500);
        }
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
        if (!/#\/(?:.*\/)?w\//.test(where)) {
          log(`${combo}: the person's own desktop (${where || '/'}); opening the workspace where the agent is`);
          await tab.navigate(`${siteOrigin}/${/#\/(?:.*\/)?w\//.test(firstWhere) ? firstWhere : ''}`);
          const back = await until(async () => {
            const page = await tab.evaluate(PAGE);
            return page.entry === 'open' && page.mount ? page : undefined;
          }, 240000, 300);
          if (!back) throw new Error(`the workspace did not open again: ${JSON.stringify(await tab.evaluate(PAGE))}`);
        }
        await sleep(2000);
        log(`${combo}: in the workspace`, await tab.evaluate(`(globalThis.tiloMount?.state()?.workspaceName ?? "") + " " + location.hash`));
        // From the workspace on, the page's memory is sampled: what it grows
        // to over the run is the number the check below is about.
        memory = watchMemory(tab);
        await memory.sample('landed');

        // The desk itself, before anything opens over it: its search takes the keyboard when the
        // chord asks for it, and the first press beside the card puts it away again.
        const searchProblem = await pressBesideSearch(tab, {height});
        check(`${combo}: a press beside system search dismisses it`, !searchProblem, searchProblem ?? '');

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
        // A first visit has chosen neither light nor dark, so the shell
        // draws the one its browser is in; the shot's tone says which.
        const drew = await luminance(scratch, tab);
        check(`${combo}: the shell draws this scheme`, drew !== null && (theme === 'light' ? drew > 0.5 : drew < 0.5),
          `prefers-color-scheme is ${theme} in this tab (emulated before load); the shot's mean luminance is ${drew === null ? 'unreadable' : drew.toFixed(2)}`);
        // The person's system flips under the page: it follows, and draws
        // the other scheme without a reload.
        const other = theme === 'light' ? 'dark' : 'light';
        const tone = async wanted => {
          const value = await luminance(scratch, tab);
          return value !== null && (wanted === 'light' ? value > 0.5 : value < 0.5) ? value : undefined;
        };
        const emulate = value => tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value}]});
        await emulate(other);
        const flipped = await until(() => tone(other), 15000, 300);
        check(`${combo}: the shell follows the system as it flips`, flipped !== undefined,
          flipped === undefined ? `the page still draws at a mean luminance of ${await luminance(scratch, tab)} after ${other}` : `the mean luminance is ${flipped.toFixed(2)}`);
        await shot(tab, combo, `flipped-${other}`);
        await emulate(theme);
        await until(() => tone(theme), 15000, 300);

        // Following left who is here open beside the note, over the words the agent is about to
        // write: the person closes it, as their own press closes it, before reading the note.
        const stillOpen = await closePeople(tab);
        check(`${combo}: the person can put who is here away to read the note`, stillOpen === null,
          stillOpen ?? '');

        // 5. A few of the agent's actions, with the person following: the
        //    recording, and `following-mid-edit` from the first write.
        const steps = await planActions(agent, apps);
        // The steps up to the first write and its shot are shot at the page's own scale; the
        // recording is of the ones after it (the agent writing on, the person following), drawn at
        // one pixel per CSS pixel. The builder renders the page in software, and at twice the
        // pixels a frame took seconds: a recording of that was a handful of frames, and a still
        // shot taken beside it held the page long enough for the agent's write to come back
        // unfinished.
        const recordFrom = Math.min(steps.length, steps.shootAt + 1);
        const pacing = async (from, to) => {
          for (const [index, step] of steps.entries()) {
            if (index < from || index >= to) continue;
            if (index === steps.shootAt) await watchAgentCursor(tab);
            const sentAt = Date.now();
            const sending = agent.call(step.tool, step.args);
            if (index === steps.shootAt) {
              // The edit is in flight. Following brings the note's window forward, and the frame
              // says which window that is: the cursor is read only once the note it writes in is
              // the window in front, so the shot is of the note at work and not of the console
              // still covering it.
              // The agent's cursor lands on the text as the write commits, and the words it
              // changed stay lit for a second and a half: read it the moment it is in the frame's
              // semantics, never after a fixed wait. Whether the note is the window drawn last is
              // said in the log: the frame's order of windows moves as they are redrawn, and a
              // shot of the note behind the console is still a shot of the note.
              const showing = await until(async () => (await seenAgentCursor(tab)) ?? undefined, 15000, 150);
              await stopWatchingAgentCursor(tab);
              log(`${combo}: mid-edit, the note is ${noteInFront(showing ?? []) ? '' : 'not '}the window drawn last`);
              const cursor = showing ? agentCursor(showing) : undefined;
              check(`${combo}: the agent's cursor stands on what it writes`, Boolean(cursor),
                showing ? `the frame shows ${names(showing)}`
                  : `the page published no agent cursor while the write was in flight; what it says of people at work: ${
                    JSON.stringify((await semantics(tab)).filter(node => /writing|selecting|claude code\s*:|^agent\s*:/i.test(node.name))
                      .map(node => ({role: node.role, name: node.name, rect: node.rect})))}: ${names(await semantics(tab))}`);
              log(`${combo}: mid-edit shot in the note` +
                `${cursor ? `, its cursor at ${JSON.stringify(cursor.rect)}` : ', with no agent cursor'}`);
              // Following brings forward the window that shows the note: never a second one
              // beside it. The windows are their tabs, named by what they show.
              const title = new RegExp(`^${(steps.noteTitle ?? 'launch notes').replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`, 'i');
              const noteWindows = (showing ?? await semantics(tab)).filter(node => node.role === 'Tab' && title.test(node.name));
              check(`${combo}: following shows the note in one window`, noteWindows.length <= 1,
                `${noteWindows.length} windows show it: ${noteWindows.map(node => JSON.stringify(node.rect)).join(', ')}`);
              await shot(tab, combo, 'following-mid-edit', {read: showing,
                expect: [new RegExp(`^${(steps.noteTitle ?? 'launch notes').replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`, 'i')]});
            }
            const first = await sending;
            const answers = [verdict(first)];
            const answer = await settled(agent, first, said => answers.push(verdict(said)));
            step.answer = answer;
            calls.push({combo, step: step.label, tool: step.tool, args: step.args, answers,
              answer: verdict(answer), ms: Date.now() - sentAt, said: answer.text.slice(0, 300)});
            writeSummary();
            log(`${combo}: agent ${step.label} -> ${answers.join(' -> ')} ${answer.text.slice(0, 300)}`);
            // The person sees each change land before the agent makes the next: the page draws it
            // on its next frame, which on a software-drawn page is seconds away, and a call sent
            // sooner lands in the same frame as this one, so the follow shows two lines at once.
            await afterFrames(tab, 1, 15000);
            await sleep(600);
          }
        };
        await pacing(0, recordFrom);
        const scaled = scale => tab.send('Emulation.setDeviceMetricsOverride',
          {width, height, deviceScaleFactor: scale, mobile});
        await scaled(1);
        await sleep(1500);
        const video = await record(browser, tab, combo, {width, height}, () => pacing(recordFrom, steps.length));
        await scaled(2);
        await sleep(1500);
        // The shots are of the agent working: an action it could not do is
        // the run's failure, not a note about the product.
        for (const step of steps) {
          check(`${combo}: the agent ${step.label}`, Boolean(step.answer) && verdict(step.answer) === 'ok',
            step.answer ? `${verdict(step.answer)}: ${step.answer.text.slice(0, 400)}` : 'it never ran');
        }
        if (video) log(`${combo}: recording`, JSON.stringify(video));
        // The agent's card lists the run it just made: a row under its Runs, where it said "No
        // runs yet". Following opens it beside the work when the page saw the agent arrive;
        // otherwise the person opens it from the agent's face in the bar.
        const cardSays = nodes => {
          const heading = nodes.findIndex(node => /^runs$/i.test(node.name) && /heading/i.test(JSON.stringify(node.role)));
          if (heading >= 0) return nodes.slice(heading + 1).find(node => /listitem/i.test(JSON.stringify(node.role)) && node.name) ?? {name: 'Runs, with none listed'};
          return nodes.find(node => /^no runs yet$/i.test(node.name));
        };
        const listsRun = said => Boolean(said) && !/^(no runs yet|runs, with none listed)$/i.test(said.name);
        const openCard = async () => {
          if (cardSays(await semantics(tab))) return;
          // The face's card may stand already (a face pressed again brings everyone here in its
          // place): its Open card is pressed where it is. Otherwise the face opens it, and Open
          // card is pressed once a frame drew the card, not where an earlier frame had it. While
          // the person follows the agent on a phone, the bar folds the banner into the agent's
          // face beside Stop: that face is the one the bar shows, pressed at its leading edge,
          // away from Stop.
          if (!matching(await semantics(tab), /^open card$/i)) {
            const before = await presentedFrames(tab);
            const banner = matching(await semantics(tab), /^following claude code$/i);
            const face = await press(tab, /^claude code, agent$/i, {what: "the agent's face"});
            if (!face && banner?.rect) {
              const {x0, y0, y1} = banner.rect;
              await clickAt(tab, x0 + (y1 - y0) / 2, (y0 + y1) / 2);
              log(`pressed the agent's face in the following banner at ${Math.round(x0 + (y1 - y0) / 2)},${Math.round((y0 + y1) / 2)}`);
            } else if (!face) {
              log(`the bar shows no face of the agent to open its card from: ${names(await semantics(tab))}`);
            }
            await untilDrawn(tab, async () => ((await presentedFrames(tab)) ?? 0) > (before ?? 0)
              && matching(await semantics(tab), /^open card$/i), 3000, 200);
          }
          if (!await press(tab, /^open card$/i, {what: "the agent's card"})) {
            log(`the agent's card offers no Open card: ${names(await semantics(tab))}`);
          }
        };
        await openCard();
        // Console opens the card in a window of its own: on a page drawing a frame every few
        // seconds, that is several frames, so the wait is counted in frames, not seconds.
        const card = await untilDrawn(tab, async () => {
          const said = cardSays(await semantics(tab));
          return listsRun(said) ? said : undefined;
        }, 20000, 200, WINDOW_FRAMES);
        const cardNow = cardSays(await semantics(tab));
        check(`${combo}: the agent's card lists its run`, Boolean(card),
          card ? card.name : cardNow ? `the card says "${cardNow.name}"`
            // The windows standing, by their tabs: what the desk holds at once, which the first
            // names of the tree (one window's controls) do not say.
            : `no card; the windows are ${(await semantics(tab)).filter(node => node.role === 'Tab').map(node => `"${node.name}"`).join(', ')}`);
        await shot(tab, combo, 'agent-card');

        // 6. Who is here, kept on screen, with the agent in it.
        const pinProblem = await pinOverlay(tab);
        check(`${combo}: who is here is kept on screen`, !pinProblem, pinProblem ?? '');
        await shot(tab, combo, 'presence-with-agent', {expect: [/^everyone here$/i, /claude code/i]});
        // The line beside the agent's name is what a person reads of its work: the record it is
        // in and the app. The field a write landed in is the tool's own name for a part of that
        // record, and no name a person reads.
        const status = (await semantics(tab)).find(node => /^agent;\s*\S/i.test(node.description ?? ''));
        note(`${combo}: who is here says where the agent is, in a person's words`, Boolean(status),
          status ? status.description : `no row described it: ${names(await semantics(tab))}`);
        check(`${combo}: who is here names the record and its app, never the field written in`,
          Boolean(status) && !/\bBody of\b|=|#|\//i.test(status.description),
          status ? status.description : names(await semantics(tab)));
        // Kept on screen, it stays while the person works elsewhere: a press on the desk is the
        // desk's, and the list is still there after it. The desk's bottom edge, which is the desk
        // at every width — the panel is a bottom sheet on a phone, and a press on its invite
        // button is not a press elsewhere.
        await clickAt(tab, Math.round(width / 2), Math.round(height - 12));
        await afterFrames(tab, 2);
        check(`${combo}: who is here stays while the desk is pressed`, Boolean(rosterOf(await semantics(tab))),
          names(await semantics(tab)));
        // A window filling the desk: the bar's own controls and the reading it shows stand apart at
        // every width, the phone's included.
        if (await press(tab, /^maximize$/i, {what: 'maximizing the window'})) {
          // The window's control is Restore once it fills the desk.
          await untilDrawn(tab, async () => matching(await semantics(tab), /^restore$/i), 900);
          await shot(tab, combo, 'maximized');
          await press(tab, /^(restore|maximize)$/i, {what: 'restoring the window'});
          await untilDrawn(tab, async () => matching(await semantics(tab), /^maximize$/i), 600);
        }
        const overlay = await semantics(tab);
        note(`${combo}: the overlay lists the agent`, overlay.some(node => /claude code/i.test(node.name)), names(overlay));
        // Back in the bar: offered, the control puts the list back; an overlay
        // that offers none is said as it is, with the controls it does offer.
        const back = await unpinned(tab);
        if (back === undefined) {
          note(`${combo}: who is here goes back in the bar`, false,
            `the pinned overlay offers no "Put back in the bar" control: ${names(await semantics(tab))}`);
        } else {
          check(`${combo}: who is here goes back in the bar`, Boolean(back),
            back ? 'pressed, and the list closed' : names(await semantics(tab)));
        }
        // 7. Once the agent leaves, the person is told what came of it ("Claude Code finished:
        //    3 changes in Workspace"): it is finished once it stayed away longer than a reconnect.
        // The agent's face in the bar, or the banner of following it, is it still here: once its
        // tilo mcp ends it leaves who is here as its session closes, not after the page gave up
        // waiting for its word.
        await agent.close();
        const closedAt = Date.now();
        // What the page says of the agent: its face in the bar, or the banner of following it. A
        // phone's bar has room for one face, and it is whoever did something last, so an agent
        // that comes back is on the desk where the person looks, at every width.
        const presentAs = async () => (await semantics(tab)).find(node =>
          /^claude code, agent$/i.test(node.name)
          || /^following claude code/i.test(node.name));
        const present = async () => Boolean(await presentAs());
        // What still says the agent is here, where it should have gone: its face, or the banner.
        const stillSays = async () => {
          const node = await presentAs();
          return node ? `"${node.name}" (${node.role}) at ${JSON.stringify(node.rect)}` : 'nothing';
        };
        const gone = await until(async () => (await present()) ? undefined : true, 30000, 250);
        const leftAfter = (Date.now() - closedAt) / 1000;
        log(`${combo}: the agent left who is here ${leftAfter.toFixed(1)} s after its tilo mcp ended`);
        check(`${combo}: the agent leaves who is here once its tilo mcp ends`, Boolean(gone) && leftAfter < 10,
          gone ? `${leftAfter.toFixed(1)} s after tilo mcp ended` : `still listed 30 s after, by ${await stillSays()}: ${names(await semantics(tab))}`);
        // 7b. The agent comes back: `tilo login` tells it to restart, and its client restarts its
        //     MCP server often. Each new tilo mcp on this computer is on the page that stayed
        //     open, with what it is doing: once after the last one ended, and once straight
        //     after the last one was killed, before the host could notice it went.
        for (const [round, ending] of [[1, 'ended'], [2, 'killed']]) {
          agent = mcpClient();
          const startedAt = Date.now();
          await agent.initialize();
          await agent.call('tilo_ls');
          const back = await until(async () => (await present()) || undefined, 30000, 250);
          const backAfter = (Date.now() - startedAt) / 1000;
          log(`${combo}: tilo mcp run ${round + 1} is on the open page ${backAfter.toFixed(1)} s after it started`);
          check(`${combo}: a tilo mcp started again is on the open page (after the last ${ending === 'ended' ? 'ended' : 'one ended'})`,
            Boolean(back) && backAfter < 15,
            back ? `${backAfter.toFixed(1)} s after it started` : `not listed 30 s after it started: ${names(await semantics(tab))}`);
          if (ending === 'killed') {
            agent.child.kill('SIGKILL');
            await until(() => agent.child.exitCode !== null || agent.child.signalCode !== null || undefined, 5000, 50);
          } else {
            await agent.close();
            const left = await until(async () => (await present()) ? undefined : true, 30000, 250);
            check(`${combo}: tilo mcp run ${round + 1} leaves who is here once it ends`, Boolean(left),
              left ? 'gone' : `still listed 30 s after, by ${await stillSays()}: ${names(await semantics(tab))}`);
          }
        }
        // The killed run's successor: on the page though its predecessor never said it left.
        agent = mcpClient();
        const restartedAt = Date.now();
        await agent.initialize();
        await agent.call('tilo_ls');
        const afterKill = await until(async () => (await present()) || undefined, 30000, 250);
        const afterKillIn = (Date.now() - restartedAt) / 1000;
        log(`${combo}: tilo mcp after a killed one is on the open page ${afterKillIn.toFixed(1)} s after it started`);
        // The killed run is listed until the host notices it went, so the agent is still listed
        // well after that only if the page hears its successor.
        await sleep(35000);
        const stays = await present();
        check(`${combo}: a tilo mcp started right after one was killed is on the open page`,
          Boolean(afterKill) && afterKillIn < 15 && stays,
          !afterKill ? `not listed 30 s after it started: ${names(await semantics(tab))}`
            : stays ? `${afterKillIn.toFixed(1)} s after it started, still listed 35 s later`
              : `listed ${afterKillIn.toFixed(1)} s after it started, gone 35 s later with its killed predecessor`);
        await agent.close();
        const finalLeft = await until(async () => (await present()) ? undefined : true, 30000, 250);
        check(`${combo}: the last tilo mcp leaves who is here once it ends`, Boolean(finalLeft),
          finalLeft ? 'gone' : `still listed 30 s after, by ${await stillSays()}: ${names(await semantics(tab))}`);
        const finishedNotice = node => /claude code finished/i.test(`${node.name} ${node.description ?? ''}`);
        const finished = await until(async () => (await semantics(tab))
          .find(node => /claude code finished: \d+ changes?/i.test(`${node.name} ${node.description ?? ''}`)), 150000, 1000);
        if (!finished) log(`${combo}: the finished notice`, (await semantics(tab)).filter(finishedNotice).map(node => node.name).join(' | ') || 'none');
        check(`${combo}: the person is told what the agent changed once it stops`, Boolean(finished),
          finished ? finished.name : names(await semantics(tab)));
        // The notice comes in with a fade: shot once it stands, when the page has stopped
        // drawing (a page draws only while something moves), not part way in.
        await atRest(tab);
        await shot(tab, combo, 'agent-finished');

        const exceptions = tab.log.filter(line => line.startsWith('EXC') || /panic/i.test(line));
        check(`${combo}: no exceptions in the page`, exceptions.length === 0, exceptions.join(' | '));

        const used = await memory.stop();
        writeFileSync(join(browserDir, `${combo}-memory.json`), JSON.stringify(used, null, 1));
        check(`${combo}: the page's memory stays under ${megabyte(WASM_BUDGET)} MB`, used.peak < WASM_BUDGET,
          `wasm ${megabyte(used.first)} MB at the workspace, ${megabyte(used.peak)} MB at its peak, ` +
            `${megabyte(used.last)} MB at the end; canvases ${used.canvases.join(', ') || 'none'}`);
        check(`${combo}: the page's memory grows by no more than ${megabyte(WASM_GROWTH_ALLOWED)} MB`,
          used.peak - used.first < WASM_GROWTH_ALLOWED,
          `wasm ${megabyte(used.first)} MB at the workspace, ${megabyte(used.peak)} MB at its peak ` +
            `over ${used.samples.length} samples`);
        // A page that trapped still answers its input, and stops presenting: the frames it
        // counts are how a wedged page reads from here, whatever trap wedged it.
        check(`${combo}: the page keeps presenting`, used.presented[1] > used.presented[0] + 10,
          `${used.presented[0]} frames at the workspace, ${used.presented[1]} at the end`);
        log(`${combo}: memory`,
          `wasm peak ${megabyte(used.peak)} MB, ${used.presented[1] - used.presented[0]} frames, canvases ${used.canvases.join(', ')}`);

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
        await memory?.stop().catch(() => {});
        await agent?.close().catch(() => {});
        await tab.close().catch(() => {});
      }
    }
  }
} finally {
  // Written before the clean-up, which waits on processes, and again after it.
  writeSummary();
  for (const entry of [...children].reverse()) await stop(entry);
  site.close();
  await browser.close().catch(() => {});
  writeSummary();
}
const failures = checks.filter(entry => !entry.pass);
console.log(`\n${checks.length - failures.length}/${checks.length} checks passed; shots under ${shotsDir}`);
if (notes.length) console.log(`what the product does not do today:\n${notes.map(entry => `- ${entry.pass ? '(now it does) ' : ''}${entry.id}: ${entry.detail}`).join('\n')}`);
if (failures.length) {
  console.error(`${failures.length} failed:\n${failures.map(failure => `- ${failure.id}: ${failure.detail}`).join('\n')}`);
  process.exit(1);
}
