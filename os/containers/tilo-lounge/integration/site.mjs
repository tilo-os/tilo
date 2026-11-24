// The site's host for the Lounge integration test: the web entry's bundle,
// served as the web entry image serves it, with the entry configuration this
// deployment writes — its home is the Lounge, joined through the entry the
// relay lists it at, and its directory is the relay's. What it has no file
// for it hands to the server (the web entry image's `TILO_WEB_SERVER`).
//
// usage: node site.mjs SITE_DIR CERT KEY WORKSPACE_ORIGIN SERVER_ORIGIN NAME ADMISSION PORT HOST_INFO
// SERVER_ORIGIN is the server's own web listener, HTTPS on the certificate it
// makes: this hop skips verifying it, as the page does.
// HOST_INFO is the line the workspace's host writes in `web-host.json` for its
// host, which the web entry image hands its page from TILO_WEB_HOME_HOST_INFO
// so the page dials that host as named; an empty HOST_INFO is only for a site
// whose host is not there to ask.
// Prints {"url": "https://127.0.0.1:<port>"} once it listens.
import {readFileSync, statSync, existsSync} from 'node:fs';
import {createServer} from 'node:https';
import {request as plainRequest} from 'node:http';
import {request as secureRequest} from 'node:https';
import {extname, join, normalize} from 'node:path';

const [site, cert, key, origin, serverOrigin, name, admission, port, info] = process.argv.slice(2);
if (!site || !cert || !key || !origin || !serverOrigin || !name || !admission || !port || info === undefined) {
  console.error('usage: node site.mjs SITE_DIR CERT KEY WORKSPACE_ORIGIN SERVER_ORIGIN NAME ADMISSION PORT HOST_INFO');
  process.exit(2);
}

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

// What the web entry image writes as its entry configuration from
// TILO_WEB_HOME_HOST, TILO_WEB_HOME_HOST_INFO and TILO_WEB_HOME_ENTRY, for a site whose home workspace is hosted at `origin`.
const entry = JSON.stringify({
  home: {name, host: info ? {origin, info} : {origin}, admission},
  directory: `${origin}/peer/v1/directory`,
});

/** The file at `path` below the bundle, or `null`. */
const fileAt = path => {
  let file = join(site, path);
  if (!file.startsWith(site)) return null;
  if (existsSync(file) && statSync(file).isDirectory()) file = join(file, 'index.html');
  return existsSync(file) ? file : null;
};

/** Serve `file` as the web entry image serves what it lays out. */
const serve = (response, file) => {
  const body = readFileSync(file);
  response.writeHead(200, {
    'content-type': TYPES[extname(file)] ?? 'application/octet-stream',
    'cache-control': 'no-store',
    'content-length': body.length,
  });
  response.end(body);
};

/** Hand `request` to the server at `serverOrigin` under its own path, and its
 * answer back as it came. That is the plain listener an
 * ingress forwards to once it terminated TLS, so this hop is the deployment's
 * own and no certificate stands on it. */
const toServer = (request, response) => {
  const path = new URL(request.url, 'https://x').pathname;
  const secure = serverOrigin.startsWith('https:');
  const ask = secure ? secureRequest : plainRequest;
  const upstream = ask(`${serverOrigin}${path}`, {
    method: request.method,
    headers: {...request.headers, host: new URL(origin).host},
    ...(secure ? {rejectUnauthorized: false} : {}),
  }, answer => {
    response.writeHead(answer.statusCode ?? 502, answer.headers);
    answer.pipe(response);
  });
  upstream.on('error', error => {
    console.error(`site: the server did not answer: ${error.message}`);
    response.writeHead(502).end();
  });
  request.pipe(upstream);
};

const server = createServer({cert: readFileSync(cert), key: readFileSync(key)}, (request, response) => {
  const path = normalize(decodeURIComponent(new URL(request.url, 'https://x').pathname));
  if (path === '/entry.json') {
    const body = Buffer.from(entry);
    response.writeHead(200, {'content-type': 'application/json', 'cache-control': 'no-cache', 'content-length': body.length});
    response.end(body);
    return;
  }
  const file = fileAt(path);
  if (file) { serve(response, file); return; }
  // Everything else the entry serves from the host behind it: a workspace's
  // app as a web page (`/w/…`, the page the server renders), the host's
  // routes, and a directory listing. The ingress hands what it terminates TLS
  // for to the server (`TILO_WEB_SERVER`), and so does this.
  toServer(request, response);
});
server.listen(Number(port), '127.0.0.1', () => {
  console.log(JSON.stringify({url: `https://127.0.0.1:${server.address().port}`}));
});
