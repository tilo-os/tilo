// Serve a built desktop bundle for local development and the smoke test, and
// optionally the fetch proxy that lets apps opting into
// `tilo_net::proxy::ProxyUse::CrossOrigin` (api-tester, feeds, tube,
// calendar) read sites without CORS headers. The proxy is the one workspace
// hosts serve (`tilo_net::proxy::host`, wire protocol v1), run here as the
// `tilo-fetch-proxy` program on loopback; this script only forwards
// /_tilo/net/fetch to it and tells the page where it is.
//
// usage: node serve.mjs BUNDLE_DIR [--port 8787] [--fetch-proxy] [--allow-private]
//                                  [--proxy-bin PATH]
//   --fetch-proxy    serve /_tilo/net/fetch and tell the page about it
//   --allow-private  let the proxy reach loopback and private addresses
//                    (a local test server); refused by default
//   --proxy-bin      the tilo-fetch-proxy program (default
//                    $TILO_FETCH_PROXY_BIN, else target/{debug,release}, else
//                    `cargo run -p tilo-net --features proxy-dev-server`)
//
// Development only: requests are unauthenticated, so it listens on loopback.
import {spawn} from 'node:child_process';
import {createReadStream, existsSync} from 'node:fs';
import {readFile, stat} from 'node:fs/promises';
import {createServer, request as httpRequest} from 'node:http';
import path from 'node:path';
import {createInterface} from 'node:readline';
import {fileURLToPath} from 'node:url';

const args = process.argv.slice(2);
const valued = new Set(['--port', '--proxy-bin']);
const root = path.resolve(args.find((a, i) => !a.startsWith('--') && !valued.has(args[i - 1])) ?? '.');
const flag = name => args.includes(name);
const option = name => (args.includes(name) ? args[args.indexOf(name) + 1] : undefined);
const port = Number(option('--port') ?? 8787);
const proxyEnabled = flag('--fetch-proxy');
const allowPrivate = flag('--allow-private');
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..');

export const ENDPOINT = '/_tilo/net/fetch';

const TYPES = {'.html': 'text/html; charset=utf-8', '.mjs': 'text/javascript', '.js': 'text/javascript',
  '.wasm': 'application/wasm', '.json': 'application/json', '.css': 'text/css',
  '.ttf': 'font/ttf', '.otf': 'font/otf', '.woff2': 'font/woff2', '.png': 'image/png', '.svg': 'image/svg+xml'};

/** The command that runs tilo-fetch-proxy. */
function proxyCommand() {
  const named = option('--proxy-bin') ?? process.env.TILO_FETCH_PROXY_BIN;
  const built = ['target/debug/tilo-fetch-proxy', 'target/release/tilo-fetch-proxy']
    .map(file => path.join(repo, file)).find(existsSync);
  const program = named ?? built;
  const proxyArgs = ['--listen', '127.0.0.1:0', ...(allowPrivate ? ['--allow-private'] : [])];
  if (program) return [program, proxyArgs];
  return ['cargo', ['run', '--quiet', '-p', 'tilo-net', '--features', 'proxy-dev-server',
    '--bin', 'tilo-fetch-proxy', '--', ...proxyArgs]];
}

/** Start tilo-fetch-proxy; resolves to its origin once it listens. */
function startProxy() {
  const [program, proxyArgs] = proxyCommand();
  const child = spawn(program, proxyArgs, {cwd: repo, stdio: ['ignore', 'pipe', 'inherit']});
  process.on('exit', () => child.kill());
  for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => process.exit(0));
  return new Promise((resolve, reject) => {
    child.on('error', reject);
    child.on('exit', code => reject(new Error(`tilo-fetch-proxy exited (${code}) before it listened`)));
    createInterface({input: child.stdout}).once('line', line => {
      try {
        resolve(JSON.parse(line).listening);
      } catch {
        reject(new Error(`tilo-fetch-proxy said ${line}`));
      }
    });
  });
}

/** Forward one proxy request to tilo-fetch-proxy and stream its answer
 * back; a body it breaks off (a bound reached) breaks off here too. */
function forward(proxyOrigin, request, response) {
  const upstream = httpRequest(new URL(request.url, proxyOrigin), {method: request.method, headers: request.headers}, answer => {
    response.writeHead(answer.statusCode, answer.headers);
    answer.pipe(response);
    answer.on('aborted', () => response.destroy());
    answer.on('error', () => response.destroy());
  });
  upstream.on('error', error => {
    if (response.headersSent) { response.destroy(); return; }
    response.writeHead(502, {'x-tilo-proxy-error': 'unreachable', 'content-type': 'text/plain; charset=utf-8'});
    response.end(`the development fetch proxy is not reachable: ${error.message}`);
  });
  request.pipe(upstream);
}

async function serveFile(request, response) {
  const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
  let file = path.join(root, pathname === '/' ? 'index.html' : pathname);
  if (!file.startsWith(root)) { response.writeHead(403).end(); return; }
  if ((await stat(file).catch(() => null))?.isDirectory()) file = path.join(file, 'index.html');
  const info = await stat(file).catch(() => null);
  if (!info?.isFile()) { response.writeHead(404).end('not found'); return; }
  const type = TYPES[path.extname(file)] ?? 'application/octet-stream';
  if (proxyEnabled && file.endsWith('.html')) {
    // How a host on the page's own origin names the endpoint to tilo-net.
    const tag = `<script>globalThis.__tiloFetchProxy = ${JSON.stringify(ENDPOINT)};</script>`;
    const page = Buffer.from((await readFile(file, 'utf8')).replace('<head>', `<head>${tag}`));
    response.writeHead(200, {'content-type': type, 'content-length': page.length, 'cache-control': 'no-cache'});
    response.end(page);
    return;
  }
  // Streamed, with its length: under Bun one write of a large file (the
  // 32 MB CJK font) arrived cut short while the page loaded others, and
  // the page drew every CJK character and emoji as a box.
  response.writeHead(200, {'content-type': type, 'content-length': info.size, 'cache-control': 'no-cache'});
  createReadStream(file).on('error', () => response.destroy()).pipe(response);
}

const proxyOrigin = proxyEnabled ? await startProxy() : undefined;
const server = createServer(async (request, response) => {
  if (proxyOrigin && new URL(request.url, 'http://localhost').pathname === ENDPOINT) {
    forward(proxyOrigin, request, response);
    return;
  }
  try {
    await serveFile(request, response);
  } catch (error) {
    if (!response.headersSent) response.writeHead(500, {'content-type': 'text/plain; charset=utf-8'});
    response.end(String(error));
  }
});
server.listen(port, '127.0.0.1', () => {
  console.log(`serving ${root} on http://127.0.0.1:${port}/${proxyEnabled ? ` with the fetch proxy at ${ENDPOINT}` : ''}`);
});
