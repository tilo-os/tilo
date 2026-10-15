#!/usr/bin/env node
// A native window streamed to the browser desktop, qualified end to end: a
// session's legacy window, encoded by the production encoder (H.264, or --codec), through
// a real host's legacy-window bridge and live plane, onto a desktop bundle in
// headless Chrome that decodes it with WebCodecs (legacy-windows.mjs) and
// draws it over the window's tile. It measures what a person sees and checks
// that what they see is right. It writes one report; `tools/perf/adapters.py
// native-stream` turns it into the `stream` KPI group's records
// (Perf / scheduled, tools/perf/kpi-budgets.toml).
//
// usage: node scripts/perf/native_stream_qualification.mjs BUNDLE_DIR --out REPORT_JSON
//          --relay TILO_RELAY --source LEGACY_STREAM_SOURCE --renderer gpu|swiftshader
//          [--seconds S] [--window WxH] [--fps N] [--inputs N] [--carrier webtransport|https] [--warm-up S]
//          [--codec h264|vp9|av1]
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
//            production peer.mjs and host.mjs
//
// The run, in order:
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
//
// Correctness, judged on every run (a miss exits 1, with the report): no
// decoder error; every drawn picture's number reads back with a valid check
// (no corrupt picture) and is the frame its timestamp names; pictures never go
// back to an older frame; the page ends on the last frame sent; this member
// drives the window (it is the session's own user) and every press arrives,
// in order.
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
import {launch, sleep} from '../../tools/web/chrome.mjs';
import {VIDEO_DECODER_CONFIGS} from '../../crates/browser/tilo-browser-runtime/web/legacy-windows.mjs';
import {Invalid, RENDERERS, chromeFlags, classifyAdapter, hostState, parseViewport, stats, verifyBundle, within}
  from './browser_qualification.mjs';

export const GOALS = {startupMs: 1000, latencyP95Ms: 100, deliveredPercent: 99, inputP95Ms: 50};
export const CARRIERS = ['webtransport', 'https'];
/** The stream's codec (`--codec`): VP9 and AV1 are what a session without an
 * H.264 encoder streams once every member's page decodes them. */
export const CODECS = Object.keys(VIDEO_DECODER_CONFIGS);
// The session's number blocks (legacy_stream_source.rs `CODE_BITS`, `code_geometry`).
export const CODE_BITS = 20;
const VIEWPORT = {width: 1280, height: 800, deviceScaleFactor: 1};
const HARNESS_PATH = '/native-stream-qualification.html';
const USAGE = 'usage: native_stream_qualification.mjs BUNDLE_DIR --out REPORT_JSON --relay BIN --source BIN '
  + '--renderer gpu|swiftshader [--seconds S] [--window WxH] [--fps N] [--inputs N] [--carrier webtransport|https] [--warm-up S] '
  + '[--codec h264|vp9|av1] [--viewport WxH] [--expect-revision SHA] [--allow-dev] [--max-load L] [--enforce-goals]';

export function parseArgs(argv) {
  const options = {seconds: 10, inputs: 20, carrier: 'webtransport', codec: 'h264', allowDev: false, enforceGoals: false,
    window: {width: 1280, height: 720}};
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
    else if (arg === '--enforce-goals') options.enforceGoals = true;
    else if (arg.startsWith('--')) throw Error(`unknown option ${arg}\n${USAGE}`);
    else positionals.push(arg);
  }
  if (positionals.length !== 1 || !options.out || !options.relay || !options.source || !options.renderer) throw Error(USAGE);
  if (!RENDERERS.includes(options.renderer)) throw Error(`--renderer must be one of ${RENDERERS.join(', ')}`);
  if (!CARRIERS.includes(options.carrier)) throw Error(`--carrier must be one of ${CARRIERS.join(', ')}`);
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
const PROBE = `(() => {
  const readCode = ${readCode.toString()};
  const probe = globalThis.__tiloStream = {draws: [], decoder: {created: 0, configs: [], chunks: 0, keys: 0, outputs: 0, errors: []}};
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
      probe.draws.push({wallUs: Math.round((performance.timeOrigin + at) * 1000), tsUs: image.timestamp, index,
        visible: canvas.style.display === 'block', width, height: canvas.height, key: canvas.dataset.key});
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
  const {connectPeer} = await import('./peer.mjs');
  await wasm.default();
  const profile = q.get('profile');
  const peer = globalThis.peer = await connectPeer({
    origin: q.get('origin'), userPublicKey: q.get('user'), label: 'Stream viewer', room: q.get('room'),
    profile, displayName: 'Stream viewer', runtime: wasm,
  });
  globalThis.tiloMount = await peer.mountDesktop(document.getElementById('surface'), {});
  globalThis.harnessReady = true;
} catch (error) { globalThis.harnessError = String(error?.stack ?? error); }
</script>`;

const TYPES = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.mjs': 'text/javascript',
  '.json': 'application/json', '.wasm': 'application/wasm', '.png': 'image/png', '.css': 'text/css',
  '.ttf': 'font/ttf', '.otf': 'font/otf', '.woff2': 'font/woff2', '.ftl': 'text/plain; charset=utf-8'};

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
export function analyse({sent, draws, start, stop, presses, arrivals, decoder, control, measureFromUs = -Infinity}) {
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
    `  ${report.browser.product} | adapter ${report.renderer} | decoders ${c.codecs.join(', ') || 'none'}`,
    `  sent ${c.sent} frames (${c.sentFps} fps, ${c.keyframes} keyframes), drawn ${c.distinctDrawn} (${c.drawnFps} fps), `
      + `delivered ${c.delivered}/${c.owed} (${c.deliveredPercent} %), desktop under it presented ${c.desktopFramesPerSecond} frames/s (${c.desktopStillWindowFramesPerSecond} once the window was still)`,
    row('startup (ms)', s.startup_ms), row('frame latency (ms)', s.frame_latency_ms),
    row('frame interval (ms)', s.frame_interval_ms), row('encode (ms)', s.encode_ms), row('input (ms)', s.input_ms),
  ];
  const k = report.correctness;
  lines.push(k.failures.length ? `  INCORRECT: ${k.failures.join('; ')}`
    : `  correct: no decoder errors, no corrupt, misnamed or older pictures, ended on frame ${k.lastSent}, `
      + `${k.inputsArrived}/${k.inputsSent} presses arrived in order`);
  for (const goal of report.goals) {
    lines.push(`  ${goal.met ? 'MET ' : 'MISS'} ${goal.name}: ${goal.measured} vs ${goal.target}`);
  }
  return lines.join('\n');
}

// --- the run --------------------------------------------------------------------

async function attachPage(browser, viewport) {
  const {result: {targetId}} = await browser.send('Target.createTarget', {url: 'about:blank'});
  const {result: {sessionId}} = await browser.send('Target.attachToTarget', {targetId, flatten: true});
  const errors = [];
  // The page's own errors and warnings, for an invalid run's diagnosis.
  const console_ = [];
  browser.on(message => {
    if (message.sessionId !== sessionId) return;
    if (message.method === 'Runtime.exceptionThrown') {
      const details = message.params.exceptionDetails;
      errors.push((details.exception?.description ?? details.text).slice(0, 400));
    } else if (message.method === 'Runtime.consoleAPICalled' && ['error', 'warning'].includes(message.params.type)
      && console_.length < 200) {
      console_.push(message.params.args.map(arg => arg.value ?? arg.description ?? '').join(' ').slice(0, 300));
    }
  });
  const send = (method, params = {}) => browser.send(method, params, sessionId);
  const evaluate = async (expression, ms = 60_000) => {
    const reply = await within(send('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true}), ms, 'the page');
    if (reply.error) throw Error(reply.error.message);
    if (reply.result.exceptionDetails) {
      const details = reply.result.exceptionDetails;
      throw Error(details.exception?.description ?? details.text);
    }
    return reply.result.result.value;
  };
  await send('Runtime.enable');
  await send('Page.enable');
  await send('Emulation.setDeviceMetricsOverride', {...viewport, mobile: false});
  await send('Emulation.setFocusEmulationEnabled', {enabled: true});
  await send('Page.addScriptToEvaluateOnNewDocument', {source: PROBE});
  return {send, evaluate, errors, console: console_};
}

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
  await page.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x: at.x, y: at.y});
  await sleep(300);
  for (let i = 0; i < inputs; i++) {
    presses.push(Math.round((performance.timeOrigin + performance.now()) * 1000));
    await page.send('Input.dispatchMouseEvent', {type: 'mousePressed', x: at.x, y: at.y, button: 'left', clickCount: 1});
    await page.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: at.x, y: at.y, button: 'left', clickCount: 1});
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
    const room = randomUUID();
    // The dev host's certificate is self-signed: WebTransport pins it by
    // hash, and its HTTPS routes need the exception.
    const flags = [...chromeFlags(options.renderer, process.getuid?.()), '--ignore-certificate-errors'];
    browser = await launch({args: flags});
    const version = (await browser.send('Browser.getVersion')).result;
    const page = await attachPage(browser, options.viewport);
    const url = `${pages.origin}${HARNESS_PATH}?${new URLSearchParams({origin, user: host.userPublicKey, room, profile: `stream-${room}`})}`;
    await page.send('Page.navigate', {url});
    // The desktop's first frame: SwiftShader compiles its pipelines first.
    await until(page, 'globalThis.harnessReady || globalThis.harnessError', 240_000, 'the desktop did not mount');
    const error = await page.evaluate('globalThis.harnessError ?? null');
    if (error) throw new Invalid(`the page did not connect: ${error}`);
    // The stream starts on a desktop already shown, with its live layer and
    // the legacy-window layer on it, so startup is the stream's own.
    await until(page, "globalThis.tiloMount?.canvas?.parentElement?.dataset.phase === 'presented'", 240_000,
      'the desktop presented no frame');
    await until(page, "Boolean(document.querySelector('.tilo-legacy-windows'))", 60_000,
      'the desktop opened no legacy-window layer (no live plane)');
    await until(page, 'globalThis.peer?.live?.available === true', 120_000,
      'the page\'s live plane did not come up').catch(async error => {
      throw new Invalid(`${error.message}: ${await page.evaluate(LIVE).catch(() => 'unavailable')}; page console: ${page.console.slice(-15).join(' | ')}; host log: ${relay.lines.slice(-30).join('\n').slice(-4000)}`);
    });
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
    const startSession = async (name, surface, seconds) => {
      const log = join(scratch, `${name}.jsonl`);
      const process_ = child(name, options.source, ['--dev-state', join(scratch, 'host'), '--host', host.irohAddr,
        '--room', room, '--out', log, '--width', String(options.window.width), '--height', String(options.window.height),
        '--seconds', String(seconds), '--surface', String(surface), '--codec', options.codec,
        ...(options.fps ? ['--fps', String(options.fps)] : [])],
      {RUST_LOG: process.env.STREAM_SOURCE_LOG ?? 'warn'});
      children.push(process_);
      const ready = JSON.parse(await process_.line(event('ready'), 120_000, 'its subscription'));
      const exited = () => within(new Promise(done => { if (process_.exited()) done(); else process_.process.on('exit', done); }),
        15_000, `the ${name}'s exit`);
      return {source: process_, session: ready, log, exited};
    };
    const {source, session, log: framesLog, exited} = await startSession('session', 7, options.warmUp + options.seconds);
    const mine = `${session.device}:`;
    const decoderBefore = await page.evaluate('JSON.parse(JSON.stringify(globalThis.__tiloStream.decoder))', 300_000);
    console.error(`session ready: ${session.encoder} at ${session.fps} fps`);
    await sleep(1000);
    source.process.stdin.write('start\n');
    const start = JSON.parse(await source.line(event('started'), 10_000, 'the stream start'));
    // Frames captured from here on are timed (the warm-up's are only judged).
    const measureFromUs = start.startUs + options.warmUp * 1e6;
    await sleep(Math.max(0, measureFromUs / 1000 - (performance.timeOrigin + performance.now())));
    const framesBefore = await desktopFrames();
    const stop = JSON.parse(await source.line(event('stopped'), (options.warmUp + options.seconds + 120) * 1000, 'the end of the stream'));
    const framesAfter = await desktopFrames();
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
    const probe = await page.evaluate('globalThis.__tiloStream', 120_000);
    const live = JSON.parse(await page.evaluate(LIVE));
    source.process.stdin.end();
    await exited();
    // This session's pictures, and what the decoders did since it started
    // (a decoder error at any time counts).
    const draws = probe.draws.filter(draw => draw.key?.startsWith(mine));
    const decoder = {...probe.decoder, created: probe.decoder.created - decoderBefore.created,
      chunks: probe.decoder.chunks - decoderBefore.chunks, keys: probe.decoder.keys - decoderBefore.keys,
      outputs: probe.decoder.outputs - decoderBefore.outputs, configs: probe.decoder.configs.slice(decoderBefore.configs.length)};
    const sent = readFileSync(framesLog, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line));
    const arrivals = source.lines.map(line => { try { return JSON.parse(line); } catch { return null; } })
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
    const analysis = analyse({sent, draws, start, stop, presses, arrivals, decoder, control, measureFromUs});
    analysis.counts.desktopStillWindowFramesPerSecond = stillWindow;
    analysis.counts.desktopFramesPerSecond = Number.isFinite(framesBefore) && Number.isFinite(framesAfter)
      ? Math.round((framesAfter - framesBefore) / ((stop.stopUs - measureFromUs) / 1e6) * 10) / 10 : null;
    const summary = Object.fromEntries(Object.entries(analysis.samples).map(([name, values]) => [name, stats(values)]));
    const report = {
      schema: 1, measuredAt: new Date().toISOString(), bundle, renderer: options.renderer, adapter,
      browser: {product: version.product, userAgent: version.userAgent}, viewport: options.viewport,
      host: {...before, before, after, maxLoad: options.maxLoad ?? null, platform: `${os.platform()} ${os.release()} ${os.arch()}`},
      session: {encoder: session.encoder, hardware: session.hardware, fps: session.fps, width: session.width, height: session.height,
        codec: session.codec ?? options.codec},
      live: {carrier: live.live?.carrier ?? null, state: live.live?.state ?? null},
      workload: {seconds: options.seconds, inputs: options.inputs, carrier: options.carrier, codec: options.codec, warmUpSeconds: options.warmUp,
        chromeFlags: flags},
      summary, counts: analysis.counts, correctness: analysis.correctness, goals: judgeGoals(summary, analysis.counts),
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
