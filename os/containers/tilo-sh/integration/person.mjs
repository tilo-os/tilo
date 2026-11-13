// A person in their own headless Chromium on the local tilo.sh, as a
// visitor arrives there: a fresh profile opens the site and lands in the
// Lounge as a new person. Then, as asked:
//
//   --follow LINK   they follow an invitation link and press Join on its card;
//   --key NAME      they make an API key: `tilo keys create NAME` runs as a
//                   backend's terminal runs it, and the person approves the
//                   link it prints on the site's own /sign-in page, in this
//                   same browser, by pressing Allow;
//   --no-pages      the key they make on the door's grants page does not
//                   operate web pages in Browser: the box is unticked;
//   --root FILE     they are a person another program can hold: the page
//                   makes a recovery key for them (its own "save a recovery
//                   key") and FILE gets what opens them elsewhere, the
//                   envelope and that key, as `tilo-approve person` reads it;
//   --window NAME --type TEXT
//                   after following the invitation, a window of the
//                   workspace another device hosts (a Linux app in the
//                   workspace's VM) appears on their desk, as a tile whose
//                   accessible name matches NAME (a case-blind pattern); they
//                   click it and type TEXT, then Enter, key by key as a
//                   keyboard sends it. report.window says when it appeared,
//                   how long each key took to change the window's picture
//                   (keyToFrameMs), and a picture of it is kept as
//                   window.png;
//   --page NAME --changes N
//                   after following the invitation, the desk tile whose
//                   accessible name matches NAME (the hosted Browser window)
//                   is the page of a window another device draws, and they
//                   watch it: a line `{"desk": …}` says when the page is drawn
//                   there and how much of the tile is content rather than
//                   background, and every N pictures that differ from the one
//                   before are kept as page-<n>.png and said in a line of their
//                   own, with the milliseconds since the last one. That is how
//                   a check holds the person to seeing the agent's work on
//                   their desk while it happens;
//   --passkey       they keep themselves with a passkey (a virtual
//                   authenticator, as a platform one answers), the browser
//                   then forgets the site, and they sign in again with that
//                   passkey: the same person comes back.
//
// usage: node person.mjs SITE RELAY OUT_DIR [--follow LINK] [--key NAME --tilo BIN --api URL [--no-pages]] [--root FILE] [--passkey]
//
// Prints one JSON line: {arrivedMs, joinedMs, room, followed, key, entered}.
// `arrivedMs` is a fresh visitor's entry: from navigating to the site to the
// page done arriving and a member of the Lounge, which the entry-speed work
// measures. Saves the page's console and a screenshot in OUT_DIR, and exits
// non-zero, saying what never happened, when a step did not. With `--key` it
// also keeps what the person reads there, in both appearances: the sign-in
// card they allow, the door's grants page, and the key that page makes.
import {spawn} from 'node:child_process';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {inflateSync} from 'node:zlib';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const [site, relay, out, ...rest] = process.argv.slice(2);
const option = name => {
  const at = rest.indexOf(name);
  return at < 0 ? null : rest[at + 1];
};
if (!site || !relay || !out) {
  console.error('usage: node person.mjs SITE RELAY OUT_DIR [--follow LINK] [--key NAME --tilo BIN --api URL [--no-pages]] [--root FILE]');
  process.exit(2);
}
const follow = option('--follow');
const keyName = option('--key');
const rootFile = option('--root');
const noPages = rest.includes('--no-pages');
const passkey = rest.includes('--passkey');
const tiloBin = option('--tilo');
const door = option('--api');
const windowName = option('--window');
const typed = option('--type');
const pageName = option('--page');
const changes = Number(option('--changes') ?? 1);
mkdirSync(out, {recursive: true});

const report = {site, relay};
// The stack is plain HTTP on this machine: no certificate to accept, as for
// any person's browser. A software GPU, and no sandbox where this runs as root.
const browser = await launch({args: [
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
]});
const tab = await browser.tab();
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false})
  .catch(() => {});

const text = () => tab.evaluate(`(document.body?.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 600)`)
  .catch(() => '');
const finish = async (code, message) => {
  if (message) report.error = message;
  writeFileSync(join(out, 'console.log'), tab.log.join('\n'));
  const shot = await tab.screenshot().catch(() => null);
  if (shot) writeFileSync(join(out, 'page.png'), shot);
  await browser.close().catch(() => {});
  if (message) {
    const said = tab.log.slice(-30);
    if (said.length) console.error(`--- the page said:\n${said.join('\n')}`);
    console.error(`FAIL: ${message}`);
  }
  console.log(JSON.stringify(report));
  process.exit(code);
};
const until = async (expression, what, timeout = 180_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await tab.evaluate(expression).catch(() => false);
    if (value) return value;
    if (Date.now() > deadline) await finish(1, `${what} never happened: ${await text()}`);
    await sleep(100);
  }
};
// The page as a person would look at it, kept in OUT_DIR as `name-light.png`
// and `name-dark.png`: what each surface the check visits says, in the two
// appearances it has.
const capture = async name => {
  for (const theme of ['light', 'dark']) {
    await tab.send('Emulation.setEmulatedMedia', {
      media: 'screen',
      features: [{name: 'prefers-color-scheme', value: theme}],
    }).catch(() => {});
    await sleep(150);
    const shot = await tab.screenshot().catch(() => null);
    if (shot) writeFileSync(join(out, `${name}-${theme}.png`), shot);
  }
  await tab.send('Emulation.setEmulatedMedia', {features: []}).catch(() => {});
};
// The page's identity module, the very one the page holds its person in: the
// page's modules live under the build's own version directory (/v/<hash>/),
// which its preload hints name.
const identityModule = `await import(new URL('identity.mjs', new URL('.', (document.querySelector('link[rel="modulepreload"][href], script[type="module"][src]').href
  || document.querySelector('script[type="module"][src]').src))).href)`;
const memberships = `JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')`;
const entered = `JSON.parse(globalThis.localStorage.getItem('tilo-entered-workspaces') ?? '[]')`;

// A fresh visitor's entry.
const began = Date.now();
await tab.navigate(site);
await until(`['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, 'the page arriving');
if (await tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
  await finish(1, `the page stopped arriving: ${await text()}`);
}
report.arrivedMs = Date.now() - began;
const membership = JSON.parse(await until(`(() => {
  const kept = ${memberships}[${JSON.stringify(new URL(relay).origin)}];
  return kept?.room ? JSON.stringify(kept) : '';
})()`, 'joining the Lounge'));
report.joinedMs = Date.now() - began;
report.room = membership.room;
console.error(`ok: a fresh visitor arrived in ${report.arrivedMs} ms and was in the Lounge (${report.room}) at ${report.joinedMs} ms`);

if (rootFile) {
  // The person this page is, opened elsewhere: the page's own identity module
  // makes a recovery key for them and keeps the envelope that goes with it.
  const held = await tab.evaluate(`(async () => {
    // The page's modules live under the build's own version directory
    // (/v/<hash>/), which its preload hints name: the identity module is one
    // of them, and importing the same address gives the module the page
    // itself holds its person in.
    const hint = document.querySelector('link[rel="modulepreload"][href], script[type="module"][src]');
    const base = new URL('.', hint.href || hint.src);
    const {browserIdentity} = await import(new URL('identity.mjs', base).href);
    const person = await browserIdentity();
    const recoveryKey = await person.createRecoveryKey();
    return JSON.stringify({person: person.userPublicKey, recovery_key: recoveryKey, envelope: person.envelope});
  })()`);
  const kept = JSON.parse(held);
  if (!kept.recovery_key || !kept.envelope) await finish(1, `the page made no way to hold this person: ${held}`);
  // The person held must be the person the page joined the Lounge as.
  if (membership.userPublicKey && membership.userPublicKey !== kept.person) {
    await finish(1, `the page holds ${kept.person}, but joined as ${membership.userPublicKey}`);
  }
  writeFileSync(rootFile, JSON.stringify(kept, null, 2), {mode: 0o600});
  report.person = kept.person;
  report.root = rootFile;
  console.error(`ok: the person ${kept.person} can be held from ${rootFile}`);
}

if (passkey) {
  // A platform authenticator that answers with PRF, which is what lets a
  // passkey open a Tilo person; resident, so signing in needs no name.
  await tab.send('WebAuthn.enable', {enableUI: false});
  const added = await tab.send('WebAuthn.addVirtualAuthenticator', {options: {protocol: 'ctap2', transport: 'internal',
    hasResidentKey: true, hasUserVerification: true, isUserVerified: true, hasPrf: true}});
  if (added.error) await finish(1, `no virtual authenticator: ${added.error.message}`);
  // Kept with a passkey for this site's name, then kept at the Lounge's host,
  // as the page's "Keep me" does.
  const kept = await tab.evaluate(`(async () => {
    const id = ${identityModule};
    const person = await id.browserIdentity();
    try {
      await person.keepWithPasskey({userName: 'local passkey check'});
      await id.publishEnvelope({origin: ${JSON.stringify(new URL(relay).origin)}, identity: person});
    } catch (error) { return JSON.stringify({error: String(error?.message ?? error), code: error?.code ?? error?.name}); }
    return JSON.stringify({person: person.userPublicKey, rpIds: person.envelope.wraps.filter(wrap => wrap.kind === 'passkey').map(wrap => wrap.rp_id)});
  })()`).then(JSON.parse);
  if (kept.error) await finish(1, `the passkey was not registered on ${new URL(site).hostname}: ${kept.error} (${kept.code})`);
  console.error(`ok: ${kept.person} kept themselves with a passkey for ${kept.rpIds.join(', ')}`);
  // The browser forgets the site: a fresh visitor, the authenticator alone
  // remembering them.
  await tab.send('Storage.clearDataForOrigin', {origin: new URL(site).origin, storageTypes: 'all'});
  await tab.navigate(site);
  await until(`document.documentElement.dataset.arrival === 'done'`, 'the page arriving again');
  const back = await tab.evaluate(`(async () => {
    const id = ${identityModule};
    const before = id.currentIdentity()?.userPublicKey ?? null;
    try {
      const person = await id.signInWithPasskey({fetchEnvelope: root => id.fetchEnvelope(root, [${JSON.stringify(new URL(relay).origin)}])});
      return JSON.stringify({before, person: person.userPublicKey});
    } catch (error) { return JSON.stringify({error: String(error?.message ?? error), code: error?.code ?? error?.name}); }
  })()`).then(JSON.parse);
  if (back.error) await finish(1, `signing in with the passkey failed: ${back.error} (${back.code})`);
  if (back.before === kept.person) await finish(1, 'the browser still held the person after it forgot the site');
  if (back.person !== kept.person) await finish(1, `the passkey signed in ${back.person}, not ${kept.person}`);
  report.passkey = {person: kept.person, rpIds: kept.rpIds};
  console.error(`ok: after the browser forgot the site, the passkey signed ${back.person} in again`);
}

if (follow) {
  const before = JSON.stringify(await tab.evaluate(`JSON.stringify(${entered})`));
  await tab.navigate(follow);
  // The invitation's card names the workspace and the role; they press Join.
  await until(`(() => {
    const join = [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
    if (!join || join.disabled) return false;
    join.click();
    return true;
  })()`, 'the invitation offering to join');
  // Joined: the page entered a workspace it had not, other than the Lounge.
  report.followed = await until(`(() => {
    const now = ${entered};
    const found = (Array.isArray(now) ? now : Object.values(now)).find(entry =>
      (entry?.room ?? entry?.scope) && (entry.room ?? entry.scope) !== ${JSON.stringify(report.room)});
    return found ? JSON.stringify(found) : '';
  })()`, `joining the workspace the invitation names (entered before: ${before})`).then(JSON.parse);
  console.error(`ok: the invitation landed them in ${JSON.stringify(report.followed)}`);
}

// The keys a keyboard sends for each character typed: its DOM code, and
// whether Shift is held. What the page hands the hosted window is the key, not
// the text, so each character goes as the key that makes it.
const keyOf = char => {
  if (/^[a-z]$/.test(char)) return {code: `Key${char.toUpperCase()}`, key: char};
  if (/^[A-Z]$/.test(char)) return {code: `Key${char}`, key: char, shift: true};
  if (/^[0-9]$/.test(char)) return {code: `Digit${char}`, key: char};
  const named = {
    ' ': {code: 'Space', key: ' '}, '-': {code: 'Minus', key: '-'}, '.': {code: 'Period', key: '.'},
    '/': {code: 'Slash', key: '/'}, '>': {code: 'Period', key: '>', shift: true},
    '_': {code: 'Minus', key: '_', shift: true}, '=': {code: 'Equal', key: '='},
    '+': {code: 'Equal', key: '+', shift: true}, '$': {code: 'Digit4', key: '$', shift: true},
    '%': {code: 'Digit5', key: '%', shift: true}, '(': {code: 'Digit9', key: '(', shift: true},
    ')': {code: 'Digit0', key: ')', shift: true},
  };
  return named[char] ?? null;
};
const press = async ({code, key, shift = false}, text = key) => {
  const modifiers = shift ? 8 : 0;
  if (shift) await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', code: 'ShiftLeft', key: 'Shift', windowsVirtualKeyCode: 16});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', code, key, text, modifiers});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', code, key, modifiers});
  if (shift) await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', code: 'ShiftLeft', key: 'Shift', windowsVirtualKeyCode: 16});
};

// A picture of a window's box, as the PNG the browser writes it: what the
// person looking at their desk sees of it. The pixels are read back so that
// two pictures can be compared by what changed in them, not by whether the
// file's bytes differ — a caret blinking is not the page changing.
const pixelsOf = png => {
  if (png.length < 24 || png.readUInt32BE(0) !== 0x89504e47) throw new Error('not a PNG');
  let at = 8;
  let width = 0;
  let height = 0;
  let depth = 0;
  let colour = 0;
  let interlace = 0;
  const parts = [];
  while (at + 8 <= png.length) {
    const length = png.readUInt32BE(at);
    const kind = png.toString('ascii', at + 4, at + 8);
    const body = png.subarray(at + 8, at + 8 + length);
    if (kind === 'IHDR') {
      width = body.readUInt32BE(0);
      height = body.readUInt32BE(4);
      depth = body[8];
      colour = body[9];
      interlace = body[12];
    } else if (kind === 'IDAT') {
      parts.push(body);
    } else if (kind === 'IEND') {
      break;
    }
    at += 12 + length;
  }
  if (depth !== 8 || interlace !== 0 || (colour !== 2 && colour !== 6)) {
    throw new Error(`a PNG this does not read: depth ${depth}, colour ${colour}, interlace ${interlace}`);
  }
  const channels = colour === 6 ? 4 : 3;
  const raw = inflateSync(Buffer.concat(parts));
  const stride = width * channels;
  const data = Buffer.alloc(width * height * 4, 255);
  let previous = Buffer.alloc(stride);
  for (let row = 0; row < height; row++) {
    const filter = raw[row * (stride + 1)];
    const current = Buffer.from(raw.subarray(row * (stride + 1) + 1, (row + 1) * (stride + 1)));
    for (let i = 0; i < stride; i++) {
      const left = i >= channels ? current[i - channels] : 0;
      const up = previous[i];
      const upLeft = i >= channels ? previous[i - channels] : 0;
      let predictor = 0;
      if (filter === 1) predictor = left;
      else if (filter === 2) predictor = up;
      else if (filter === 3) predictor = (left + up) >> 1;
      else if (filter === 4) {
        const p = left + up - upLeft;
        const pa = Math.abs(p - left);
        const pb = Math.abs(p - up);
        const pc = Math.abs(p - upLeft);
        predictor = pa <= pb && pa <= pc ? left : (pb <= pc ? up : upLeft);
      }
      current[i] = (current[i] + predictor) & 0xff;
    }
    for (let x = 0; x < width; x++) {
      const from = x * channels;
      const to = (row * width + x) * 4;
      data[to] = current[from];
      data[to + 1] = current[from + 1];
      data[to + 2] = current[from + 2];
      if (channels === 4) data[to + 3] = current[from + 3];
    }
    previous = current;
  }
  return {width, height, data};
};
// How many pixels two pictures of the same box differ in, by more than a
// colour change this compression and scaling could make on its own.
const differingPixels = (before, after) => {
  if (before.width !== after.width || before.height !== after.height) return before.width * before.height;
  let count = 0;
  for (let i = 0; i < before.data.length; i += 4) {
    if (Math.abs(before.data[i] - after.data[i]) > 16
      || Math.abs(before.data[i + 1] - after.data[i + 1]) > 16
      || Math.abs(before.data[i + 2] - after.data[i + 2]) > 16) count++;
  }
  return count;
};
// How much of a picture is what is drawn in it rather than the box's
// background: a page with a form on it is not a tile waiting for one.
const contentShare = picture => {
  const counts = new Map();
  for (let i = 0; i < picture.data.length; i += 4 * 7) {
    const colour = (picture.data[i] << 16) | (picture.data[i + 1] << 8) | picture.data[i + 2];
    counts.set(colour, (counts.get(colour) ?? 0) + 1);
  }
  const background = [...counts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0];
  let drawn = 0;
  let seen = 0;
  for (let i = 0; i < picture.data.length; i += 4) {
    seen++;
    const colour = (picture.data[i] << 16) | (picture.data[i + 1] << 8) | picture.data[i + 2];
    if (colour !== background) drawn++;
  }
  return seen ? drawn / seen : 0;
};

if (windowName || pageName) {
  if (!follow) await finish(2, '--window and --page need --follow');
  const name = windowName ?? pageName;
  const pattern = new RegExp(name, 'i');
  const waiting = Date.now();
  // The page's semantic layer names every tile: the hosted window's is found
  // by its name, as a screen reader finds it.
  await tab.send('Accessibility.enable', {}).catch(() => {});
  let found = null;
  while (!found && Date.now() - waiting < 180_000) {
    const {result} = await tab.send('Accessibility.getFullAXTree', {}).catch(() => ({result: {nodes: []}}));
    found = (result?.nodes ?? []).find(node => !node.ignored && node.backendDOMNodeId
      && pattern.test(String(node.name?.value ?? '')));
    if (!found) await sleep(250);
  }
  if (!found) await finish(1, `no window named like /${name}/i appeared on the desk in 180 s (it reads: ${await text()})`);
  report.window = {name: found.name?.value, role: found.role?.value, appearedMs: Date.now() - waiting};
  console.error(`ok: the window ${JSON.stringify(report.window.name)} is on the desk after ${report.window.appearedMs} ms`);
  const {result: box} = await tab.send('DOM.getBoxModel', {backendNodeId: found.backendDOMNodeId});
  const quad = box?.model?.content ?? [];
  if (quad.length < 8) await finish(1, `the window ${JSON.stringify(report.window.name)} has no box on the page to click`);
  const x = (quad[0] + quad[4]) / 2;
  const y = (quad[1] + quad[5]) / 2;
  for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
  }
  report.window.clip = {x: quad[0], y: quad[1], width: quad[4] - quad[0], height: quad[5] - quad[1]};
}

if (windowName) {
  if (!follow || typed === null) await finish(2, '--window needs --follow and --type');
  const clip = {...report.window.clip, scale: 1};
  // A picture arrives once the window's stream is decoded: what the person
  // sees before they type.
  await sleep(1500);
  const before = await tab.screenshot().catch(() => null);
  if (before) writeFileSync(join(out, 'window-before.png'), before);
  const picture = async () => (await tab.send('Page.captureScreenshot', {format: 'png', clip})
    .catch(() => ({data: ''}))).data;
  const answered = [];
  const typing = Date.now();
  for (const char of typed) {
    const key = keyOf(char);
    if (!key) await finish(2, `--type has ${JSON.stringify(char)}, which no key here types`);
    const seen = await picture();
    const pressed = performance.now();
    await press(key);
    for (;;) {
      const elapsed = performance.now() - pressed;
      if (elapsed > 3000) { answered.push(null); break; }
      if (await picture() !== seen) { answered.push(Math.round(elapsed)); break; }
    }
  }
  await press({code: 'Enter', key: 'Enter'}, '\r');
  report.window.typedAt = typing;
  report.window.typedMs = Date.now() - typing;
  const timed = answered.filter(ms => ms !== null).sort((a, b) => a - b);
  const at = share => timed[Math.min(timed.length - 1, Math.floor(share * timed.length))];
  report.window.keyToFrameMs = {
    keys: answered.length,
    unanswered: answered.length - timed.length,
    median: timed.length ? at(0.5) : null,
    p90: timed.length ? at(0.9) : null,
    each: answered,
  };
  console.error(`ok: a key reaches the window's next picture in ${report.window.keyToFrameMs.median} ms (median), ${report.window.keyToFrameMs.p90} ms (90th percentile) over ${timed.length} of ${answered.length} keys`);
  await sleep(1500);
  const after = await tab.screenshot().catch(() => null);
  if (after) writeFileSync(join(out, 'window.png'), after);
}

if (pageName) {
  if (!follow) await finish(2, '--page needs --follow');
  const clip = {...report.window.clip, scale: 1};
  const shot = async () => (await tab.send('Page.captureScreenshot', {format: 'png', clip})
    .catch(() => ({data: ''}))).data;
  const look = async () => pixelsOf(Buffer.from(await shot(), 'base64'));
  // A picture arrives once the window's page is drawn there: the person
  // waits for one the publisher sent, as an arriving card is no page.
  let drawn = null;
  const lookFrom = Date.now();
  while (!drawn && Date.now() - lookFrom < 120_000) {
    const picture = await look().catch(() => null);
    if (picture?.width && contentShare(picture) >= 0.02) drawn = picture;
    else await sleep(250);
  }
  if (!drawn) await finish(1, `the page on the desk is still the arriving card after 120 s (it reads: ${await text()})`);
  writeFileSync(join(out, 'page-desk.png'), await shot());
  const said = {name: report.window.name, appearedMs: report.window.appearedMs,
    drawnMs: Date.now() - lookFrom, content: Number(contentShare(drawn).toFixed(4))};
  report.desk = said;
  // A line of its own, before the run ends: the check goes on to have the
  // agent work the page while the person watches, and reads these as they come.
  process.stdout.write(`${JSON.stringify({desk: said})}\n`);
  console.error(`ok: the page is drawn on the desk after ${said.drawnMs} ms, ${(said.content * 100).toFixed(1)}% of the tile its content`);
  // Every picture that differs from the one before is the page changing
  // under their eyes. A caret blinking moves a few pixels; the agent filling
  // a field or a page showing its receipt moves many.
  let previous = drawn;
  const watching = Date.now();
  let changed = 0;
  let at = watching;
  while (changed < changes && Date.now() - watching < 600_000) {
    const picture = await look().catch(() => null);
    if (!picture) { await sleep(200); continue; }
    const moved = differingPixels(previous, picture);
    if (moved > 150) {
      previous = picture;
      const kept = await shot();
      changed++;
      writeFileSync(join(out, `page-${changed}.png`), Buffer.from(kept, 'base64'));
      const one = {n: changed, ms: Date.now() - at, moved, picture: join(out, `page-${changed}.png`)};
      at = Date.now();
      console.error(`ok: the page on the desk changed ${moved} pixels, ${one.ms} ms after the change before`);
      report.changes = [...(report.changes ?? []), one];
      process.stdout.write(`${JSON.stringify({change: one})}\n`);
      continue;
    }
    await sleep(150);
  }
  if (changed < changes) await finish(1, `the page on the desk changed ${changed} times of the ${changes} the check waits for`);
}

if (keyName) {
  if (!tiloBin || !door) await finish(2, '--key needs --tilo and --api');
  // A backend's terminal: its own home, the local door.
  const home = join(out, 'terminal');
  mkdirSync(home, {recursive: true});
  const terminal = spawn(tiloBin, ['keys', 'create', keyName], {
    env: {...process.env, HOME: home, XDG_STATE_HOME: join(home, 'state'), XDG_CONFIG_HOME: join(home, 'config'),
      TILO_API_URL: door},
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let said = '';
  let printed = '';
  terminal.stderr.on('data', chunk => { said += chunk; });
  terminal.stdout.on('data', chunk => { printed += chunk; });
  const ended = new Promise(resolve => terminal.on('exit', resolve));
  const deadline = Date.now() + 30_000;
  let link = null;
  while (!link) {
    link = said.match(/^\s*(https?:\/\/\S+)\s*$/m)?.[1] ?? null;
    if (!link && Date.now() > deadline) await finish(1, `tilo keys create printed no link: ${said}`);
    if (!link) await sleep(100);
  }
  report.approveLink = link;
  await tab.navigate(link);
  await until(`['asked', 'stopped'].includes(document.documentElement.dataset.signIn)`, 'the sign-in page asking');
  if (await tab.evaluate('document.documentElement.dataset.signIn') !== 'asked') {
    await finish(1, `the sign-in page stopped: ${await text()}`);
  }
  report.asked = await text();
  // The card the person reads before they allow: what the key reaches, and
  // what it may do in Browser.
  await capture('sign-in');
  await tab.evaluate(`document.getElementById('sign-in-allow').click()`);
  const code = await Promise.race([ended, sleep(120_000).then(() => 'timeout')]);
  if (code !== 0) {
    terminal.kill();
    await finish(1, `tilo keys create did not get the key (${code}): ${said} | the page: ${await text()}`);
  }
  report.key = printed.match(/TILO_API_KEY=(\S+)/)?.[1] ?? null;
  if (!report.key) await finish(1, `tilo keys create printed no key: ${printed}`);
  console.error(`ok: they approved "${keyName}" on the site's sign-in page and the terminal got its key`);

  // The grants page the same browser is now signed in to: the keys, each
  // saying whether it operates web pages in Browser, and the way to revoke
  // one; and the page that shows the key it makes once.
  await tab.navigate(`${door}/grants`);
  await until(`document.querySelector('ul.grants, form.new') !== null`, 'the grants page');
  await capture('grants');
  await tab.evaluate(`(() => {
    document.querySelector('form.new input[name="name"]').value = 'The grants page key';
    // What the person gives this key: a key they do not let operate web
    // pages asks them about each page action that acts on one.
    const box = document.querySelector('form.new input[name="browser_pages"]');
    if (box) box.checked = ${noPages ? 'false' : 'true'};
    document.querySelector('form.new').requestSubmit();
    return true;
  })()`);
  // The door asks the person to allow the key it would make (the same card
  // the site showed for the terminal's key); allowing it answers with the
  // key, which that page shows once.
  const making = await until(`document.querySelector('code.key') ? 'made'
    : (['asked', 'stopped'].includes(document.documentElement.dataset.signIn) ? 'asked' : '')`,
  'the card asking for the key or the key itself');
  if (making === 'asked') {
    await capture('key-ask');
    await tab.evaluate(`document.getElementById('sign-in-allow').click()`);
  }
  await until(`document.querySelector('code.key') !== null`, 'the key the grants page made');
  report.madeOnPage = await text();
  report.madeKey = await tab.evaluate(`document.querySelector('code.key').textContent.trim()`);
  if (noPages) {
    report.noPagesKey = report.madeKey;
    report.noPagesSaid = report.madeOnPage;
  }
  await capture('key');
}
await finish(0);
