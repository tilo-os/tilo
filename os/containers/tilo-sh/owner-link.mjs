// The page `stack.sh owner` prints: it reads the user key that browser made
// for the Lounge out of the site's localStorage (the own workspace it named on
// this host, a host it joined, or a stand-in key it holds) and hands it to the
// stack, which declares that person an owner in the Lounge's roles file. The
// page is served under the site's own origin (Caddy routes /owner here),
// because only that origin's localStorage holds the key.
//
// usage: node owner-link.mjs STATE_DIR PORT TOKEN
// Prints {"url": "http://127.0.0.1:<port>/owner/<token>"} once it listens,
// then takes one key, keeps it in STATE_DIR/<token>.key (mode 0600) and stops.
import {mkdirSync, writeFileSync} from 'node:fs';
import {createServer} from 'node:http';
import {join} from 'node:path';

const [state, port, token] = process.argv.slice(2);
if (!state || !port || !token) {
  console.error('usage: node owner-link.mjs STATE_DIR PORT TOKEN');
  process.exit(2);
}
mkdirSync(state, {recursive: true});
const path = `/owner/${token}`;
const out = join(state, `${token}.key`);

const page = (body, script = '') => `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Make this browser the Lounge's owner</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 16px/1.5 system-ui, sans-serif; margin: 0; display: grid; place-items: center;
         min-height: 100vh; background: Canvas; color: CanvasText; }
  main { max-width: 34rem; padding: 2rem; }
  h1 { font-size: 1.35rem; margin: 0 0 1rem; }
  p { margin: 0 0 1rem; }
  code { font-size: 0.95em; }
</style>
</head>
<body><main><h1>Make this browser the Lounge's owner</h1>
<p id="state" role="status" aria-live="polite">${body}</p>
${script}
</main></body>
</html>
`;

// The person's user key as this browser made it: the own workspace it named on
// this host carries it, a host it joined names it beside the workspace, and a
// stand-in key it holds for an identity names the identity.
const key_script = `
const KEY = /^[0-9a-f]{64}$/;
const named = 'tilo-own-workspace-named:' + location.origin + ':';
function userKey() {
  try {
    for (let i = 0; i < localStorage.length; i += 1) {
      const name = localStorage.key(i) ?? '';
      if (name.startsWith(named)) {
        const user = name.slice(named.length);
        if (KEY.test(user)) return user;
      }
    }
    const memberships = JSON.parse(localStorage.getItem('tilo-host-memberships') ?? '{}') ?? {};
    for (const kept of Object.values(memberships)) {
      const user = kept?.userPublicKey;
      if (typeof user === 'string' && KEY.test(user)) return user;
    }
    for (let i = 0; i < localStorage.length; i += 1) {
      const name = localStorage.key(i) ?? '';
      if (!name.startsWith('tilo-stand-in:')) continue;
      const user = name.slice('tilo-stand-in:'.length);
      if (KEY.test(user)) return user;
    }
  } catch { /* storage denied: read as no identity */ }
  return null;
}
const state = document.getElementById('state');
const key = userKey();
// The key's short form, as the Lounge's own screens show it, so the person
// can tell which identity this is.
const fingerprint = key ? key.slice(0, 12).toUpperCase().match(/../g).join(' ') : '';
if (!key) {
  state.innerHTML = 'This browser holds no Lounge identity yet: open <a href="/">the Lounge</a> '
    + 'first, then follow this link again.';
} else {
  state.textContent = "Declaring this browser's Lounge identity " + fingerprint
    + " the Lounge's owner…";
  fetch(location.pathname, {method: 'POST', headers: {'content-type': 'application/json'},
    body: JSON.stringify({key})})
    .then(response => response.ok ? response.json() : Promise.reject(new Error(String(response.status))))
    .then(() => {
      state.innerHTML = "This browser is the Lounge's owner now. Open <a href=\"/\">the Lounge</a>";
    })
    .catch(() => {
      state.textContent = 'The stack did not take the key: run stack.sh owner again.';
    });
}
`;

const server = createServer((request, response) => {
  if (request.url !== path) {
    response.writeHead(404, {'content-type': 'text/plain; charset=utf-8'});
    response.end('not this link\n');
    return;
  }
  if (request.method === 'GET') {
    response.writeHead(200, {'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store'});
    response.end(page('Looking for the identity this browser made for the Lounge…', `<script>${key_script}</script>`));
    return;
  }
  // Only the page itself posts, as JSON: another origin's form posts a body
  // this does not read.
  if (request.method !== 'POST' || request.headers['content-type'] !== 'application/json') {
    response.writeHead(404, {'content-type': 'text/plain; charset=utf-8'});
    response.end('not this link\n');
    return;
  }
  const chunks = [];
  request.on('data', chunk => chunks.push(chunk));
  request.on('end', () => {
    let key = '';
    try {
      key = String(JSON.parse(Buffer.concat(chunks).toString('utf8')).key ?? '');
    } catch { /* not JSON: refused below */ }
    if (!/^[0-9a-f]{64}$/.test(key)) {
      response.writeHead(400, {'content-type': 'application/json'});
      response.end(JSON.stringify({error: 'not a user key'}));
      return;
    }
    response.writeHead(200, {'content-type': 'application/json', 'cache-control': 'no-store'});
    // Kept once the answer is out, then the server stops: the stack reads the
    // key and declares it, and a link used is a link gone.
    response.end(JSON.stringify({declared: true}), () => {
      writeFileSync(out, `${key}\n`, {mode: 0o600});
      server.close();
      setTimeout(() => process.exit(0), 200);
    });
  });
});
server.listen(Number(port), '127.0.0.1', () => {
  const bound = server.address().port;
  console.log(JSON.stringify({url: `http://127.0.0.1:${bound}${path}`}));
});
