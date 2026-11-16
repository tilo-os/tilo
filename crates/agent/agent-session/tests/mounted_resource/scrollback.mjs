/** A late joiner reads back through a chat's earlier parts in the real
 * compiled Chat: the earlier part the host has arrives as the reader
 * scrolls, and the one it never had ends in "not available".
 * stdin is ephemeral fixture material, never a live identity or account. */
import assert from 'node:assert/strict';
import {createServer} from 'node:https';
import {readFileSync, existsSync, mkdirSync, writeFileSync} from 'node:fs';
import {resolve, join, extname} from 'node:path';
import {createInterface} from 'node:readline';
import {launch, pinnedCertificates, servedCertificate, sleep} from '../../../../../tools/web/chrome.mjs';
import {mountChat} from './mount-chat.mjs';

const UNAVAILABLE = "Earlier messages aren't available on this device's hosts";
const ARRIVING = 'Earlier messages are on their way';
/** Whether the page's semantics (as JSON) show the message saying `text`:
 * a message reads "<author>: <text>". */
const holds = (semantics, text) => semantics.includes(`: ${text}"`);

const bundle = resolve(process.argv[2]);
assert.ok(existsSync(join(bundle, 'app/index.html')), 'built full-tab entry page is required');
const input = createInterface({input: process.stdin});
const lines = input[Symbol.asyncIterator]();
const credentials = JSON.parse((await lines.next()).value);
const types = {'.html': 'text/html', '.mjs': 'text/javascript', '.js': 'text/javascript',
  '.css': 'text/css', '.wasm': 'application/wasm', '.json': 'application/json'};
const server = createServer(credentials, (request, response) => {
  const path = new URL(request.url, 'https://localhost').pathname;
  if (path === '/fixture.html') {
    response.writeHead(200, {'content-type': 'text/html'}).end('<!doctype html><style>html,body{margin:0;height:100%}#surface{position:fixed;inset:0}</style><div id="surface"></div>');
    return;
  }
  const file = resolve(bundle, '.' + path + (path.endsWith('/') ? 'index.html' : ''));
  if (!file.startsWith(bundle + '/') || !existsSync(file)) { response.writeHead(404).end(); return; }
  response.writeHead(200, {'content-type': types[extname(file)] ?? 'application/octet-stream'}).end(readFileSync(file));
});
await new Promise(done => server.listen(0, '127.0.0.1', done));
const origin = `https://127.0.0.1:${server.address().port}`;
process.stdout.write(JSON.stringify({origin}) + '\n');
const fixture = JSON.parse((await lines.next()).value);
process.env.CHROME ??= '/usr/bin/chromium-browser';
const evidence = resolve('target/mounted-resource-scrollback');
mkdirSync(evidence, {recursive: true});
const stage = (name, detail = {}) => process.stdout.write(JSON.stringify({stage: name, ...detail}) + '\n');
let browser;
let tab;
let failed = false;
const deadline = setTimeout(async () => {
  console.error('Scrollback journey exceeded its 400-second bound');
  await browser?.close();
  server.close();
  process.exit(1);
}, 400_000);
try {
  // The page's own fixture's certificate and the host's development one: what
  // its two origins present, and the only certificates it accepts.
  browser = await launch({args: [pinnedCertificates([{certificate: credentials.cert, host: '127.0.0.1'},
      await servedCertificate(fixture.origin)])]});
  tab = await browser.tab();
  await tab.navigate(origin + '/fixture.html');
  await tab.waitFor('document.readyState === "complete"');
  await tab.front();
  const supplied = await tab.send('Runtime.evaluate', {expression: `window.fixture = ${JSON.stringify(fixture)}; void 0`});
  assert.equal(supplied.result?.exceptionDetails, undefined, 'fixture bootstrap could not be supplied');
  const mounted = await mountChat(tab);
  assert.equal(mounted.error, undefined, JSON.stringify(mounted));
  await tab.waitFor('document.querySelector("canvas")?.width > 0');
  const semantics = async () => JSON.stringify(await tab.evaluate('(mount.requestSemantics(), mount.state().semantics)'));
  const SEMANTICS = 'JSON.stringify(mount.state().semantics)';
  const last = `2.${fixture.perPart - 1}`;
  // When each part's messages first show, and whether an earlier part was
  // ever said to be on its way meanwhile: the page tells every change it
  // shows (`globalThis.tiloChanges`), so it notes each one as it arrives,
  // where the driver looking on a clock could miss a state the page drew and
  // replaced between two looks.
  await tab.evaluate(`(() => {
    globalThis.__seen = {};
    globalThis.__seenAt = performance.now();
    const note = () => {
      const text = ${SEMANTICS};
      for (const [name, needle] of [['head', ': 2.0"'], ['part1', ': 1.0"'],
        ['arriving', ${JSON.stringify(ARRIVING)}], ['unavailable', ${JSON.stringify(UNAVAILABLE)}]]) {
        if (globalThis.__seen[name] === undefined && text.includes(needle)) {
          globalThis.__seen[name] = Math.round(performance.now() - globalThis.__seenAt);
        }
      }
    };
    globalThis.__seenWatch ??= globalThis.tiloChanges.listen(note);
    note();
    return true;
  })()`);
  await tab.waitFor(`(${SEMANTICS}).includes(${JSON.stringify(`: ${last}"`)})`, {timeout: 60_000})
    .catch(async () => { throw Error(`the head never showed in 60 s: ${await semantics()}`); });
  const seen = await tab.evaluate('JSON.stringify(globalThis.__seen)').then(JSON.parse);
  let shown;
  stage('first-seen', seen);
  // Nothing earlier is asked for before the reader scrolls, however long
  // the page settles: no part 1, and nothing said to be on its way.
  await sleep(3_000);
  shown = await semantics();
  assert.equal(seen.part1, undefined, 'part 1 arrived before any scroll');
  assert.equal(seen.arriving, undefined, 'an earlier part was asked for before any scroll');
  assert.ok(!holds(shown, '1.0') && !shown.includes(ARRIVING), 'nothing earlier asked for while the page settled');
  stage('head-shown-alone');
  writeFileSync(join(evidence, 'head.png'), await tab.screenshot());

  // Over the timeline: where the page says a message of the head is drawn.
  const box = await tab.evaluate(`(() => {
    const found = [];
    const walk = node => {
      if (!node || typeof node !== 'object') return;
      const name = typeof node.name === 'string' ? node.name : typeof node.label === 'string' ? node.label : '';
      const b = node.bounds ?? node.rect;
      if (/: 2[.][0-9]+$/.test(name) && b) found.push(b);
      for (const value of Object.values(node)) walk(value);
    };
    walk(mount.state().semantics);
    const r = document.querySelector('canvas').getBoundingClientRect();
    const b = found.at(-1);
    if (!b) return {x: r.left + r.width * 0.45, y: r.top + r.height * 0.5, from: 'canvas'};
    const x0 = b.x0 ?? b.x ?? b.left, y0 = b.y0 ?? b.y ?? b.top;
    const x1 = b.x1 ?? (x0 + (b.width ?? b.w)), y1 = b.y1 ?? (y0 + (b.height ?? b.h));
    return {x: (x0 + x1) / 2, y: (y0 + y1) / 2, from: JSON.stringify(b)};
  })()`);
  stage('wheel-at', box);
  const wheelUp = () => tab.send('Input.dispatchMouseEvent', {type: 'mouseWheel', x: box.x, y: box.y, deltaX: 0, deltaY: -3000});
  // Scroll back until the page shows `needle`, noting whether it ever said
  // the part was on its way (the recorder above saw it, not this wait).
  const first = text => [...text.matchAll(/: (\d\.\d+)"/g)].slice(0, 2).map(m => m[1]);
  const sawArriving = async () => (await tab.evaluate('globalThis.__seen?.arriving !== undefined')) === true;
  const scrollUntil = async (what, needle, limit) => {
    const until = Date.now() + limit;
    let reported = Date.now();
    for (;;) {
      // The page's own word for what it shows, waited for as it changes, and
      // progress said every ten seconds while it still does not.
      const found = await tab.waitFor(`(${SEMANTICS}).includes(${JSON.stringify(needle)})`,
        {timeout: Math.max(1, Math.min(until - Date.now(), 10_000))}).catch(() => false);
      shown = await semantics();
      if (found) return;
      if (Date.now() - reported > 10_000) {
        reported = Date.now();
        stage('scrolling', {what, first: first(shown), arriving: shown.includes(ARRIVING),
          unavailable: shown.includes(UNAVAILABLE)});
      }
      if (Date.now() > until) {
        writeFileSync(join(evidence, 'semantics.json'), shown);
        throw Error(`timed out scrolling back until ${what}`);
      }
      // The page takes a notch as a line, however far it says to go.
      for (let notch = 0; notch < 50; notch++) await wheelUp();
    }
  };
  await scrollUntil('part 1 arrived', ': 1.0"', 150_000);
  assert.ok(!shown.includes(UNAVAILABLE), 'the part the host has is not unavailable');
  stage('earlier-part-arrived', {sawArriving: await sawArriving()});
  writeFileSync(join(evidence, 'part-1.png'), await tab.screenshot());

  await scrollUntil('the missing part said so', UNAVAILABLE, 150_000);
  assert.ok(!holds(shown, '0.0'), 'nothing of the part the host never had shows');
  assert.ok(!shown.includes(ARRIVING), 'nothing is loading once the hosts said they have none');
  stage('missing-part-unavailable', {sawArriving: await sawArriving()});
  writeFileSync(join(evidence, 'unavailable.png'), await tab.screenshot());
  // It stays said: no retry, no spinner coming back.
  await sleep(2_000);
  shown = await semantics();
  assert.ok(shown.includes(UNAVAILABLE) && !shown.includes(ARRIVING), 'the final state holds');
  stage('final-state-holds');
} catch (error) {
  failed = true;
  console.error(error?.stack ?? String(error));
  if (tab) console.error('page log', tab.log.slice(-40).join('\n'));
  const said = await tab?.evaluate('JSON.stringify(mount.state().semantics)').catch(() => null);
  if (said) {
    writeFileSync(join(evidence, 'failure-semantics.json'), said);
    console.error('messages shown', [...said.matchAll(/: (\d\.\d+)"/g)].map(m => m[1]).join(' ').slice(0, 2000));
  }
} finally {
  clearTimeout(deadline);
  await browser?.close();
  await new Promise(done => server.close(done));
}
process.exit(failed ? 1 : 0);
