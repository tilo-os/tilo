// Headless Chrome for page tests: a static server for a built directory, a
// browser on a throwaway profile, and a small DevTools Protocol client.
//
// The browser is `$CHROME`, else Chrome for Testing from Playwright's cache.
// Pages are served from 127.0.0.1, a secure context, so OPFS, Web Locks and
// WebGPU are available.
import {spawn} from 'node:child_process';
import {createHash, X509Certificate} from 'node:crypto';
import {accessSync, constants as fsConstants, readFileSync, existsSync, mkdtempSync, readdirSync, readlinkSync, rmSync, statSync, statfsSync} from 'node:fs';
import {createServer} from 'node:http';
import {isIP} from 'node:net';
import {homedir, hostname, tmpdir} from 'node:os';
import {extname, join, normalize, resolve} from 'node:path';
import {connect} from 'node:tls';
import {fileURLToPath} from 'node:url';
import {brotliCompressSync, constants as zlibConstants} from 'node:zlib';

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript',
  '.mjs': 'text/javascript',
  '.json': 'application/json',
  '.wasm': 'application/wasm',
  '.png': 'image/png',
  '.css': 'text/css',
  '.ttf': 'font/ttf',
  '.otf': 'font/otf',
  '.woff2': 'font/woff2',
};

export const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

/** Run in a page before its own scripts: the entry page's first screen (its
 * first visit's "Set up with your agent", web/first-run.mjs) counts as seen,
 * so a test that opens the site in a fresh profile gets the desktop it drives,
 * as a returning person does. A test of the first screen opts out
 * (`tab({firstRun: true})`). */
export const FIRST_RUN_SEEN = "try { localStorage.setItem('tilo-setup', JSON.stringify({step: 'done'})); } catch {}";

/** The certificate `origin` presents, as a pin for [`pinnedCertificates`]:
 * `{certificate, host}`. A check whose stack makes its certificate in memory (a
 * host's development identity) has no file to read, so it asks the listener.
 * The connection carries no request and nothing is trusted on its strength:
 * it answers what to pin, and the certificate is checked before a page loads
 * it. */
export function servedCertificate(origin) {
  const {hostname, port} = new URL(origin);
  return new Promise((decide, refuse) => {
    const socket = connect({host: hostname, port: Number(port || 443), rejectUnauthorized: false,
      ...(isIP(hostname) ? {} : {servername: hostname})}, () => {
      const presented = socket.getPeerCertificate(false)?.raw;
      socket.end();
      if (!presented?.length) {
        refuse(new Error(`${origin} presented no certificate`));
        return;
      }
      const body = presented.toString('base64').match(/.{1,64}/g).join('\n');
      decide({certificate: `-----BEGIN CERTIFICATE-----\n${body}\n-----END CERTIFICATE-----\n`, host: hostname});
    });
    socket.on('error', refuse);
  });
}

/** The Chromium flag that lets a page load these certificates and no others:
 * `--ignore-certificate-errors-spki-list` accepts a certificate error for a
 * connection whose chain names one of the pins, as a page's WebTransport pins
 * `serverCertificateHashes`. What it accepts is exactly the certificate the
 * stack was handed, and never a class of error: a check that passes a looser
 * flag than this hides a site no person's browser opens.
 *
 * A pin is `{certificate, host}` — PEM or DER, and the name the page dials it
 * by — and `sha256` where the stack announced the certificate's digest (a
 * host's `certificateSha256`), so what is pinned is the certificate the stack
 * says it serves. Each is checked here first: it names its host, and its dates
 * span now, because the flag would otherwise let the browser accept a
 * certificate it should refuse for exactly those reasons. */
export function pinnedCertificates(pins) {
  const hashes = pins.map(({certificate, host, sha256}) => {
    const parsed = new X509Certificate(certificate);
    if (sha256 && createHash('sha256').update(parsed.raw).digest('hex') !== String(sha256).toLowerCase()) {
      throw new Error(`${host} presents a certificate that is not the one it announced (${sha256})`);
    }
    const now = Date.now();
    if (!(Date.parse(parsed.validFrom) <= now && now <= Date.parse(parsed.validTo))) {
      throw new Error(`the certificate ${host} presents is valid ${parsed.validFrom} to ${parsed.validTo}, not now`);
    }
    const named = isIP(host) ? parsed.checkIP(host) : parsed.checkHost(host);
    if (!named) throw new Error(`the certificate ${host} presents does not name it`);
    const spki = parsed.publicKey.export({type: 'spki', format: 'der'});
    return createHash('sha256').update(spki).digest('base64');
  });
  return `--ignore-certificate-errors-spki-list=${hashes.join(',')}`;
}

/** Compressed copies by path, kept until the file they were made from changes:
 * a bundle's start module is tens of megabytes and a page load asks for it
 * again every run. */
const compressed = new Map();

/** `file` as the browser's `{body, encoding}`, brotli when it accepts it: a
 * deployment serves the bundle compressed, so the transfer a page waits on is
 * the compressed one. */
function body(file, accept) {
  // A PNG is already compressed.
  if (TYPES[extname(file)] === 'image/png' || !accept.includes('br')) {
    return {body: readFileSync(file), encoding: null};
  }
  const {mtimeMs, size} = statSync(file);
  const cached = compressed.get(file);
  if (cached && cached.mtimeMs === mtimeMs && cached.size === size) return cached.served;
  const body = brotliCompressSync(readFileSync(file), {
    params: {[zlibConstants.BROTLI_PARAM_QUALITY]: 5, [zlibConstants.BROTLI_PARAM_SIZE_HINT]: size},
  });
  const served = {body, encoding: 'br'};
  compressed.set(file, {mtimeMs, size, served});
  return served;
}

/** Compress everything under `root` before serving: a page load then waits on
 * the transfer, not on the server compressing it, as a deployment's edge
 * serves what it compressed ahead of time. */
function warm(root) {
  for (const entry of readdirSync(root, {recursive: true, withFileTypes: true})) {
    if (!entry.isFile()) continue;
    body(join(entry.parentPath, entry.name), 'br');
  }
}

/** Serve `root` on 127.0.0.1, or the first of `roots` that has the file: a
 * built `pkg/` beside the page's sources serves one origin, so a module that
 * imports `./pkg/…` finds it though it lives in another directory. Resolves
 * to `{url, close}`. */
export function serve(root, {also = []} = {}) {
  const roots = [root, ...also].map(dir => resolve(dir));
  for (const dir of roots) warm(dir);
  const server = createServer((request, response) => {
    const path = normalize(decodeURIComponent(new URL(request.url, 'http://x').pathname));
    const candidates = roots.map(dir => join(dir, path));
    if (!candidates.every(candidate => roots.some(dir => candidate.startsWith(dir)))) {
      response.writeHead(403).end();
      return;
    }
    let file = candidates.find(candidate => existsSync(candidate));
    if (!file) { response.writeHead(404).end(); return; }
    if (statSync(file).isDirectory()) file = join(file, 'index.html');
    if (!existsSync(file)) { response.writeHead(404).end(); return; }
    const {body: bytes, encoding} = body(file, request.headers['accept-encoding'] ?? '');
    response.writeHead(200, {
      'content-type': TYPES[extname(file)] ?? 'application/octet-stream',
      'cache-control': 'no-store',
      'content-length': bytes.length,
      ...(encoding ? {'content-encoding': encoding, vary: 'accept-encoding'} : {}),
    });
    response.end(bytes);
  });
  return new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve({
    url: `http://127.0.0.1:${server.address().port}`,
    close: () => new Promise(done => server.close(done)),
  })));
}

function chromeBinary() {
  if (process.env.CHROME) return process.env.CHROME;
  const cache = join(homedir(), 'Library/Caches/ms-playwright');
  const linuxCache = join(homedir(), '.cache/ms-playwright');
  for (const base of [cache, linuxCache]) {
    if (!existsSync(base)) continue;
    for (const dir of readdirSync(base).filter(d => /^chromium-\d+$/.test(d)).sort().reverse()) {
      for (const candidate of [
        'chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
        'chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
        'chrome-linux64/chrome',
        'chrome-linux/chrome',
      ]) {
        const path = join(base, dir, candidate);
        if (existsSync(path)) return path;
      }
    }
  }
  throw new Error('no Chrome: set CHROME or install Playwright\'s chromium');
}

/** Where a throwaway profile goes, in preference order: memory when the host has
 * a tmpfs for it (/dev/shm, else $XDG_RUNTIME_DIR), as tilo_paths::memory_temp_dir
 * gives app test profiles, else the temp directory. Nothing in it needs durability, and on
 * disk Chromium's exit waits on the profile's writeback: on a busy shared disk
 * its threads sat in the kernel past every shutdown deadline.
 *
 * A page's own store lives in the profile, so a directory with less room than
 * one page's work fills up and every save after it fails: a job container's
 * /dev/shm is 64 MiB, which one browser fills. */
const profileRoom = 1 << 30;

function profileHomes() {
  return [process.platform === 'linux' ? '/dev/shm' : null, process.env.XDG_RUNTIME_DIR, tmpdir()]
    .filter(Boolean);
}

function throwawayDir() {
  for (const dir of profileHomes()) {
    try {
      if (!statSync(dir).isDirectory()) continue;
      accessSync(dir, fsConstants.W_OK);
      const room = statfsSync(dir);
      if (room.bavail * room.bsize >= profileRoom) return dir;
    } catch {
      // Not there, not writable, or nothing to say about its room: the next one.
    }
  }
  return tmpdir();
}

/** A profile whose browser started less than this long ago is still starting. */
const startingProfileMs = 10 * 60_000;

/** Whether `profile`'s browser has stopped. Chromium names its owner in the
 * profile's `SingletonLock` symlink as `hostname-pid`, so a profile whose pid
 * is gone, or whose lock was never written and nobody has touched since, is a
 * leftover; anything this cannot prove is left alone. */
function abandoned(profile) {
  let lock;
  try {
    lock = readlinkSync(join(profile, 'SingletonLock'));
  } catch {
    return Date.now() - statSync(profile).mtimeMs > startingProfileMs;
  }
  const owner = /^(.*)-(\d+)$/.exec(lock);
  if (!owner || owner[1] !== hostname()) return false;
  try {
    process.kill(Number(owner[2]), 0);
    return false;
  } catch (error) {
    return error.code === 'ESRCH';
  }
}

/** Remove profiles whose browser stopped without a test noticing. A killed or
 * crashed test leaves its supervisor and Chromium behind, and each profile
 * holds hundreds of megabytes of memory-backed disk that only the test that
 * created it would have removed. */
function sweepProfiles() {
  for (const home of new Set(profileHomes())) {
    let names;
    try {
      names = readdirSync(home);
    } catch {
      continue;
    }
    for (const name of names) {
      if (!name.startsWith('tilo-chrome-')) continue;
      const profile = join(home, name);
      try {
        if (!abandoned(profile)) continue;
        rmSync(profile, {recursive: true, force: true, maxRetries: 5, retryDelay: 100});
      } catch {
        // Another launch removed it first, or it is not ours to judge.
      }
    }
  }
}

/** What every test browser starts with, so a page test passes only the flags
 * that are its own (a certificate pin list, a window size, a log).
 *
 * Software rendering on Linux: headless Chrome draws WebGPU with SwiftShader
 * unless a machine's GPU is named, and a host GPU in headless Chrome can lose
 * its device mid-run. A job container's /dev/shm is 64 MiB and a renderer that
 * runs out of it dies. Tabs a test drives in turn must not be throttled or
 * frozen as background tabs: a browser a person is not looking at still
 * answers. A build container runs as root, where Chromium refuses its own
 * sandbox. */
const BASELINE = [
  ...(process.platform === 'linux'
    ? ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader']
    : []),
  '--disable-dev-shm-usage',
  '--disable-background-timer-throttling',
  '--disable-renderer-backgrounding',
  '--disable-backgrounding-occluded-windows',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
];

/** A page that captures and plays media: a synthetic microphone and camera
 * the call really encodes and sends, no prompt before a person would have
 * pressed Allow, and audio that plays without a gesture. `media: 'ask'` keeps
 * the prompt for a flow that answers the page's permission itself. */
const MEDIA = ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream',
  '--autoplay-policy=no-user-gesture-required'];
const MEDIA_ASKS = ['--use-fake-device-for-media-stream', '--autoplay-policy=no-user-gesture-required'];

/** A headless browser on a fresh profile. `args` are the flags a test adds to
 * [`BASELINE`]; `media` is `true` for a page that captures and plays media,
 * or `'ask'` for one that answers the microphone prompt itself.
 *
 * `TILO_CHROME_ARGS` adds flags of the machine's own, space separated, after
 * every other: a run that wants the machine's GPU (a recording) passes
 * `--enable-gpu --use-angle=vulkan --enable-features=Vulkan
 * --ignore-gpu-blocklist` there, and Chromium takes the switch it read last. */
/**
 * The page-side wait `waitFor` evaluates: `expression` read now and again on
 * each change the page tells (`globalThis.tiloChanges`), each change to its
 * document, and each step of its loading, until it is truthy (answered) or
 * `withinMs` passed (answered `undefined`). A read that throws counts as not
 * yet. An expression may answer a promise.
 */
/**
 * The page-side pause until the page's next change (`globalThis.tiloChanges`),
 * or a change to its document while its modules are not in yet: answers
 * `true` on one, `false` when none came within `withinMs`.
 */
export function nextChange(withinMs) {
  const within = Math.max(0, Math.floor(withinMs));
  return `(globalThis.tiloChanges ? globalThis.tiloChanges.next(${within}) : new Promise(resolve => {
    const observer = new MutationObserver(() => { clearTimeout(timer); observer.disconnect(); resolve(true); });
    const timer = setTimeout(() => { observer.disconnect(); resolve(false); }, ${within});
    observer.observe(document, {subtree: true, childList: true, attributes: true, characterData: true});
  }))`;
}

export function untilTruthy(expression, withinMs) {
  return `new Promise(resolve => {
    let done = false, reading = false, again = false;
    const stops = [];
    const finish = value => {
      if (done) return;
      done = true;
      for (const stop of stops) stop();
      resolve(value);
    };
    const look = async () => {
      if (done) return;
      if (reading) { again = true; return; }
      reading = true;
      do {
        again = false;
        let value;
        try { value = await (${expression}); } catch { value = undefined; }
        if (value) { finish(value); break; }
      } while (again && !done);
      reading = false;
    };
    const timer = setTimeout(() => finish(undefined), ${Math.max(0, Math.floor(withinMs))});
    stops.push(() => clearTimeout(timer));
    const observer = new MutationObserver(look);
    observer.observe(document, {subtree: true, childList: true, attributes: true, characterData: true});
    stops.push(() => observer.disconnect());
    for (const [target, event] of [[document, 'readystatechange'], [globalThis, 'load'], [globalThis, 'hashchange'], [globalThis, 'popstate']]) {
      target.addEventListener(event, look);
      stops.push(() => target.removeEventListener(event, look));
    }
    // The page's own changes, once its modules are in: a document still
    // loading them tells its loading steps above until they are.
    let followed = null;
    const follow = () => {
      if (followed || !globalThis.tiloChanges?.listen) return;
      followed = globalThis.tiloChanges.listen(look);
      stops.push(followed);
    };
    follow();
    if (!followed) {
      const seek = new MutationObserver(() => { follow(); if (followed) seek.disconnect(); });
      seek.observe(document, {subtree: true, childList: true, attributes: true});
      stops.push(() => seek.disconnect());
    }
    void look();
  })`;
}

export async function launch({args = [], media = false} = {}) {
  sweepProfiles();
  const profile = mkdtempSync(join(throwawayDir(), 'tilo-chrome-'));
  const supervised = process.platform === 'linux';
  const browser = chromeBinary();
  const browserArgs = [
    '--headless=new',
    '--remote-debugging-port=0',
    `--user-data-dir=${profile}`,
    '--no-first-run',
    '--no-default-browser-check',
    // A throwaway profile has nothing to keep secret: never ask the macOS
    // keychain (or a Linux keyring) for the profile's encryption key, which
    // otherwise prompts the person at the machine on every launch.
    '--use-mock-keychain',
    '--password-store=basic',
    '--enable-unsafe-webgpu',
    ...BASELINE,
    ...(media === 'ask' ? MEDIA_ASKS : media ? MEDIA : []),
    // No crash reporter. In a job container its handler cannot trace the
    // browser's processes ("no ptrace"), and a renderer that asked it for a
    // report then died: about one reload in four in the agent-setup check
    // crashed its tab, none in eight runs without it. A renderer that does
    // crash still prints its signal and stack on stderr without it.
    '--disable-crash-reporter',
    '--disable-breakpad',
    ...args,
    ...(process.env.TILO_CHROME_ARGS ?? '').split(/\s+/).filter(Boolean),
    'about:blank',
  ];
  // The existing subreaper owns even detached utility processes. Chromium's
  // root exiting alone does not prove that its profile has stopped changing.
  // It also follows this process's death: a test that is killed must not leave
  // a browser rendering, and once its writers have stopped the supervisor
  // removes the profile it was given.
  const child = spawn(supervised ? 'python3' : browser, supervised
    ? [fileURLToPath(new URL('./owned_browser.py', import.meta.url)), browser, ...browserArgs]
    : browserArgs, {
    stdio: ['ignore', 'ignore', 'pipe'],
    ...(supervised ? {env: {
      ...process.env,
      TILO_OWNED_BROWSER_PARENT: String(process.pid),
      TILO_OWNED_BROWSER_PROFILE: profile,
    }} : {}),
  });
  // The end of what the browser (or its supervisor) said: why a launch or a
  // shutdown failed is otherwise lost with the pipe.
  let said = '';
  const endpoint = await new Promise((resolve, reject) => {
    let buffered = '';
    child.stderr.on('data', chunk => {
      said = (said + chunk).slice(-64000);
      if (buffered === null) return;
      buffered += chunk;
      const match = buffered.match(/DevTools listening on (ws:\/\/\S+)/);
      if (match) { buffered = null; resolve(match[1]); }
    });
    child.on('exit', code => reject(new Error(`Chrome exited (${code}): ${said.slice(-400)}`)));
  });
  // Browser and supervisor lines that explain a failed shutdown, not
  // Chrome's routine noise.
  const shutdownLines = () => said.split('\n')
    .filter(line => !/registration_request|DEPRECATED_ENDPOINT/.test(line)).slice(-12).join('\n');
  const ws = new WebSocket(endpoint);
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, {once: true});
    ws.addEventListener('error', reject, {once: true});
  });
  let id = 0;
  const pending = new Map();
  const listeners = new Set();
  // Nothing a test asks of a page takes longer than this, so a command still
  // unanswered then is a browser or a page that stopped: the test that is
  // waiting on it fails and names the command, instead of waiting for ever.
  const ANSWER_TIMEOUT_MS = 180_000;
  // A screenshot comes with the page's next frame: one that takes longer was
  // asked of a document the page left.
  const CAPTURE_TIMEOUT_MS = 15_000;
  const stop = error => {
    for (const waiting of pending.values()) waiting.reject(error);
    pending.clear();
  };
  ws.addEventListener('message', ({data}) => {
    const message = JSON.parse(data);
    const waiting = message.id && pending.get(message.id);
    if (waiting) {
      pending.delete(message.id);
      waiting.resolve(message);
    } else {
      for (const listener of listeners) listener(message);
    }
  });
  ws.addEventListener('close', () => stop(new Error('the browser\'s connection closed')));
  ws.addEventListener('error', () => stop(new Error('the browser\'s connection failed')));
  // What the browser said that explains a process that died: a page whose
  // renderer was killed says so here, and a test that would otherwise wait out
  // its own step hears it at once.
  const died = () => said.split('\n')
    .filter(line => /crash|kill|OOM|out of memory|SIGSEGV|SIGILL|SIGKILL|SIGABRT|FATAL/i.test(line))
    .slice(-6).map(line => line.trim()).join(' | ');
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const n = ++id;
    const timer = setTimeout(() => {
      pending.delete(n);
      const why = died();
      reject(new Error(`${method}: the browser stopped answering for ${ANSWER_TIMEOUT_MS / 1000}s${why ? ` (what it said: ${why})` : ''}`));
    }, ANSWER_TIMEOUT_MS);
    timer.unref?.();
    pending.set(n, {resolve: message => { clearTimeout(timer); resolve(message); },
      reject: error => { clearTimeout(timer); reject(error); }});
    ws.send(JSON.stringify({id: n, method, params, sessionId}));
  });
  // Why a page's renderer ended, as the browser itself reports it: the
  // per-page `Inspector.targetCrashed` says only that it died, while
  // `Target.targetCrashed` says how — `killed` with the signal the process
  // exited by (9 for an OOM kill's SIGKILL, 11 for a fault in native code),
  // or `oom` where the browser ended it for memory. A run then names the
  // reason instead of only the death.
  const crashedTargets = new Map();
  send('Target.setDiscoverTargets', {discover: true}).catch(() => {});
  listeners.add(message => {
    if (message.method !== 'Target.targetCrashed') return;
    const {targetId, status, errorCode} = message.params ?? {};
    crashedTargets.set(targetId, `${status ?? 'no status'}${errorCode === undefined ? '' : `, exit code ${errorCode}`}`);
  });
  const crashOf = targetId => crashedTargets.get(targetId) ?? null;

  /** A tab: navigate, evaluate (awaiting promises), collect errors. In
   * `context` (from `context()`) when given, else the browser's own profile.
   * The entry page's first screen counts as seen unless `firstRun`. */
  async function tab({context, firstRun = false} = {}) {
    const {result: {targetId}} = await send('Target.createTarget',
      {url: 'about:blank', ...(context ? {browserContextId: context} : {})});
    const {result: {sessionId}} = await send('Target.attachToTarget', {targetId, flatten: true});
    const log = [];
    // A renderer that died answers nothing: every command on it would wait
    // out the whole answer timeout, so the tab says it died once, here, where
    // a flow's failure dump shows it, and refuses further commands at once.
    let dead = null;
    // Each new document's script context, counted: a wait whose document
    // went away carries on once the next one is there.
    let documents = 0;
    const loaded = new Set();
    const listener = message => {
      if (message.sessionId !== sessionId) return;
      if (message.method === 'Runtime.executionContextCreated' && message.params?.context?.auxData?.isDefault) {
        documents += 1;
        for (const done of [...loaded]) done();
      }
      if (message.method === 'Inspector.targetCrashed') {
        // Why, where the browser has said: its own report for this page's
        // target, which names the exit or says it ended it for memory.
        dead = `the renderer process died${crashOf(targetId) ? ` (${crashOf(targetId)})` : ''}`;
        log.push(`CRASH ${dead} (the browser: ${died() || 'said nothing'})`);
        // The browser's own report can land a moment after this event.
        setTimeout(() => {
          const why = crashOf(targetId);
          if (!why) return;
          dead = `the renderer process died (${why})`;
          log.push(`CRASH the browser reported: ${why}`);
        }, 1_000).unref?.();
        return;
      }
      if (message.method === 'Runtime.exceptionThrown') {
        const details = message.params.exceptionDetails;
        // The whole stack: a Wasm trap's first frames are the panic or
        // allocation machinery, and the frame that caused it comes later.
        // A thrown value that is not an Error has no description: show the
        // value, and where it was thrown.
        const thrown = details.exception?.description ?? `${details.text} ${JSON.stringify(details.exception?.value ?? details.exception?.className ?? null)}`;
        const at = details.stackTrace?.callFrames?.slice(0, 4).map(f => `${f.functionName}@${f.url.split('/').pop()}:${f.lineNumber}`).join(' < ') ?? `${details.url ?? ''}:${details.lineNumber}`;
        log.push(`EXC ${thrown.slice(0, 8000)} at ${at}`);
      }
      if (message.method === 'Runtime.consoleAPICalled' && ['error', 'warning'].includes(message.params.type)) {
        log.push(`${message.params.type.toUpperCase()} ${message.params.args.map(a => a.value ?? a.description ?? '').join(' ').slice(0, 2000)}`);
      }
    };
    listeners.add(listener);
    await send('Runtime.enable', {}, sessionId);
    await send('Page.enable', {}, sessionId);
    // The domain that reports a renderer's death (`Inspector.targetCrashed`).
    await send('Inspector.enable', {}, sessionId).catch(() => {});
    if (!firstRun) await send('Page.addScriptToEvaluateOnNewDocument', {source: FIRST_RUN_SEEN}, sessionId);
    const evaluate = async (expression, {timeout = 120000} = {}) => {
      if (dead) throw new Error(`${expression}: ${dead}`);
      const reply = await send('Runtime.evaluate', {
        expression, awaitPromise: true, returnByValue: true, timeout,
      }, sessionId);
      if (reply.error) throw new Error(`${expression}: ${reply.error.message}`);
      if (reply.result.exceptionDetails) {
        const details = reply.result.exceptionDetails;
        throw new Error(`${expression}: ${details.exception?.description ?? details.text}`);
      }
      return reply.result.result.value;
    };
    // Waits in the page, reading `expression` again whenever the page says
    // something it shows changed (`globalThis.tiloChanges`, which the
    // page's mounts and session tell on every status), its document changed,
    // or it loaded further: never on a clock. A navigation ends the document
    // the wait ran in, and it carries on in the next one.
    const waitFor = async (expression, {timeout = 120000} = {}) => {
      const deadline = Date.now() + timeout;
      for (;;) {
        const left = deadline - Date.now();
        if (left <= 0) throw new Error(`timed out waiting for ${expression}`);
        if (dead) throw new Error(`${expression}: ${dead}`);
        const document = documents;
        const value = await evaluate(untilTruthy(expression, left), {timeout: left + 5_000})
          .catch(() => undefined);
        if (value) return value;
        // The document went away under the wait (a navigation, a reload):
        // the next one is waited on once it is there.
        if (documents === document) await nextDocument(deadline - Date.now());
      }
    };
    const nextDocument = within => new Promise(resolve => {
      const done = () => { clearTimeout(timer); loaded.delete(done); resolve(); };
      // Never what keeps the run going: a wait that lost its race (a flow
      // that failed or finished meanwhile) ends with the process.
      const timer = setTimeout(done, Math.max(0, within));
      timer.unref?.();
      loaded.add(done);
    });
    // One capture at a time: Chromium answers two overlapping
    // `Page.captureScreenshot`s (a recording's frame and a shot) with frames
    // of each other's size, so a shot came back as the page tiled 2x2.
    let capturing = Promise.resolve();
    // Captures go one at a time. One asked for while the page loads another
    // document (a link opened in it, a reload) can wait for a frame of the
    // document that went away and is never answered: it gives up after
    // CAPTURE_TIMEOUT_MS, so the next capture asks the new document rather than
    // queueing behind it for the whole answer timeout.
    const capture = params => {
      const taking = capturing.then(() => {
        let timer;
        const lost = new Promise((_, refuse) => {
          timer = setTimeout(() => refuse(new Error(`Page.captureScreenshot: no frame came in ${CAPTURE_TIMEOUT_MS / 1000}s (the page was loading another document?)`)), CAPTURE_TIMEOUT_MS);
          timer.unref?.();
        });
        return Promise.race([send('Page.captureScreenshot', params, sessionId), lost]).finally(() => clearTimeout(timer));
      });
      capturing = taking.catch(() => undefined);
      return taking;
    };
    return {
      targetId,
      sessionId,
      log,
      /** Why the tab's renderer is gone, or `null` while it answers: the
       * browser's own report of how it ended once that has landed, since the
       * death alone tells a run nothing it can act on. */
      crash: () => (dead && crashOf(targetId) ? `the renderer process died (${crashOf(targetId)})` : dead),
      send: (method, params) => method === 'Page.captureScreenshot' ? capture(params) : send(method, params, sessionId),
      evaluate,
      waitFor,
      /** How many documents the tab started: a read that saw the count stay
       * the same read the tab's current document. */
      documents: () => documents,
      /** Resolves once the tab started its next document, or `within` ms. */
      nextDocument,
      /** Bring the tab to the front: the browser answers a passkey or a
       * permission prompt in the tab the person is looking at, and puts a tab
       * nobody looks at to sleep. A tab already closed has nothing to bring. */
      front: () => send('Page.bringToFront', {}, sessionId).catch(() => {}),
      navigate: url => send('Page.navigate', {url}, sessionId),
      reload: () => send('Page.reload', {ignoreCache: true}, sessionId),
      screenshot: async () => {
        const shot = await capture({format: 'png'});
        return shot.result?.data ? Buffer.from(shot.result.data, 'base64') : null;
      },
      close: async () => {
        listeners.delete(listener);
        await send('Target.closeTarget', {targetId});
      },
    };
  }

  return {
    tab,
    /** Recent browser-process diagnostics, including GPU-process failures. */
    diagnostics: () => shutdownLines(),
    /** The end of what the browser said, without its routine noise: with
     * `--enable-logging=stderr` Chromium's own log, which tells why a process died. */
    crashLines: () => said.split('\n')
      .filter(line => line.trim() && !/registration_request|DEPRECATED_ENDPOINT|adjust OOM score/.test(line))
      .slice(-80).join('\n'),
    /** A fresh browser context: storage, cookies and Web Locks of its own, as
     * another person's browser profile would have. Resolves to its id. */
    context: async () => (await send('Target.createBrowserContext', {disposeOnDetach: true})).result.browserContextId,
    send,
    /** Observe every protocol event; returns the unsubscribe function. */
    on: listener => { listeners.add(listener); return () => listeners.delete(listener); },
    close: async () => {
      const exited = child.exitCode === null
        ? new Promise(resolve => child.once('exit', resolve))
        : Promise.resolve();
      let timer;
      const deadline = new Promise(resolve => {
        timer = setTimeout(() => resolve(false), 10_000);
      });
      // A wedged CDP close must not consume the supervisor's cleanup budget.
      // Request bounded termination while the original ten-second deadline
      // still covers reaping every owned writer. Never kill the subreaper.
      const stopOwned = supervised ? setTimeout(() => child.kill('SIGTERM'), 2_000) : null;
      let closed;
      try {
        // A browser that closes as it is asked answers with a closed
        // connection; what closes the browser is its exit, raced below.
        const asked = send('Browser.close').catch(() => undefined);
        await Promise.race([asked, exited, deadline]);
        closed = await Promise.race([exited.then(() => true), deadline]);
      } finally {
        clearTimeout(timer);
        clearTimeout(stopOwned);
        ws.close();
      }
      if (!closed) {
        if (supervised) {
          // Keep the subreaper alive to stop its own descendants; killing it
          // would orphan precisely the writers whose exit we need to prove.
          child.kill('SIGTERM');
          throw new Error(`owned browser shutdown timed out; profile retained\n${shutdownLines()}`);
        }
        child.kill('SIGKILL');
        await exited;
      }
      if (supervised && child.exitCode !== 0) {
        throw new Error(`owned browser shutdown failed (${child.exitCode}); profile retained\n${shutdownLines()}`);
      }
      // Only the supervisor's successful exit proves every owned writer was
      // reaped. It removes the profile it was given by then; this removal is
      // the backup for the paths that never reached it. Keep filesystem
      // retries bounded and report persistent errors.
      rmSync(profile, {recursive: true, force: true, maxRetries: 5, retryDelay: 100});
    },
  };
}
