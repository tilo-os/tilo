// Headless Chrome for page tests: a static server for a built directory, a
// browser on a throwaway profile, and a small DevTools Protocol client.
//
// The browser is `$CHROME`, else Chrome for Testing from Playwright's cache.
// Pages are served from 127.0.0.1, a secure context, so OPFS, Web Locks and
// WebGPU are available.
import {spawn} from 'node:child_process';
import {accessSync, constants as fsConstants, createReadStream, existsSync, mkdtempSync, readdirSync, rmSync, statSync} from 'node:fs';
import {createServer} from 'node:http';
import {homedir, tmpdir} from 'node:os';
import {extname, join, normalize} from 'node:path';
import {fileURLToPath} from 'node:url';

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

/** Serve `root` on 127.0.0.1; resolves to `{url, close}`. */
export function serve(root) {
  const server = createServer((request, response) => {
    const path = normalize(decodeURIComponent(new URL(request.url, 'http://x').pathname));
    let file = join(root, path);
    if (!file.startsWith(root)) { response.writeHead(403).end(); return; }
    if (existsSync(file) && statSync(file).isDirectory()) file = join(file, 'index.html');
    if (!existsSync(file)) { response.writeHead(404).end(); return; }
    response.writeHead(200, {
      'content-type': TYPES[extname(file)] ?? 'application/octet-stream',
      'cache-control': 'no-store',
    });
    createReadStream(file).pipe(response);
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

/** Where a throwaway profile goes: memory when the host has a tmpfs for it
 * (/dev/shm, else $XDG_RUNTIME_DIR), as tilo_paths::memory_temp_dir gives app
 * test profiles, else the temp directory. Nothing in it needs durability, and on
 * disk Chromium's exit waits on the profile's writeback: on a busy shared disk
 * its threads sat in the kernel past every shutdown deadline. */
function throwawayDir() {
  for (const dir of [process.platform === 'linux' ? '/dev/shm' : null, process.env.XDG_RUNTIME_DIR]) {
    if (!dir) continue;
    try {
      if (statSync(dir).isDirectory()) {
        accessSync(dir, fsConstants.W_OK);
        return dir;
      }
    } catch {
      // Not there or not writable: the next one.
    }
  }
  return tmpdir();
}

/** A headless browser on a fresh profile. */
export async function launch({args = []} = {}) {
  const profile = mkdtempSync(join(throwawayDir(), 'tilo-chrome-'));
  const supervised = process.platform === 'linux';
  const browser = chromeBinary();
  const browserArgs = [
    '--headless=new',
    '--remote-debugging-port=0',
    `--user-data-dir=${profile}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--enable-unsafe-webgpu',
    // Tabs a test drives in turn must not be throttled as background tabs.
    '--disable-background-timer-throttling',
    '--disable-renderer-backgrounding',
    '--disable-backgrounding-occluded-windows',
    ...args,
    'about:blank',
  ];
  // The existing subreaper owns even detached utility processes. Chromium's
  // root exiting alone does not prove that its profile has stopped changing.
  const child = spawn(supervised ? 'python3' : browser, supervised
    ? [fileURLToPath(new URL('./owned_browser.py', import.meta.url)), browser, ...browserArgs]
    : browserArgs, {stdio: ['ignore', 'ignore', 'pipe']});
  // The end of what the browser (or its supervisor) said: why a launch or a
  // shutdown failed is otherwise lost with the pipe.
  let said = '';
  const endpoint = await new Promise((resolve, reject) => {
    let buffered = '';
    child.stderr.on('data', chunk => {
      said = (said + chunk).slice(-4000);
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
  ws.addEventListener('message', ({data}) => {
    const message = JSON.parse(data);
    if (message.id && pending.has(message.id)) {
      pending.get(message.id)(message);
      pending.delete(message.id);
    } else {
      for (const listener of listeners) listener(message);
    }
  });
  const send = (method, params = {}, sessionId) => new Promise(resolve => {
    const n = ++id;
    pending.set(n, resolve);
    ws.send(JSON.stringify({id: n, method, params, sessionId}));
  });

  /** A tab: navigate, evaluate (awaiting promises), collect errors. In
   * `context` (from `context()`) when given, else the browser's own profile. */
  async function tab({context} = {}) {
    const {result: {targetId}} = await send('Target.createTarget',
      {url: 'about:blank', ...(context ? {browserContextId: context} : {})});
    const {result: {sessionId}} = await send('Target.attachToTarget', {targetId, flatten: true});
    const log = [];
    const listener = message => {
      if (message.sessionId !== sessionId) return;
      if (message.method === 'Runtime.exceptionThrown') {
        const details = message.params.exceptionDetails;
        log.push(`EXC ${(details.exception?.description ?? details.text).slice(0, 600)}`);
      }
      if (message.method === 'Runtime.consoleAPICalled' && ['error', 'warning'].includes(message.params.type)) {
        log.push(`${message.params.type.toUpperCase()} ${message.params.args.map(a => a.value ?? a.description ?? '').join(' ').slice(0, 600)}`);
      }
    };
    listeners.add(listener);
    await send('Runtime.enable', {}, sessionId);
    await send('Page.enable', {}, sessionId);
    const evaluate = async (expression, {timeout = 120000} = {}) => {
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
    const waitFor = async (expression, {timeout = 120000, interval = 250} = {}) => {
      const deadline = Date.now() + timeout;
      for (;;) {
        const value = await evaluate(expression).catch(() => undefined);
        if (value) return value;
        if (Date.now() > deadline) throw new Error(`timed out waiting for ${expression}`);
        await sleep(interval);
      }
    };
    return {
      targetId,
      sessionId,
      log,
      send: (method, params) => send(method, params, sessionId),
      evaluate,
      waitFor,
      navigate: url => send('Page.navigate', {url}, sessionId),
      reload: () => send('Page.reload', {ignoreCache: true}, sessionId),
      screenshot: async () => {
        const shot = await send('Page.captureScreenshot', {format: 'png'}, sessionId);
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
        await Promise.race([send('Browser.close'), exited, deadline]);
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
      // reaped. Keep filesystem retries bounded and report persistent errors.
      rmSync(profile, {recursive: true, force: true, maxRetries: 5, retryDelay: 100});
    },
  };
}
