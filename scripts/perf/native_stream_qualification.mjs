#!/usr/bin/env node
// A native window streamed to the browser desktop, qualified end to end: a
// session's legacy window, encoded by the production encoder (H.264, or --codec), through
// a real host's legacy-window bridge and live plane, onto a desktop bundle in
// headless Chrome that decodes it with WebCodecs (legacy-windows.mjs) and
// draws it over the window's tile. It measures what a person sees and checks
// that what they see is right. It writes one report; `tools/perf/adapters.py
// native-stream` turns it into the `stream` KPI group's records
// (t's perf-stream, tools/perf/kpi-budgets.toml).
//
// usage: node scripts/perf/native_stream_qualification.mjs BUNDLE_DIR --out REPORT_JSON
//          --relay TILO_RELAY --source LEGACY_STREAM_SOURCE --renderer gpu|swiftshader
//          [--seconds S] [--window WxH] [--fps N] [--inputs N] [--carrier webtransport|https] [--warm-up S]
//          [--codec h264|vp9|av1] [--reopen] [--platform chrome|safari]
//          [--viewport WxH] [--expect-revision SHA] [--allow-dev] [--max-load L] [--enforce-goals]
//
// The pieces, all built from one commit:
//   host     `tilo-relay dev` (feature web): the browser listener, the live
//            plane and the legacy-window bridge (LEGACY_WINDOWS.md)
//   session  `legacy_stream_source` (tilo-relay's example, feature
//            stream-source): a device of the host's user that grants the host
//            its window and streams it as the shell's share session does,
//            through tilo_media::RealtimeEncoder with the shell's legacy-share
//            configuration; only the Wayland capture is replaced, by a drawn
//            window whose every frame carries its number in black and white
//            blocks (the session's `draw_code`)
//   page     the desktop bundle (build.sh --bundle desktop) as a certified
//            browser device of the same user in the same room, through the
//            production peer.mjs and host.mjs, in Chrome (headless, over the
//            DevTools protocol) or in Safari (--platform safari, over
//            WebDriver: macOS, a logged-in user, `safaridriver --enable`
//            once, and a person to accept the dev host's certificate)
//
// The room is one the host governs, as a shared workspace is: the page
// creates it and enters it as its owner's device, and the session enters it
// as another, so its roles decide who drives the window.
//
// The run, in order:
//   reopen    with --reopen, a first window streams 4 s and closes before
//             the measured one opens, on a desktop then at rest: the desktop
//             must rest under each, the window after another as under the
//             first (a desktop once presented a frame for every picture
//             of a window opened after another had closed)
//   startup   the session starts its stream (grant, stream start) while the
//             page shows the room's desktop: to the first presented picture
//             of the window, visible on the desktop
//   warm-up   with --warm-up S (default 10 on SwiftShader, 0 on a GPU) the
//             window streams S seconds more, judged but not timed: a software
//             rasterizer compiles the pipelines a window's tile needs when
//             the desktop first draws one, blocking the page for seconds that
//             are the rasterizer's, not the stream's
//   stream    the window repaints at the session's rate for --seconds: every
//             picture the page draws is read back (its number) and timed
//             against the moment the session captured that frame
//   converge  the window goes still: the page must end on its last frame
//   input     --inputs clicks on the window: from the page's click to the
//             session receiving the press
//   accept    the acceptance cases (native_stream_acceptance.mjs): focus,
//             paste, IME, resize, the HTTPS fallback when WebTransport fails, a
//             viewer who watches but cannot drive, recovery from a dropped
//             stream and typing that never reaches a stale window, with a
//             second window, a second device of the owner and a second person
//
// Safari's run (--platform safari) is the second browser a qualification
// needs beside Chrome: its WebDriver drives one window, which is other
// people's, with no browser context and no input method, and Safari has no
// WebTransport, so it takes --carrier https. A Safari report names its driver
// (`browser.driver`) and the cases it could not drive; tools/perf/adapters.py
// refuses one whose carrier or codec is not what the run asked for.
//
// Correctness, judged on every run (a miss exits 1, with the report): no
// decoder error; the desktop under the window presents at most
// MAX_DESKTOP_FPS frames a second while the window streams (its pictures go
// to the window's own canvas, never through the desktop, and the report
// names what asked for the frames it did present); every drawn picture's
// number reads back with a valid check (no corrupt picture) and is the frame
// its timestamp names; pictures never go back to an older frame; the page ends on the last frame sent; this member
// drives the window (it is the session's own user) and every press arrives,
// in order; and every acceptance case holds.
//
// Goals (reported; enforced only with --enforce-goals, which a hardware
// qualification uses):
//   startup        <= 1000 ms to the first picture (an uninterrupted flow)
//   frame latency  p95 <= 100 ms capture to presented picture
//   delivery       >= 99 % of the frames sent after the first picture drawn
//   input          p95 <= 50 ms click to the session
//
// The page reads a picture back as it draws it (a 3-row getImageData of the
// window's canvas), which adds that readback to the page's work; the number
// blocks are drawn into the window, so the bitstream is the session's own.
//
// The run is invalid, and exits 2 without a report, when it cannot observe
// its subject: the bundle's bytes differ from its build.json or it is not
// the expected build, the browser decodes no --codec, the WebGPU adapter is not
// --renderer, the host was busier than --max-load before or after, a piece
// did not start, or the page never drew a picture of the window.
import {spawn} from 'node:child_process';
import {randomUUID} from 'node:crypto';
import {createReadStream, existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync} from 'node:fs';
import {createServer} from 'node:http';
import os from 'node:os';
import {extname, join, normalize, resolve} from 'node:path';
import {createInterface} from 'node:readline';
import {launch as launchChrome, sleep} from '../../tools/web/chrome.mjs';
import {Unavailable, launch as launchSafari} from '../../tools/web/webdriver.mjs';
import {VIDEO_DECODER_CONFIGS} from '../../crates/browser/tilo-browser-runtime/web/legacy-windows.mjs';
import {Invalid, RENDERERS, chromeFlags, classifyAdapter, hostState, parseViewport, stats, verifyBundle, within}
  from './browser_qualification.mjs';
import {runAcceptance} from './native_stream_acceptance.mjs';

export const GOALS = {startupMs: 1000, latencyP95Ms: 100, deliveredPercent: 99, inputP95Ms: 50};
/** Most frames a second the desktop under a streaming window may present
 * (its opening and the odd status change, never one per picture). */
export const MAX_DESKTOP_FPS = 2;
/** The surfaces of the window a --reopen run opens and closes first, and of the measured one. */
export const EARLIER_SURFACE = 6;
/** How long that first window streams. */
const EARLIER_SECONDS = 4;
export const MEASURED_SURFACE = 7;
export const CARRIERS = ['webtransport', 'https'];
/**
 * The browsers a run drives: Chrome over CDP (tools/web/chrome.mjs) and
 * Safari over WebDriver (tools/web/webdriver.mjs). Safari runs on macOS
 * alone, and has no WebTransport, so it takes `--carrier https`, and an
 * input method and a clipboard cannot be driven from its driver.
 */
export const PLATFORMS = ['chrome', 'safari'];
/** The stream's codec (`--codec`): VP9 and AV1 are what a session without an
 * H.264 encoder streams once every member's page decodes them. */
export const CODECS = Object.keys(VIDEO_DECODER_CONFIGS);
// The session's number blocks (legacy_stream_source.rs `CODE_BITS`, `code_geometry`).
export const CODE_BITS = 20;
const VIEWPORT = {width: 1280, height: 800, deviceScaleFactor: 1};
const HARNESS_PATH = '/native-stream-qualification.html';
const USAGE = 'usage: native_stream_qualification.mjs BUNDLE_DIR --out REPORT_JSON --relay BIN --source BIN '
  + '--renderer gpu|swiftshader [--seconds S] [--window WxH] [--fps N] [--inputs N] [--carrier webtransport|https] [--warm-up S] '
  + '[--codec h264|vp9|av1] [--reopen] [--platform chrome|safari] [--viewport WxH] [--expect-revision SHA] [--allow-dev] [--max-load L] [--enforce-goals]';

export function parseArgs(argv) {
  const options = {seconds: 10, inputs: 20, carrier: 'webtransport', codec: 'h264', allowDev: false, enforceGoals: false, reopen: false,
    platform: 'chrome', window: {width: 1280, height: 720}};
  const positionals = [];
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const number = () => {
      const value = Number(argv[++i]);
      if (!Number.isFinite(value) || value <= 0) throw Error(`${arg} needs a positive number`);
      return value;
    };
    if (arg === '--out') options.out = argv[++i];
    else if (arg === '--relay') options.relay = argv[++i];
    else if (arg === '--source') options.source = argv[++i];
    else if (arg === '--renderer') options.renderer = argv[++i];
    else if (arg === '--carrier') options.carrier = argv[++i];
    else if (arg === '--codec') options.codec = argv[++i];
    else if (arg === '--seconds') options.seconds = number();
    else if (arg === '--fps') options.fps = number();
    else if (arg === '--inputs') options.inputs = number();
    else if (arg === '--warm-up') options.warmUp = Number.isFinite(Number(argv[i + 1])) && Number(argv[i + 1]) >= 0 ? Number(argv[++i])
      : (() => { throw Error('--warm-up needs seconds, 0 for none'); })();
    else if (arg === '--max-load') options.maxLoad = number();
    else if (arg === '--window') {
      const match = /^(\d{3,4})x(\d{2,4})$/.exec(argv[++i] ?? '');
      if (!match || Number(match[1]) % 2 || Number(match[2]) % 2) throw Error('--window is an even WIDTHxHEIGHT, for example 1280x720');
      options.window = {width: Number(match[1]), height: Number(match[2])};
    } else if (arg === '--viewport') options.viewport = parseViewport(argv[++i]);
    else if (arg === '--expect-revision') options.expectRevision = argv[++i];
    else if (arg === '--allow-dev') options.allowDev = true;
    else if (arg === '--reopen') options.reopen = true;
    else if (arg === '--platform') options.platform = argv[++i];
    else if (arg === '--enforce-goals') options.enforceGoals = true;
    else if (arg.startsWith('--')) throw Error(`unknown option ${arg}\n${USAGE}`);
    else positionals.push(arg);
  }
  if (positionals.length !== 1 || !options.out || !options.relay || !options.source || !options.renderer) throw Error(USAGE);
  if (!RENDERERS.includes(options.renderer)) throw Error(`--renderer must be one of ${RENDERERS.join(', ')}`);
  if (!CARRIERS.includes(options.carrier)) throw Error(`--carrier must be one of ${CARRIERS.join(', ')}`);
  if (!PLATFORMS.includes(options.platform)) throw Error(`--platform must be one of ${PLATFORMS.join(', ')}`);
  if (!CODECS.includes(options.codec)) throw Error(`--codec must be one of ${CODECS.join(', ')}`);
  for (const name of ['inputs']) if (!Number.isInteger(options[name])) throw Error(`--${name} must be a whole number`);
  if (options.fps !== undefined && !Number.isInteger(options.fps)) throw Error('--fps must be a whole number');
  // A software rasterizer compiles the desktop's pipelines on the CPU before
  // its first frame.
  options.frameTimeout = options.renderer === 'gpu' ? 10 : 120;
  options.warmUp ??= options.renderer === 'gpu' ? 0 : 10;
  options.viewport ??= VIEWPORT;
  return {...options, bundle: resolve(positionals[0]), relay: resolve(options.relay), source: resolve(options.source)};
}

/** The number blocks' pitch, side and offset in a picture `width` wide (the session's `code_geometry`). */
export function codeGeometry(width) {
  const pitch = Math.floor(width / (CODE_BITS + 1));
  return {pitch, side: Math.floor(pitch * 4 / 5), offset: Math.floor(pitch / 4)};
}

/** The 4-bit check of a frame number (the session's `code_check`). */
export function codeCheck(index) {
  const high = index >> 8 & 0xff, low = index & 0xff;
  return ((high >> 4) ^ (high & 0xf) ^ (low >> 4) ^ (low & 0xf) ^ 0xa) & 0xf;
}

/**
 * The frame number in `rows` (RGBA, `rows.length / 4 / width` rows through
 * the blocks' centres), or null when its check does not match: a corrupt
 * picture. Runs in the page too (its source is inlined into the probe).
 */
export function readCode(rows, width) {
  const pitch = Math.floor(width / 21), side = Math.floor(pitch * 4 / 5), offset = Math.floor(pitch / 4);
  const height = Math.floor(rows.length / 4 / width);
  if (pitch < 4 || height < 1) return null;
  let bits = 0;
  for (let bit = 0; bit < 20; bit++) {
    const centre = offset + bit * pitch + Math.floor(side / 2);
    let sum = 0, n = 0;
    for (let y = 0; y < height; y++) {
      for (let x = centre - 1; x <= centre + 1; x++) {
        const at = (y * width + x) * 4;
        sum += (rows[at] + rows[at + 1] + rows[at + 2]) / 3; n++;
      }
    }
    bits = bits * 2 + (sum / n > 128 ? 1 : 0);
  }
  const index = Math.floor(bits / 16), check = bits % 16;
  const high = index >> 8 & 0xff, low = index & 0xff;
  return (((high >> 4) ^ (high & 0xf) ^ (low >> 4) ^ (low & 0xf) ^ 0xa) & 0xf) === check ? index : null;
}

// --- in the page --------------------------------------------------------------

// Installed before the page's scripts: every picture a legacy window's canvas
// draws (its number read back, when, whether the window was on screen), and
// what the page's video decoders did.
//
// It also keeps the page's WebTransport sessions, so the harness can drop the
// live plane under the page (`__tiloDropLive()`, as a network that loses the
// path does), and with `__tiloBlockWebTransport` set before it (a page on a
// network that blocks UDP), every WebTransport session goes to a port nothing
// answers on and never becomes ready. Each canvas a window is drawn on is an
// epoch: a window the page opens again (after its live plane came back) is
// drawn on a new one.
//
// A browser that has no WebTransport at all (Safari's) has none to wrap, and
// the page takes the HTTPS stream it offers instead.
const PROBE = `(() => {
  const readCode = ${readCode.toString()};
  const probe = globalThis.__tiloStream = {draws: [], decoder: {created: 0, configs: [], chunks: 0, keys: 0, outputs: 0, errors: []},
    transport: {attempts: 0, blocked: 0, dropped: 0}, pastes: 0};
  const transports = [];
  const NativeTransport = globalThis.WebTransport;
  if (NativeTransport) {
    globalThis.WebTransport = class extends NativeTransport {
      constructor(url, options) {
        probe.transport.attempts++;
        const blocked = globalThis.__tiloBlockWebTransport === true;
        if (blocked) { const dead = new URL(url); dead.port = '9'; url = dead.href; probe.transport.blocked++; }
        super(url, options);
        if (!blocked) transports.push(this);
      }
    };
  }
  globalThis.__tiloDropLive = () => {
    for (const session of transports.splice(0)) { try { session.close({closeCode: 0, reason: 'path lost'}); probe.transport.dropped++; } catch {} }
    return probe.transport.dropped;
  };
  addEventListener('paste', event => { probe.pastes++; probe.pasted = event.clipboardData?.getData('text/plain') ?? null; }, true);
  const epochs = new WeakMap();
  let epochCount = 0;
  const Native = globalThis.VideoDecoder;
  if (Native) {
    globalThis.VideoDecoder = class extends Native {
      constructor(init) {
        super({output: frame => { probe.decoder.outputs++; init.output(frame); },
          error: error => { probe.decoder.errors.push(String(error?.message ?? error)); init.error(error); }});
        probe.decoder.created++;
      }
      configure(config) { probe.decoder.configs.push(config.codec); return super.configure(config); }
      decode(chunk) { probe.decoder.chunks++; if (chunk.type === 'key') probe.decoder.keys++; return super.decode(chunk); }
    };
  }
  const drawImage = CanvasRenderingContext2D.prototype.drawImage;
  CanvasRenderingContext2D.prototype.drawImage = function (image, ...rest) {
    const result = drawImage.call(this, image, ...rest);
    const canvas = this.canvas;
    if (canvas?.classList?.contains('tilo-legacy-window') && typeof VideoFrame !== 'undefined' && image instanceof VideoFrame
      && probe.draws.length < 200000) {
      const at = performance.now();
      const width = canvas.width, pitch = Math.floor(width / 21);
      const y = Math.floor(pitch / 4) + Math.floor(Math.floor(pitch * 4 / 5) / 2) - 1;
      let index = null;
      try { index = readCode(this.getImageData(0, y, width, 3).data, width); } catch {}
      if (!epochs.has(canvas)) epochs.set(canvas, ++epochCount);
      probe.draws.push({wallUs: Math.round((performance.timeOrigin + at) * 1000), tsUs: image.timestamp, index,
        visible: canvas.style.display === 'block', width, height: canvas.height, key: canvas.dataset.key, epoch: epochs.get(canvas)});
    }
    return result;
  };
})();`;

const harnessPage = () => `<!doctype html>
<meta charset="utf-8">
<title>native stream qualification</title>
<style>html,body{margin:0;height:100%;background:#111}#surface{position:relative;width:100vw;height:100vh}</style>
<div id="surface"></div>
<script type="module">
try {
  const q = new URLSearchParams(location.search);
  const wasm = await import('./pkg/tilo_browser_runtime.js');
  const {connectPeer, joinWorkspace} = await import('./peer.mjs');
  await wasm.default();
  globalThis.tiloRuntime = wasm;
  const origin = q.get('origin'), profile = q.get('profile'), label = q.get('label') || 'Stream viewer';
  let peer;
  // How far the page got, for a run that ends invalid while it connects.
  const stage = name => { globalThis.harnessStage = name; };
  if (q.get('invite')) {
    // Another person, with a root of their own and an invitation: the
    // room's owner admits them.
    stage('joining');
    const {createIdentity} = await import('./root-identity.mjs');
    const {identity} = await createIdentity({passkey: false});
    peer = await joinWorkspace({origin, invitation: q.get('invite'), identity, runtime: wasm, profile,
      displayName: label, label, pollMs: 250, onProgress: progress => stage('joining: ' + progress.phase)});
  } else {
    // A device of the host's person, in a room the host governs (as a
    // workspace in Tilo Cloud is): it creates the room when none is named,
    // and enters it as its owner's device before working in it.
    const person = {origin, userPublicKey: q.get('user'), label, displayName: label, runtime: wasm, profile};
    let room = q.get('room');
    stage('certifying');
    const door = await connectPeer({...person, live: false});
    try {
      stage('entering the room');
      room ||= (await door.workspaces.create('Stream qualification')).room;
      await door.workspaces.enter(room);
    } finally { await door.dispose(); }
    stage('connecting to the room');
    peer = await connectPeer({...person, room});
  }
  globalThis.peer = peer;
  stage('mounting the desktop');
  globalThis.tiloMount = await peer.mountDesktop(document.getElementById('surface'), {});
  globalThis.harnessReady = true;
} catch (error) { globalThis.harnessError = String(error?.stack ?? error); }
</script>`;

const TYPES = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.mjs': 'text/javascript',
  '.json': 'application/json', '.wasm': 'application/wasm', '.png': 'image/png', '.css': 'text/css',
  '.ttf': 'font/ttf', '.otf': 'font/otf', '.woff2': 'font/woff2', '.ftl': 'text/plain; charset=utf-8'};

/**
 * The host's WebSocket endpoint, `ws(s)://HOST/peer/v1/ws`, from an
 * `http(s)` origin: a page's connection needs it, and a browser whose root
 * certificate the run has not accepted yet answers this one GET with the
 * exception its certificate needs.
 */
export function hostSocketUrl(origin) {
  return new URL(`${new URL(origin).pathname.replace(/\/$/, '')}/peer/v1/ws`,
    new URL(origin).origin).href.replace(/^http/, 'ws');
}

/** How long Safari waits for a person to accept the host's certificate. */
export const CERTIFICATE_WAIT_MS = Number(process.env.TILO_SAFARI_CERTIFICATE_WAIT_MS ?? 300_000);

/**
 * Safari's exception for the dev host's self-signed certificate is a
 * person's, asked once per Safari profile, and WebDriver cannot press it: the
 * page visits the host alone first, which is that dialog and nothing else, and
 * the run waits for the person watching to accept it. `ask` answers what a
 * fetch of `url` said; a browser that trusts it answers 'trusted' on the
 * first visit.
 */
export async function acceptHostCertificate({ask, socket, waitMs = CERTIFICATE_WAIT_MS, sleep: pause = sleep,
  log = message => console.error(message)}) {
  const trusted = async () => (await ask().catch(() => null)) === 'trusted';
  if (await trusted()) return 'trusted';
  log(`Safari has not accepted the host's certificate yet: accept it in the window the driver opened, within `
    + `${Math.round(waitMs / 1000)} s (${socket})`);
  const deadline = Date.now() + waitMs;
  while (Date.now() < deadline) {
    await pause(2000);
    if (await trusted()) return 'trusted';
  }
  throw new Invalid(`Safari was not given the host's certificate within ${Math.round(waitMs / 1000)} s. A person accepts it `
    + `once per Safari profile, in the window the driver opened; the run then goes on`);
}

/** A page's fetch of `url`, as 'trusted' when it answered. */
export const askForUrl = (evaluate, url) => () => evaluate(`fetch(${JSON.stringify(url)}, {mode: 'no-cors'}).then(() => 'trusted', error => String(error))`);

/** The bundle and the harness page on 127.0.0.1. */
function servePages(bundle) {
  const server = createServer((request, response) => {
    const path = normalize(decodeURIComponent(new URL(request.url, 'http://x').pathname));
    if (path === HARNESS_PATH) {
      response.writeHead(200, {'content-type': TYPES['.html'], 'cache-control': 'no-store'}).end(harnessPage());
      return;
    }
    const file = join(bundle, path);
    if (!file.startsWith(bundle) || !existsSync(file) || statSync(file).isDirectory()) { response.writeHead(404).end(); return; }
    response.writeHead(200, {'content-type': TYPES[extname(file)] ?? 'application/octet-stream', 'cache-control': 'no-store'});
    createReadStream(file).pipe(response);
  });
  return new Promise(done => server.listen(0, '127.0.0.1', () => done({
    origin: `http://127.0.0.1:${server.address().port}`,
    close: () => new Promise(closed => { server.closeAllConnections?.(); server.close(closed); }),
  })));
}

// --- processes ----------------------------------------------------------------

/** A child whose stdout lines are kept and awaited, and whose stderr tail is kept for errors. */
function child(name, command, args, env = {}) {
  const process_ = spawn(command, args, {stdio: ['pipe', 'pipe', 'pipe'], env: {...process.env, ...env}});
  const lines = [], waiters = new Set();
  let stderr = '';
  let exit = null;
  createInterface({input: process_.stdout}).on('line', line => {
    lines.push(line);
    for (const waiter of waiters) waiter();
  });
  process_.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-8000); });
  process_.on('exit', (code, signal) => { exit = {code, signal}; for (const waiter of waiters) waiter(); });
  process_.on('error', error => { exit = {error: String(error)}; for (const waiter of waiters) waiter(); });
  return {
    name, process: process_, lines,
    stderr: () => stderr,
    exited: () => exit,
    /** The first stdout line `match` accepts, within `ms`. */
    line(match, ms, what) {
      return within(new Promise((done, fail) => {
        const check = () => {
          const found = lines.find(match);
          if (found !== undefined) { waiters.delete(check); done(found); return true; }
          if (exit) { waiters.delete(check); fail(new Invalid(`${name} exited before ${what}: ${JSON.stringify(exit)}\n${stderr.slice(-2000)}`)); return true; }
          return false;
        };
        if (!check()) waiters.add(check);
      }), ms, `${name} (${what})`);
    },
    stop() { if (!exit) process_.kill('SIGTERM'); },
  };
}

const event = name => line => { try { return JSON.parse(line).event === name; } catch { return false; } };

// --- the analysis -------------------------------------------------------------

/**
 * Everything the run observed, judged: `sent` the session's frame log,
 * `draws` the page's pictures, `start` / `stop` the session's events,
 * `presses` the page's click times and `arrivals` the presses the session
 * received (wall-clock microseconds throughout).
 */
export function analyse({sent, draws, start, stop, presses, arrivals, decoder, control, measureFromUs = -Infinity, desktop}) {
  const captured = new Map(sent.map(frame => [frame.index, frame]));
  const valid = draws.filter(draw => draw.index !== null);
  const corrupt = draws.length - valid.length;
  // A picture whose number is not the frame its timestamp names.
  const misnamed = valid.filter(draw => captured.get(draw.index)?.captureUs !== draw.tsUs).length;
  let backwards = 0, repeated = 0, last = -1;
  const firstDraw = new Map();
  for (const draw of valid) {
    if (draw.index < last) backwards++;
    else if (draw.index === last) repeated++;
    last = Math.max(last, draw.index);
    if (!firstDraw.has(draw.index)) firstDraw.set(draw.index, draw);
  }
  const firstVisible = valid.find(draw => draw.visible);
  const first = valid[0];
  // Timed: frames captured after the warm-up.
  const timed = index => captured.has(index) && captured.get(index).captureUs >= measureFromUs;
  const latency = [...firstDraw.values()].filter(draw => timed(draw.index))
    .map(draw => (draw.wallUs - captured.get(draw.index).captureUs) / 1000);
  const owed = first ? sent.filter(frame => frame.index >= first.index && timed(frame.index)).length : sent.length;
  const delivered = first ? [...firstDraw.keys()].filter(index => index >= first.index && timed(index)).length : 0;
  // Intervals between consecutive new pictures, the last ones included: a
  // page that falls behind shows the window's final frames late.
  const shown = [...firstDraw.values()].filter(draw => timed(draw.index)).map(draw => draw.wallUs);
  const intervals = shown.slice(1).map((wall, i) => (wall - shown[i]) / 1000);
  const span = shown.length > 1 ? (shown.at(-1) - shown[0]) / 1e6 : NaN;
  const sentTimed = sent.filter(frame => frame.captureUs >= measureFromUs);
  const sentSpan = sentTimed.length > 1 ? (sentTimed.at(-1).captureUs - sentTimed[0].captureUs) / 1e6 : NaN;
  const inputLatency = presses.map((at, i) => arrivals[i] === undefined ? NaN : (arrivals[i] - at) / 1000);
  const final = valid.at(-1)?.index ?? null;
  const correctness = {
    decoderErrors: decoder.errors.length,
    corruptPictures: corrupt,
    misnamedPictures: misnamed,
    backwardsPictures: backwards,
    repeatedPictures: repeated,
    finalPicture: final,
    lastSent: stop.lastIndex,
    converged: final === stop.lastIndex,
    controls: control,
    inputsSent: presses.length,
    inputsArrived: arrivals.length,
  };
  correctness.failures = [
    ...(correctness.decoderErrors ? [`${correctness.decoderErrors} decoder errors: ${decoder.errors.slice(0, 3).join(' | ')}`] : []),
    ...(corrupt ? [`${corrupt} corrupt pictures (number check failed)`] : []),
    ...(misnamed ? [`${misnamed} pictures are not the frame their timestamp names`] : []),
    ...(backwards ? [`${backwards} pictures went back to an older frame`] : []),
    ...(correctness.converged ? [] : [`the page ended on frame ${final}, not the last sent ${stop.lastIndex}`]),
    ...(control ? [] : ['the session\'s own user may not drive its window']),
    ...(presses.length && arrivals.length === presses.length ? [] : [`${arrivals.length} of ${presses.length} presses reached the session`]),
    ...desktopFailures(desktop),
  ];
  return {
    samples: {
      startup_ms: firstVisible ? [(firstVisible.wallUs - start.startUs) / 1000] : [],
      frame_latency_ms: latency,
      frame_interval_ms: intervals,
      encode_ms: sentTimed.map(frame => frame.encodeUs / 1000),
      input_ms: inputLatency.filter(Number.isFinite),
    },
    counts: {
      sent: sent.length, keyframes: sent.filter(frame => frame.key).length,
      encodedBytes: sent.reduce((sum, frame) => sum + frame.bytes, 0),
      decodeCalls: decoder.chunks, decoded: decoder.outputs, decoders: decoder.created, codecs: [...new Set(decoder.configs)],
      drawn: draws.length, distinctDrawn: firstDraw.size, owed, delivered,
      deliveredPercent: owed ? Math.round(delivered / owed * 10000) / 100 : NaN,
      sentFps: sentSpan ? Math.round((sentTimed.length - 1) / sentSpan * 10) / 10 : NaN,
      drawnFps: span ? Math.round((shown.length - 1) / span * 10) / 10 : NaN,
      firstPictureMs: first ? (first.wallUs - start.startUs) / 1000 : NaN,
    },
    correctness,
  };
}

/**
 * The desktop under the window, from its presented frame counts `before`
 * and `after` the timed stream of `seconds`: frames a second, or null when
 * the page did not say.
 */
export function desktopFramesPerSecond({before, after, seconds}) {
  return Number.isFinite(before) && Number.isFinite(after) && seconds > 0
    ? Math.round((after - before) / seconds * 10) / 10 : null;
}

/** Whether the desktop stayed at rest under the streaming `window`
 * (`desktop` as `desktopFramesPerSecond` takes it); a run that measured no
 * desktop is not judged here. */
export function desktopFailures(desktop, window = 'the window') {
  if (desktop === undefined) return [];
  const fps = desktopFramesPerSecond(desktop);
  if (fps === null) return [`the desktop under ${window} did not say how many frames it presented`];
  return fps > MAX_DESKTOP_FPS
    ? [`the desktop under ${window} presented ${fps} frames/s while it streamed (at most ${MAX_DESKTOP_FPS})`] : [];
}

/** Each goal with its measured value and verdict. */
export function judgeGoals(summary, counts) {
  const atMost = (name, measured, target) => ({name, measured, target, met: measured <= target});
  return [
    atMost('startup to the first picture (ms)', summary.startup_ms.max, GOALS.startupMs),
    atMost('frame latency, capture to picture (p95, ms)', summary.frame_latency_ms.p95, GOALS.latencyP95Ms),
    {name: 'frames delivered (%)', measured: counts.deliveredPercent, target: GOALS.deliveredPercent,
      met: counts.deliveredPercent >= GOALS.deliveredPercent},
    atMost('input, click to session (p95, ms)', summary.input_ms.p95, GOALS.inputP95Ms),
  ].filter(goal => Number.isFinite(goal.measured));
}

export function formatSummary(report) {
  const s = report.summary, c = report.counts;
  const row = (name, stat) => `  ${name.padEnd(22)} n ${String(stat.n).padStart(5)}  p50 ${stat.p50}  p95 ${stat.p95}  p99 ${stat.p99}  max ${stat.max}`;
  const lines = [
    `native stream qualification: bundle ${report.bundle.revision.slice(0, 12)} (${report.bundle.profile}), `
      + `encoder ${report.session.encoder}, ${report.session.width}x${report.session.height} at ${report.session.fps} fps, carrier ${report.workload.carrier} (took ${report.live?.carrier ?? "unknown"})`,
    `  ${report.browser.product}${report.browser.driver === 'webdriver' ? ' (WebDriver)' : ''} | adapter ${report.renderer} | decoders ${c.codecs.join(', ') || 'none'}`,
    `  sent ${c.sent} frames (${c.sentFps} fps, ${c.keyframes} keyframes), drawn ${c.distinctDrawn} (${c.drawnFps} fps), `
      + `delivered ${c.delivered}/${c.owed} (${c.deliveredPercent} %), desktop under it presented ${c.desktopFramesPerSecond} frames/s (${c.desktopStillWindowFramesPerSecond} once the window was still`
      + `${c.earlierDesktopFramesPerSecond === undefined ? '' : `, ${c.earlierDesktopFramesPerSecond} under the window before it`})`,
    row('startup (ms)', s.startup_ms), row('frame latency (ms)', s.frame_latency_ms),
    row('frame interval (ms)', s.frame_interval_ms), row('encode (ms)', s.encode_ms), row('input (ms)', s.input_ms),
  ];
  const k = report.correctness;
  lines.push(k.failures.length ? `  INCORRECT: ${k.failures.join('; ')}`
    : `  correct: no decoder errors, no corrupt, misnamed or older pictures, ended on frame ${k.lastSent}, `
      + `${k.inputsArrived}/${k.inputsSent} presses arrived in order`);
  for (const [name, found] of Object.entries(report.acceptance?.cases ?? {})) {
    lines.push(`  ${found.skipped ? `SKIP acceptance ${name}: ${found.skipped}`
      : `${found.failures.length ? 'FAIL' : 'PASS'} acceptance ${name}${found.failures.length ? `: ${found.failures.join('; ')}` : ''}`}`);
  }
  for (const goal of report.goals) {
    lines.push(`  ${goal.met ? 'MET ' : 'MISS'} ${goal.name}: ${goal.measured} vs ${goal.target}`);
  }
  return lines.join('\n');
}

// --- the run --------------------------------------------------------------------

/**
 * The harness's input sources, in one vocabulary over the two drivers: keys
 * by name (native_stream_acceptance.mjs `KEYS`), the pointer from a point,
 * and text. Chrome's are Input.* protocol calls; Safari's are WebDriver
 * actions, with no input method and no clipboard (its driver has neither).
 */
const KEYS = Object.freeze({
  a: {key: 'a', code: 'KeyA', keyCode: 65}, b: {key: 'b', code: 'KeyB', keyCode: 66},
  c: {key: 'c', code: 'KeyC', keyCode: 67}, d: {key: 'd', code: 'KeyD', keyCode: 68},
  e: {key: 'e', code: 'KeyE', keyCode: 69}, f: {key: 'f', code: 'KeyF', keyCode: 70},
  z: {key: 'z', code: 'KeyZ', keyCode: 90}, v: {key: 'v', code: 'KeyV', keyCode: 86},
  Control: {key: 'Control', code: 'ControlLeft', keyCode: 17},
});
const LIFTED = {name: '', code: '', keyCode: 0, text: ''};
const keyFields = name => ({...LIFTED, ...KEYS[name]});
// Input.dispatchMouseEvent's button name for a WebDriver button number.
const POINTER_BUTTONS = {0: 'left', 1: 'middle', 2: 'right'};

/** The protocol name of a platform, for the report. */
const DRIVER = {chrome: 'cdp', safari: 'webdriver'};

/** Wrap a Chrome window (tools/web/chrome.mjs) as the pages plus input a run drives. */
function chromiumBrowser(chrome) {
  return {
    driver: DRIVER.chrome,
    product: `Chrome`,
    version: async () => (await chrome.send('Browser.getVersion')).result,
    browser: chrome,
    // When this page was attached, for the two-page timed windows.
    now: () => Number(process.hrtime.bigint() / 1000n) / 1000,
    /** A browser context of its own: another person's browser, or the second device of the same one. */
    context: () => chrome.context(),
    close: () => chrome.close(),
    diagnostics: () => chrome.diagnostics(),
    evalTimeoutMs: 180_000,
    attach: page => chromiumAttach(chrome, page),
  };
}

async function chromiumAttach(chrome, page) {
  const {result: {targetId}} = await chrome.send('Target.createTarget', {url: 'about:blank',
    ...(page.context ? {browserContextId: page.context} : {}), ...(page.window ? {newWindow: true} : {})});
  const {result: {sessionId}} = await chrome.send('Target.attachToTarget', {targetId, flatten: true});
  const errors = [];
  // The page's own errors and warnings, for an invalid run's diagnosis.
  const console_ = [];
  chrome.on(message => {
    if (message.sessionId !== sessionId) return;
    if (message.method === 'Runtime.exceptionThrown') {
      const details = message.params.exceptionDetails;
      errors.push((details.exception?.description ?? details.text).slice(0, 400));
    } else if (message.method === 'Runtime.consoleAPICalled' && ['error', 'warning'].includes(message.params.type)
      && console_.length < 200) {
      console_.push(message.params.args.map(arg => arg.value ?? arg.description ?? '').join(' ').slice(0, 300));
    }
  });
  const send = (method, params = {}) => chrome.send(method, params, sessionId);
  await send('Runtime.enable');
  await send('Page.enable');
  await send('Emulation.setDeviceMetricsOverride', {...page.viewport, mobile: false});
  await send('Emulation.setFocusEmulationEnabled', {enabled: true});
  if (page.block) await send('Page.addScriptToEvaluateOnNewDocument', {source: 'globalThis.__tiloBlockWebTransport = true;'});
  await send('Page.addScriptToEvaluateOnNewDocument', {source: PROBE});
  return {
    targetId, errors, console: console_,
    // Generous: on SwiftShader one GPU process rasterizes every page's desktop,
    // and a page waiting on it does not answer for a minute or more.
    evaluate: async (expression, ms = 180_000) => {
      const reply = await within(send('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true}), ms,
        `the ${page.name ?? 'first'} page (${expression.replace(/\s+/g, ' ').slice(0, 120)})`);
      if (reply.error) throw Error(reply.error.message);
      if (reply.result.exceptionDetails) {
        const details = reply.result.exceptionDetails;
        throw Error(details.exception?.description ?? details.text);
      }
      return reply.result.result.value;
    },
    navigate: url => send('Page.navigate', {url}),
    key: async name => {
      const fields = keyFields(name);
      await send('Input.dispatchKeyEvent', {type: 'keyDown', key: fields.key, code: fields.code,
        windowsVirtualKeyCode: fields.keyCode, text: fields.text});
      await send('Input.dispatchKeyEvent', {type: 'keyUp', key: fields.key, code: fields.code,
        windowsVirtualKeyCode: fields.keyCode});
    },
    keyDown: async (name, {modifiers = 0} = {}) => {
      const fields = keyFields(name);
      await send('Input.dispatchKeyEvent', {type: 'rawKeyDown', key: fields.key, code: fields.code,
        windowsVirtualKeyCode: fields.keyCode, modifiers});
    },
    keyUp: async (name, {modifiers = 0} = {}) => {
      const fields = keyFields(name);
      await send('Input.dispatchKeyEvent', {type: 'keyUp', key: fields.key, code: fields.code,
        windowsVirtualKeyCode: fields.keyCode, modifiers});
    },
    move: async ({x, y}) => { await send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y}); },
    press: async ({x, y}, {button = 0} = {}) => {
      const name = POINTER_BUTTONS[button];
      await send('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: name, clickCount: 1});
      await send('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: name, clickCount: 1});
    },
    wheel: async ({x, y}, {dy = 0, dx = 0} = {}) => {
      await send('Input.dispatchMouseEvent', {type: 'mouseWheel', x, y, deltaX: dx, deltaY: dy});
    },
    // Input methods and the clipboard are Chrome's protocol's, not Safari's.
    imeCompose: text => send('Input.imeSetComposition', {text, selectionStart: text.length, selectionEnd: text.length}),
    insertText: text => send('Input.insertText', {text}),
    viewport: ({width, height, deviceScaleFactor}) => send('Emulation.setDeviceMetricsOverride',
      {width, height, deviceScaleFactor, mobile: false}),
  };
}

/** Wrap a Safari window (tools/web/webdriver.mjs) as the same pages and input. */
function safariBrowser(safari) {
  return {
    driver: DRIVER.safari,
    product: safari.product,
    version: async () => safari.version,
    browser: safari,
    // The timed windows are WebDriver round trips from this clock.
    now: () => performance.now(),
    // One Safari window: the cases that need a second device or a second person
    // run in Chrome, on the CI runner.
    context: async () => { throw new Invalid('one drove one Safari window: a second browser context is Chrome\'s'); },
    close: () => safari.close(),
    diagnostics: () => '',
    evalTimeoutMs: 180_000,
    attach: page => safariAttach(safari, page),
  };
}

async function safariAttach(safari, page) {
  const errors = [], console_ = [];
  await safari.navigate('about:blank');
  // A page step reports its own errors to its result; `__tiloStream` holds
  // what the run reads, as in Chrome.
  await safari.evaluate(PROBE);
  await safari.setWindowRect(page.viewport ?? {width: 1280, height: 800});
  return {
    targetId: safari.session, errors, console: console_,
    evaluate: (expression, ms = 180_000) => within(safari.evaluate(expression), ms,
      `the ${page.name ?? 'first'} page (${expression.replace(/\s+/g, ' ').slice(0, 120)})`),
    navigate: url => safari.navigate(url),
    key: name => safari.actions([{key: {value: keyFields(name).key}}]),
    keyDown: (name, {modifiers = 0} = {}) => safari.actions([{key: {value: keyFields(name).key, up: false}}]).then(
      () => modifiers && safari.actions([{key: {value: modifiers & 2 ? '\uE009' : '\uE008', up: false}}])),
    keyUp: (name, {modifiers = 0} = {}) => safari.actions([
      ...(modifiers & 2 ? [{key: {value: '\uE009', down: false}}] : []), {key: {value: keyFields(name).key, down: false}}]),
    move: ({x, y}) => safari.actions([{pointer: {type: 'pointerMove', x, y, origin: 'viewport', duration: 0}}]),
    press: ({x, y}, {button = 0} = {}) => safari.click(x, y, {button}),
    wheel: async ({x, y}, {dy = 0, dx = 0} = {}) => {
      await safari.actions([{pointer: {type: 'pointerMove', x, y, origin: 'viewport', duration: 0}}]);
      await safari.evaluate(`(() => { const at = document.elementFromPoint(${x}, ${y});
        (at ?? document.body).dispatchEvent(new WheelEvent('wheel', {deltaX: ${dx}, deltaY: ${dy}, bubbles: true, cancelable: true})); })()`);
    },
    imeCompose: () => { throw new Invalid('WebDriver drives no input method: Safari runs this case in Chrome'); },
    insertText: () => { throw new Invalid('WebDriver drives no input method: Safari runs this case in Chrome'); },
    viewport: ({width, height}) => safari.setWindowRect({width, height}),
  };
}

/**
 * A page in `context` (a browser context of its own: another person's
 * browser) or the browser's default one, in a window of its own with
 * `window` (so every page stays visible and keeps presenting), with
 * WebTransport made to fail with `block`. Each driver's own `attach` is that
 * page: Chrome's a tab of the protocol's, Safari's its one window's.
 */
async function attachPage(browser, viewport, options = {}) {
  return browser.attach({viewport, ...options});
}

/**
 * Open the harness page `name` with `params` on a new page, and wait until
 * its desktop presents, with its live plane up and the legacy-window layer
 * on it.
 */
async function openPage(browser, {name, url, params, viewport, page: options, relay}) {
  const page = await attachPage(browser, viewport, options);
  page.name = name;
  // The desktop's first frame: SwiftShader compiles its pipelines first, on a
  // GPU process other pages may be keeping busy (`mountMs`). The desktop gives
  // that a minute and then fails its mount; the pipelines it compiled stay in
  // the GPU process's cache, so the page is loaded again, up to twice.
  const mountMs = options.mountMs ?? 240_000, opened = browser.now();
  for (let attempt = 1; ; attempt++) {
    await page.navigate(`${url}?${new URLSearchParams(params)}`);
    await until(page, 'globalThis.harnessReady || globalThis.harnessError', mountMs, `the ${name} page's desktop did not mount`)
      .catch(async error => {
        throw new Invalid(`${error.message} (stuck ${await page.evaluate('globalThis.harnessStage ?? null', 10_000).catch(() => 'unanswered')}; `
          + `page console: ${page.console.slice(-10).join(' | ')}; page errors: ${page.errors.slice(0, 3).join(' | ')})`);
      });
    const error = await page.evaluate('globalThis.harnessError ?? null');
    if (!error) break;
    if (attempt < 3 && /WebGPU presented no first frame/.test(error)) {
      console.error(`the ${name} page's desktop missed its first frame on the busy GPU process; loading it again`);
      continue;
    }
    throw new Invalid(`the ${name} page did not connect: ${error}`);
  }
  await until(page, "globalThis.tiloMount?.canvas?.parentElement?.dataset.phase === 'presented'", mountMs,
    `the ${name} page's desktop presented no frame`);
  await until(page, "Boolean(document.querySelector('.tilo-legacy-windows'))", 60_000,
    `the ${name} page's desktop opened no legacy-window layer (no live plane)`);
  await until(page, 'globalThis.peer?.live?.available === true', 120_000,
    `the ${name} page's live plane did not come up`).catch(async error => {
    throw new Invalid(`${error.message}: ${await page.evaluate(LIVE).catch(() => 'unavailable')}; page console: ${page.console.slice(-15).join(' | ')}; host log: ${relay.lines.slice(-30).join('\n').slice(-4000)}`);
  });
  console.error(`the ${name} page presented its desktop, live, in ${Math.round((browser.now() - opened) / 1000)} s`);
  return page;
}

/** The frames a session's `--out` log says it published. */
const readSent = log => readFileSync(log, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line));

/** Wait until `probe()` (in the page) is truthy, or fail with `what`. */
async function until(page, expression, ms, what) {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await page.evaluate(expression).catch(() => null);
    if (value) return value;
    if (Date.now() > deadline) throw new Invalid(`${what} within ${Math.round(ms / 1000)} s`);
    await sleep(100);
  }
}

// The page's room phase, live plane (state, members, the carrier it took) and its live members.
const LIVE = `JSON.stringify({phase: globalThis.peer?.getSnapshot?.()?.phase ?? null,
  live: globalThis.peer?.live?.stats?.() ?? null, members: globalThis.peer?.live?.members?.() ?? null,
  room: JSON.stringify({...(globalThis.peer?.getSnapshot?.() ?? {}), records: undefined}).slice(0, 1500)})`;

const WINDOW = `(() => { const c = document.querySelector('canvas.tilo-legacy-window');
  if (!c || c.style.display !== 'block') return null; const r = c.getBoundingClientRect();
  return {x: r.left + r.width / 2, y: r.top + r.height / 2, width: r.width, height: r.height, control: c.style.pointerEvents === 'auto'}; })()`;

async function clickWindow(page, inputs) {
  const presses = [];
  const at = await page.evaluate(WINDOW);
  if (!at) return {presses, control: false};
  await page.move({x: at.x, y: at.y});
  await sleep(300);
  for (let i = 0; i < inputs; i++) {
    presses.push(Math.round((performance.timeOrigin + performance.now()) * 1000));
    await page.press({x: at.x, y: at.y});
    await sleep(200);
  }
  return {presses, control: at.control};
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const before = hostState();
  if (options.maxLoad !== undefined && before.load1 / before.cpus > options.maxLoad) {
    throw new Invalid(`host contended before the run: load ${before.load1} on ${before.cpus} CPUs`);
  }
  const bundle = verifyBundle(options.bundle, options);
  const scratch = mkdtempSync(join(os.tmpdir(), 'tilo-stream-'));
  const pages = await servePages(options.bundle);
  const children = [];
  let browser;
  try {
    const relay = child('host', options.relay, ['dev', '--state', join(scratch, 'host'), '--web', '127.0.0.1:0',
      '--web-plain', '127.0.0.1:0', '--allowed-origin', pages.origin],
    // The legacy-window bridge says at debug level why it refuses a stream.
    {TILO_RELAY_LOG: process.env.TILO_RELAY_LOG ?? 'warn,tilo_relay_shell::legacy=debug'});
    children.push(relay);
    const host = JSON.parse(await relay.line(line => line.startsWith('{'), 120_000, 'its announcement'));
    const origin = options.carrier === 'https' ? host.plainOrigin : host.origin;
    if (!origin) throw new Invalid(`the host announced no ${options.carrier} origin`);
    // The dev host's certificate is self-signed: WebTransport pins it by
    // hash, Chrome's HTTPS routes take the exception below, and Safari takes
    // it by the page's first visit to the host (which it asks a person about).
    const flags = [...chromeFlags(options.renderer, process.getuid?.()), '--ignore-certificate-errors'];
    browser = options.platform === 'safari' ? safariBrowser(await launchSafari()) : chromiumBrowser(await launchChrome({args: flags}));
    const version = await browser.version();
    // Safari's exception for the dev host's self-signed certificate is the
    // person watching's: the page visits the host alone first, which is that
    // dialog and nothing else.
    if (options.platform === 'safari') {
      await acceptHostCertificate({ask: askForUrl(expression => browser.browser.evaluate(expression), hostSocketUrl(host.plainOrigin)),
        socket: hostSocketUrl(host.plainOrigin)});
    }
    const url = `${pages.origin}${HARNESS_PATH}`;
    const open = (name, params, {viewport = options.viewport, ...page} = {}) =>
      openPage(browser, {name, url, params: {origin, user: host.userPublicKey, profile: `stream-${randomUUID()}`, ...params}, viewport, page, relay});
    // The stream starts on a desktop already shown, with its live layer and
    // the legacy-window layer on it, so startup is the stream's own. The page
    // creates the room, which the host governs.
    const page = await open('first', {});
    const room = await page.evaluate('globalThis.peer.room');
    const decodable = await page.evaluate(`(async () => (await VideoDecoder.isConfigSupported({codec: ${JSON.stringify(VIDEO_DECODER_CONFIGS[options.codec])}, optimizeForLatency: true})).supported)()`);
    if (!decodable) throw new Invalid(`${version.product} decodes no ${options.codec} with WebCodecs`);
    const adapter = await page.evaluate(`(async () => { const a = await navigator.gpu?.requestAdapter();
      if (!a) return null; const i = a.info ?? {}; return {vendor: i.vendor, architecture: i.architecture, device: i.device, description: i.description}; })()`);
    if (classifyAdapter(adapter) !== options.renderer) {
      throw new Invalid(`WebGPU adapter is ${classifyAdapter(adapter)}, not ${options.renderer}: ${JSON.stringify(adapter)}`);
    }

    // The desktop under the window should not repaint for it: the window's
    // pictures go to its own canvas. Frames it presents meanwhile are work
    // the page does beside decoding.
    const desktopFrames = () => page.evaluate('globalThis.tiloMount?.state?.()?.frames ?? null', 300_000);
    const startSession = async (name, {surface, seconds, width = options.window.width, height = options.window.height, fps = options.fps, extra = []}) => {
      const log = join(scratch, `${name}.jsonl`);
      const process_ = child(name, options.source, ['--dev-state', join(scratch, 'host'), '--host', host.irohAddr,
        '--room', room, '--out', log, '--width', String(width), '--height', String(height),
        '--seconds', String(seconds), '--surface', String(surface), '--codec', options.codec,
        ...(fps ? ['--fps', String(fps)] : []), ...extra],
      {RUST_LOG: process.env.STREAM_SOURCE_LOG ?? 'warn'});
      children.push(process_);
      const ready = JSON.parse(await process_.line(event('ready'), 120_000, 'its subscription'));
      // Every acceptance case is in a room the host governs, as a workspace
      // a person shares is: its roles decide who drives a window.
      if (!ready.enteredRoom) throw new Invalid(`the ${name} could not enter the room as its owner's device: the host does not govern it`);
      const exited = () => within(new Promise(done => { if (process_.exited()) done(); else process_.process.on('exit', done); }),
        15_000, `the ${name}'s exit`);
      return {source: process_, session: ready, log, exited};
    };
    // The window before the measured one (--reopen): its desktop frames
    // a second once it streams, judged as the measured window's are.
    let earlierDesktop;
    if (options.reopen) {
      const earlier = await startSession('earlier session', {surface: EARLIER_SURFACE, seconds: EARLIER_SECONDS});
      await sleep(1000);
      earlier.source.process.stdin.write('start\n');
      await earlier.source.line(event('started'), 10_000, 'the earlier stream start');
      // Its opening is the desktop's to draw; from here it only streams.
      await sleep(1000);
      const before = await desktopFrames(), from = performance.now();
      await earlier.source.line(event('stopped'), (EARLIER_SECONDS + 120) * 1000, 'the end of the earlier stream');
      earlierDesktop = {before, after: await desktopFrames(), seconds: (performance.now() - from) / 1000};
      // It closes, and the desktop comes to rest before the measured one opens.
      earlier.source.process.stdin.end();
      await earlier.exited();
      await until(page, `!document.querySelector(${JSON.stringify(`canvas.tilo-legacy-window[data-key^="${earlier.session.device}:"]`)})`,
        30_000, 'the earlier window did not close');
      await sleep(3000);
    }
    const {source, session, log: framesLog, exited} = await startSession('session', {surface: MEASURED_SURFACE, seconds: options.warmUp + options.seconds});
    const mine = `${session.device}:`;
    const decoderBefore = await page.evaluate('JSON.parse(JSON.stringify(globalThis.__tiloStream.decoder))', 300_000);
    console.error(`session ready: ${session.encoder} at ${session.fps} fps`);
    await sleep(1000);
    source.process.stdin.write('start\n');
    const start = JSON.parse(await source.line(event('started'), 10_000, 'the stream start'));
    // Frames captured from here on are timed (the warm-up's are only judged).
    const measureFromUs = start.startUs + options.warmUp * 1e6;
    await sleep(Math.max(0, measureFromUs / 1000 - (performance.timeOrigin + performance.now())));
    // What asked for each frame the desktop presents meanwhile (the
    // runtime's frame metrics), so a desktop that does not rest says why.
    await page.evaluate('globalThis.tiloRuntime.configure_frame_metrics(true)');
    const framesBefore = await desktopFrames();
    const stop = JSON.parse(await source.line(event('stopped'), (options.warmUp + options.seconds + 120) * 1000, 'the end of the stream'));
    const framesAfter = await desktopFrames();
    const desktopRequesters = await page.evaluate(`(() => { const metrics = JSON.parse(globalThis.tiloRuntime.take_frame_metrics());
      globalThis.tiloRuntime.configure_frame_metrics(false); return metrics.map(mount => mount.requesters); })()`);
    console.error(`stream captured ${stop.frames} frames in ${stop.seconds.toFixed(1)} s, last published frame ${stop.lastIndex}`);
    // A still window: the page must come to rest on the last frame.
    const settle = Date.now() + 15_000;
    while (Date.now() < settle) {
      const final = await page.evaluate(`globalThis.__tiloStream.draws.filter(d => d.index !== null && d.key?.startsWith(${JSON.stringify(mine)})).at(-1)?.index ?? null`);
      if (final === stop.lastIndex) break;
      await sleep(200);
    }
    // The window is still now: the desktop under it should be at rest too.
    const stillFrom = await desktopFrames(), stillStart = performance.now();
    await sleep(2000);
    const stillWindow = Math.round(((await desktopFrames()) - stillFrom) / ((performance.now() - stillStart) / 1000) * 10) / 10;
    const {presses, control} = await clickWindow(page, options.inputs);
    await sleep(2000);
    // The timed run ends here; the acceptance cases add input of their own.
    const timedLines = source.lines.length;
    const probe = await page.evaluate('globalThis.__tiloStream', 120_000);
    const live = JSON.parse(await page.evaluate(LIVE));
    // This session's pictures, and what the decoders did since it started
    // (a decoder error at any time counts).
    const draws = probe.draws.filter(draw => draw.key?.startsWith(mine));
    const decoder = {...probe.decoder, created: probe.decoder.created - decoderBefore.created,
      chunks: probe.decoder.chunks - decoderBefore.chunks, keys: probe.decoder.keys - decoderBefore.keys,
      outputs: probe.decoder.outputs - decoderBefore.outputs, configs: probe.decoder.configs.slice(decoderBefore.configs.length)};
    const sent = readSent(framesLog);
    const arrivals = source.lines.slice(0, timedLines).map(line => { try { return JSON.parse(line); } catch { return null; } })
      .filter(line => line?.event === 'input' && /^PointerButton \{ button: 272, pressed: true \}$/.test(line.input))
      .map(line => line.arrivalUs);
    if (!draws.length) {
      const windows = await page.evaluate(`[...document.querySelectorAll('canvas.tilo-legacy-window')]
        .map(c => ({key: c.dataset.key, app: c.dataset.app, display: c.style.display, events: c.style.pointerEvents}))`);
      throw new Invalid(`the page drew no picture of the window (windows: ${JSON.stringify(windows)}; `
        + `decoder: ${JSON.stringify(decoder)}; page errors: ${page.errors.slice(0, 2).join(' | ')}; live: ${JSON.stringify(live)}; `
        + `session: ${JSON.stringify(session)}; published ${sent.length} frames (${sent.filter(frame => frame.key).length} keyframes); `
        + `page console: ${page.console.slice(-15).join(' | ')}; host log: ${relay.lines.slice(-30).join('\n').slice(-4000)}; session log: ${source.stderr().slice(-1500)})`);
    }
    if (page.errors.length) throw new Invalid(`page exceptions: ${page.errors.slice(0, 3).join(' | ')}`);
    const after = hostState();
    if (options.maxLoad !== undefined && after.load1 / after.cpus > options.maxLoad) {
      throw new Invalid(`host contended after the run: load ${after.load1} on ${after.cpus} CPUs`);
    }
    const desktop = {before: framesBefore, after: framesAfter, seconds: (stop.stopUs - measureFromUs) / 1e6};
    const analysis = analyse({sent, draws, start, stop, presses, arrivals, decoder, control, measureFromUs, desktop});
    analysis.counts.desktopStillWindowFramesPerSecond = stillWindow;
    analysis.counts.desktopFramesPerSecond = desktopFramesPerSecond(desktop);
    analysis.counts.desktopFrameRequesters = desktopRequesters;
    if (earlierDesktop) {
      analysis.counts.earlierDesktopFramesPerSecond = desktopFramesPerSecond(earlierDesktop);
      analysis.correctness.failures.push(...desktopFailures(earlierDesktop, 'the window before it'));
    }
    const summary = Object.fromEntries(Object.entries(analysis.samples).map(([name, values]) => [name, stats(values)]));

    // The acceptance cases (native_stream_acceptance.mjs), on the same
    // host, room and desktop, after the timed run so they cost it nothing.
    console.error('timed run done; acceptance cases');
    const acceptance = await runAcceptance({
      browser, main: page, w1: {source, session, stop, log: framesLog}, pagesOrigin: pages.origin, viewport: options.viewport, room, live: LIVE,
      openPage: (name, params, {block = false, context = false} = {}) => (async () => open(name, params,
        {viewport: {width: 640, height: 400, deviceScaleFactor: 1}, window: true, block, context: context ? await browser.context() : undefined,
          mountMs: 600_000}))(),
      startSession, readSent,
    });
    for (const extra of acceptance.children) { extra.source.process.stdin.end(); await extra.exited().catch(() => {}); }
    source.process.stdin.end();
    await exited();
    const correctness = {...analysis.correctness, failures: [...analysis.correctness.failures,
      ...Object.entries(acceptance.cases).flatMap(([name, found]) => found.failures.map(failure => `${name}: ${failure}`))]};
    const report = {
      schema: 1, measuredAt: new Date().toISOString(), bundle, renderer: options.renderer, adapter,
      browser: {product: version.product, userAgent: version.userAgent, driver: browser.driver}, viewport: options.viewport,
      host: {...before, before, after, maxLoad: options.maxLoad ?? null, platform: `${os.platform()} ${os.release()} ${os.arch()}`},
      session: {encoder: session.encoder, hardware: session.hardware, fps: session.fps, width: session.width, height: session.height,
        codec: session.codec ?? options.codec},
      live: {carrier: live.live?.carrier ?? null, state: live.live?.state ?? null},
      workload: {seconds: options.seconds, inputs: options.inputs, carrier: options.carrier, codec: options.codec, warmUpSeconds: options.warmUp,
        reopen: options.reopen, platform: options.platform, chromeFlags: flags},
      summary, counts: analysis.counts, correctness, goals: judgeGoals(summary, analysis.counts),
      acceptance: {cases: acceptance.cases, pageErrors: acceptance.pageErrors},
      samples: analysis.samples,
    };
    writeFileSync(options.out, JSON.stringify(report, null, 1) + '\n');
    console.log(formatSummary(report));
    if (report.correctness.failures.length || (options.enforceGoals && report.goals.some(goal => !goal.met))) process.exitCode = 1;
  } finally {
    for (const process_ of children) process_.stop();
    await browser?.close().catch(error => console.error(String(error)));
    await pages.close();
    rmSync(scratch, {recursive: true, force: true});
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch(error => {
    console.error(error instanceof Invalid ? `invalid run: ${error.message}${process.env.STREAM_QUALIFICATION_DEBUG ? `\n${error.stack}` : ''}` : error.stack ?? String(error));
    process.exit(2);
  });
}
