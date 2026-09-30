// Serve a built desktop bundle for local development and the smoke test, and
// optionally the same-origin fetch proxy that lets apps opting into
// `tilo_net::proxy::ProxyUse::CrossOrigin` (api-tester, feeds, calendar) read
// sites without CORS headers. Implements wire protocol v1 from
// crates/platform/tilo-net/src/proxy.rs; a production host (Tilo Cloud)
// implements the same contract behind its own session.
//
// usage: node serve.mjs BUNDLE_DIR [--port 8787] [--fetch-proxy] [--allow-private]
//   --fetch-proxy    serve /_tilo/net/fetch and tell the page about it
//   --allow-private  let the proxy reach loopback and private addresses
//                    (a local test server); refused by default
//
// Development only: requests are unauthenticated, so it listens on loopback.
import {createServer} from 'node:http';
import {readFile, stat} from 'node:fs/promises';
import {lookup} from 'node:dns/promises';
import {isIP} from 'node:net';
import path from 'node:path';

const args = process.argv.slice(2);
const root = path.resolve(args.find(a => !a.startsWith('--') && !/^\d+$/.test(a)) ?? '.');
const flag = name => args.includes(name);
const portIndex = args.indexOf('--port');
const port = Number(portIndex >= 0 ? args[portIndex + 1] : 8787);
const proxyEnabled = flag('--fetch-proxy');
const allowPrivate = flag('--allow-private');

export const ENDPOINT = '/_tilo/net/fetch';
const MARKER = 'x-tilo-proxy';
const FORWARD = 'x-tilo-forward-';
const FINAL_URL = 'x-tilo-proxy-final-url';
const ERROR = 'x-tilo-proxy-error';
const MAX_REQUEST = 16 * 1024 * 1024;
const MAX_RESPONSE = 64 * 1024 * 1024;
const TIMEOUT_MS = 30_000;
const MAX_REDIRECTS = 10;
// Never forwarded in either direction: connection management, and the page's
// own identity at the proxy's origin.
const HOP = new Set(['connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization',
  'te', 'trailer', 'transfer-encoding', 'upgrade', 'host', 'content-length',
  'cookie', 'origin', 'referer']);
// Response headers the page's origin must not act on: a cookie it would
// store, and an authentication challenge that makes the browser prompt for a
// login to the page's own origin (a tab-modal prompt that blocks all input).
const RENAMED = new Set(['set-cookie', 'www-authenticate']);

const TYPES = {'.html': 'text/html; charset=utf-8', '.mjs': 'text/javascript', '.js': 'text/javascript',
  '.wasm': 'application/wasm', '.json': 'application/json', '.css': 'text/css',
  '.ttf': 'font/ttf', '.otf': 'font/otf', '.woff2': 'font/woff2', '.png': 'image/png', '.svg': 'image/svg+xml'};

class ProxyFailure extends Error {
  constructor(code, status, message) { super(message); this.code = code; this.status = status; }
}

// Loopback, private, link-local, unique-local, CGNAT, multicast and
// unspecified addresses: the proxy's own network.
export function privateAddress(address) {
  if (isIP(address) === 4) {
    const [a, b] = address.split('.').map(Number);
    return a === 0 || a === 10 || a === 127 || (a === 100 && b >= 64 && b < 128) ||
      (a === 169 && b === 254) || (a === 172 && b >= 16 && b < 32) || (a === 192 && b === 168) || a >= 224;
  }
  const v6 = address.toLowerCase();
  if (v6.startsWith('::ffff:')) return privateAddress(v6.slice(7));
  return v6 === '::' || v6 === '::1' || v6.startsWith('fc') || v6.startsWith('fd') ||
    v6.startsWith('fe8') || v6.startsWith('fe9') || v6.startsWith('fea') || v6.startsWith('feb') || v6.startsWith('ff');
}

async function checkDestination(url) {
  if (url.protocol !== 'http:' && url.protocol !== 'https:') {
    throw new ProxyFailure('bad-request', 400, `${url.protocol} is not http or https`);
  }
  if (allowPrivate) return;
  const host = url.hostname.replace(/^\[|\]$/g, '');
  const addresses = isIP(host) ? [{address: host}] : await lookup(host, {all: true}).catch(() => {
    throw new ProxyFailure('unreachable', 502, `${host} does not resolve`);
  });
  if (addresses.some(({address}) => privateAddress(address))) {
    throw new ProxyFailure('blocked', 403, `${host} is on the proxy's own network`);
  }
}

async function readBody(request) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > MAX_REQUEST) throw new ProxyFailure('too-large', 413, 'the request body is too large');
    chunks.push(chunk);
  }
  return size ? Buffer.concat(chunks) : undefined;
}

// A header the page asked to forward replaces the browser's own of that name
// (its user-agent, say), whatever order they arrive in.
function outgoingHeaders(incoming) {
  const headers = {};
  const forwarded = {};
  for (const [name, value] of Object.entries(incoming)) {
    if (name === MARKER || HOP.has(name) || name.startsWith('sec-')) continue;
    if (name.startsWith(FORWARD)) forwarded[name.slice(FORWARD.length)] = value;
    else if (!name.startsWith('x-tilo-')) headers[name] = value;
  }
  return {...headers, ...forwarded};
}

async function relay(request, response) {
  if (request.headers[MARKER] !== '1') {
    throw new ProxyFailure('bad-request', 400, `${MARKER}: 1 is required`);
  }
  const target = new URL(request.url, 'http://localhost').searchParams.get('url');
  let url;
  try { url = new URL(target ?? ''); } catch { throw new ProxyFailure('bad-request', 400, 'url is missing or not absolute'); }
  let method = request.method;
  let body = await readBody(request);
  const headers = outgoingHeaders(request.headers);
  const deadline = AbortSignal.timeout(TIMEOUT_MS);
  let answer;
  for (let hop = 0; ; hop++) {
    await checkDestination(url);
    try {
      answer = await fetch(url, {method, headers, body, redirect: 'manual', signal: deadline});
    } catch (error) {
      if (deadline.aborted) throw new ProxyFailure('timeout', 504, `${url.host} did not answer in time`);
      throw new ProxyFailure('unreachable', 502, `${url.host}: ${error.cause?.code ?? error.message}`);
    }
    const location = answer.headers.get('location');
    if (![301, 302, 303, 307, 308].includes(answer.status) || !location) break;
    if (hop === MAX_REDIRECTS) throw new ProxyFailure('unreachable', 502, 'too many redirects');
    const next = new URL(location, url);
    if (next.origin !== url.origin) delete headers.authorization;
    if (answer.status === 303 || ((answer.status === 301 || answer.status === 302) && method === 'POST')) {
      method = 'GET';
      body = undefined;
    }
    url = next;
  }
  const declared = Number(answer.headers.get('content-length') ?? 0);
  if (declared > MAX_RESPONSE) throw new ProxyFailure('too-large', 413, 'the response is too large');
  const out = {[FINAL_URL]: url.href, 'cache-control': 'no-store'};
  answer.headers.forEach((value, name) => {
    if (HOP.has(name) || name === 'content-encoding') return;
    out[RENAMED.has(name) ? `${FORWARD}${name}` : name] = value;
  });
  response.writeHead(answer.status, out);
  let size = 0;
  for await (const chunk of answer.body ?? []) {
    size += chunk.length;
    if (size > MAX_RESPONSE) { response.destroy(); return; }
    response.write(chunk);
  }
  response.end();
}

async function serveFile(request, response) {
  const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
  let file = path.join(root, pathname === '/' ? 'index.html' : pathname);
  if (!file.startsWith(root)) { response.writeHead(403).end(); return; }
  if ((await stat(file).catch(() => null))?.isDirectory()) file = path.join(file, 'index.html');
  let bytes = await readFile(file).catch(() => null);
  if (!bytes) { response.writeHead(404).end('not found'); return; }
  if (proxyEnabled && file.endsWith('.html')) {
    // How a host hands the endpoint to tilo-net without a Rust call.
    const tag = `<script>globalThis.__tiloFetchProxy = ${JSON.stringify(ENDPOINT)};</script>`;
    bytes = Buffer.from(bytes.toString('utf8').replace('<head>', `<head>${tag}`));
  }
  response.writeHead(200, {'content-type': TYPES[path.extname(file)] ?? 'application/octet-stream',
    'cache-control': 'no-cache'});
  response.end(bytes);
}

const server = createServer(async (request, response) => {
  try {
    if (proxyEnabled && new URL(request.url, 'http://localhost').pathname === ENDPOINT) {
      await relay(request, response);
    } else {
      await serveFile(request, response);
    }
  } catch (error) {
    const failure = error instanceof ProxyFailure ? error : new ProxyFailure('unreachable', 502, String(error));
    if (!response.headersSent) {
      response.writeHead(failure.status, {[ERROR]: failure.code, 'content-type': 'text/plain; charset=utf-8'});
    }
    response.end(failure.message);
  }
});
server.listen(port, '127.0.0.1', () => {
  console.log(`serving ${root} on http://127.0.0.1:${port}/${proxyEnabled ? ` with the fetch proxy at ${ENDPOINT}` : ''}`);
});
