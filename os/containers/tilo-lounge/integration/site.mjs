// The site's host for the Lounge integration test: the web entry's bundle,
// served as the web entry image serves it, with the entry configuration this
// deployment writes — its home is the Lounge, joined through the door the
// relay lists it at, and its directory is the relay's.
//
// usage: node site.mjs SITE_DIR CERT KEY WORKSPACE_ORIGIN NAME ADMISSION PORT
// Prints {"url": "https://127.0.0.1:<port>"} once it listens.
import {readFileSync, statSync, existsSync} from 'node:fs';
import {createServer} from 'node:https';
import {extname, join, normalize} from 'node:path';

const [site, cert, key, origin, name, admission, port] = process.argv.slice(2);
if (!site || !cert || !key || !origin || !name || !admission || !port) {
  console.error('usage: node site.mjs SITE_DIR CERT KEY WORKSPACE_ORIGIN NAME ADMISSION PORT');
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
// TILO_WEB_HOME_HOST and TILO_WEB_HOME_DOOR, for a site whose home workspace
// is hosted at `origin`.
const entry = JSON.stringify({
  home: {name, host: {origin}, admission},
  directory: `${origin}/peer/v1/directory`,
});

const server = createServer({cert: readFileSync(cert), key: readFileSync(key)}, (request, response) => {
  const path = normalize(decodeURIComponent(new URL(request.url, 'https://x').pathname));
  if (path === '/entry.json') {
    const body = Buffer.from(entry);
    response.writeHead(200, {'content-type': 'application/json', 'cache-control': 'no-cache', 'content-length': body.length});
    response.end(body);
    return;
  }
  let file = join(site, path);
  if (!file.startsWith(site)) { response.writeHead(403).end(); return; }
  if (existsSync(file) && statSync(file).isDirectory()) file = join(file, 'index.html');
  if (!existsSync(file)) { response.writeHead(404).end(); return; }
  const body = readFileSync(file);
  response.writeHead(200, {
    'content-type': TYPES[extname(file)] ?? 'application/octet-stream',
    'cache-control': 'no-store',
    'content-length': body.length,
  });
  response.end(body);
});
server.listen(Number(port), '127.0.0.1', () => {
  console.log(JSON.stringify({url: `https://127.0.0.1:${server.address().port}`}));
});
