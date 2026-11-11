// The site's host for the Lounge integration test: the web entry's bundle,
// served as the web entry image serves it, with the entry configuration this
// deployment writes — its home is the Lounge, joined through the door the
// relay lists it at, and its directory is the relay's. Under `/i/` it is the
// site a sealed invitation's link opens: the page for a browser, and the
// relay's shelf for the page's own request for the invitation (the web entry
// image's `TILO_WEB_SHELF`).
//
// usage: node site.mjs SITE_DIR CERT KEY WORKSPACE_ORIGIN NAME ADMISSION PORT HOST_INFO
// HOST_INFO is the line `tilo-relay page-host` prints for the workspace's
// host, which the web entry image hands its page from TILO_WEB_HOME_HOST_INFO
// so the page dials that host as named; an empty HOST_INFO is only for a site
// whose host is not there to ask.
// Prints {"url": "https://127.0.0.1:<port>"} once it listens.
import {readFileSync, statSync, existsSync} from 'node:fs';
import {createServer, request as httpsRequest} from 'node:https';
import {extname, join, normalize} from 'node:path';

const [site, cert, key, origin, name, admission, port, info] = process.argv.slice(2);
if (!site || !cert || !key || !origin || !name || !admission || !port || info === undefined) {
  console.error('usage: node site.mjs SITE_DIR CERT KEY WORKSPACE_ORIGIN NAME ADMISSION PORT HOST_INFO');
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
// TILO_WEB_HOME_HOST, TILO_WEB_HOME_HOST_INFO and TILO_WEB_HOME_DOOR, for a
// site whose home workspace is hosted at `origin`.
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

/** Hand `request` to the relay at `origin` under the path it serves the
 * shelf at, and its answer back as it came. */
const shelf = (request, response) => {
  const path = new URL(request.url, 'https://x').pathname;
  const upstream = httpsRequest(`${origin}${path}`, {
    method: request.method,
    rejectUnauthorized: false,
    headers: {...request.headers, host: new URL(origin).host},
  }, answer => {
    response.writeHead(answer.statusCode ?? 502, answer.headers);
    answer.pipe(response);
  });
  upstream.on('error', error => {
    console.error(`site: the shelf did not answer: ${error.message}`);
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
  // A sealed invitation's link: the page for a browser that accepts HTML,
  // with the files it names relative to itself, and the relay answering under
  // the handle for everyone else (the page fetching an invitation, and the
  // inviter leaving one there).
  if (path.startsWith('/i/')) {
    const file = fileAt(path.slice('/i'.length));
    if (file) { serve(response, file); return; }
    const wantsPage = (request.headers.accept ?? '').includes('text/html');
    if (wantsPage && /^[A-Za-z0-9_-]{16}$/.test(path.slice('/i/'.length))) {
      serve(response, join(site, 'index.html'));
      return;
    }
    shelf(request, response);
    return;
  }
  const file = fileAt(path);
  if (!file) { response.writeHead(404).end(); return; }
  serve(response, file);
});
server.listen(Number(port), '127.0.0.1', () => {
  console.log(JSON.stringify({url: `https://127.0.0.1:${server.address().port}`}));
});
