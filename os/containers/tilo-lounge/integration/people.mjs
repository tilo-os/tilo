// Two people in the Lounge, as two real browsers: each context opens the web
// entry, which joins the Lounge through its door as its own person (the
// deployment admits everyone as an editor). Then one sends a chat message and
// opens a window, and the other must see both, each shown present to the
// other — the live plane's presence, the shared desk, and the chat all at
// once.
//
// The page is read through the desktop's own snapshots (its state, its window
// list, its People list and the semantic tree a screen reader reads), not
// through glyphs: what is drawn is a picture, and what a person can reach is
// the snapshot.
//
// usage: node people.mjs SITE_URL RELAY_ORIGIN WORKSPACE_ORIGIN OUT_DIR
// Prints a JSON report and saves what each page showed; exits non-zero when
// the two people did not meet.
import {mkdirSync, writeFileSync} from 'node:fs';
import {request} from 'node:https';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const [site, relay, origin, out] = process.argv.slice(2);
if (!site || !relay || !origin || !out) {
  console.error('usage: node people.mjs SITE_URL RELAY_ORIGIN WORKSPACE_ORIGIN OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});

const report = {site, relay, origin, at: new Date().toISOString()};
// Every page this run opened, so a failure leaves what each one said behind.
const opened = [];
const failed = async (message, tab) => {
  report.error = message;
  if (tab) report.shown = await snapshot(tab).catch(() => null);
  for (const page of opened) {
    writeFileSync(`${out}/${page.name}.log`, (page.tab.log ?? []).join('\n'));
    await dump(page.tab, page.name).catch(() => {});
  }
  writeFileSync(`${out}/report.json`, JSON.stringify(report, null, 2));
  console.error(`FAIL: ${message}`);
  process.exit(1);
};

// The relay's public directory: who is in the Lounge now, as the relay
// counts them. Its `here` is the live plane's presence.
const directory = () => new Promise((resolve, reject) => {
  const body = Buffer.from('{}');
  const ask = request(`${relay}/peer/v1/directory`, {
    method: 'POST', rejectUnauthorized: false,
    headers: {'content-type': 'application/json', 'content-length': body.length},
  }, response => {
    const chunks = [];
    response.on('data', chunk => chunks.push(chunk));
    response.on('end', () => {
      try { resolve(JSON.parse(Buffer.concat(chunks).toString())); } catch (error) { reject(error); }
    });
  });
  ask.on('error', reject);
  ask.end(body);
});

// What this browser kept of its join: the person it joined as, and the
// workspace it is in.
const membership = tab => tab.evaluate(`(() => {
  const all = JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}');
  return JSON.stringify(all[${JSON.stringify(new URL(origin).origin)}] ?? null);
})()`);

// The desktop's own account of itself: its state, its windows, the people it
// shows as here, and the names in every semantic node a screen reader reads.
const snapshot = async tab => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await sleep(300);
  return JSON.parse(await tab.evaluate(`(() => {
    // One field the page cannot answer yet is said as it is, never as no
    // snapshot at all: a failure must show what the page still knew.
    const safe = read => { try { return read(); } catch (error) { return \`unavailable: \${error}\`; } };
    return JSON.stringify({
    state: (() => { const s = globalThis.tiloMount?.state() ?? null;
      return s && {arrival: s.arrival ?? null, entry: s.entry ?? null, door: s.door ?? null,
        phase: s.phase ?? null, frames: s.frames ?? 0, searchOpen: !!s.searchOpen,
        title: document.title}; })(),
    windows: safe(() => globalThis.tiloMount?.navigation?.getSnapshot?.().entries?.map(entry => entry.label) ?? []),
    people: safe(() => globalThis.tiloMount?.people?.() ?? null),
    live: safe(() => globalThis.tiloMount?.live?.() ?? null),
    pointers: safe(() => globalThis.tiloMount?.pointers?.() ?? null),
    channels: safe(() => globalThis.tiloChannels?.() ?? ''),
    focus: {active: document.activeElement?.nodeName ?? null, focus: document.hasFocus(),
      canvas: document.activeElement === globalThis.tiloMount?.canvas},
    chords: (globalThis.__chords ?? []).map(event => ({key: event.key, code: event.code,
      ctrl: event.ctrlKey, meta: event.metaKey, shift: event.shiftKey,
      trusted: event.isTrusted, prevented: event.defaultPrevented})),
    keys: (globalThis.__keys ?? []).slice(-20),
    // The widgets the desktop published this frame, by the name it knows them
    // by: what a page reading the snapshot can tell apart.
    widgets: (() => { const out = [];
      const walk = n => { if (n.widget) out.push(n.widget); (n.children || []).forEach(walk); };
      const root = globalThis.tiloMount?.state()?.semantics?.root;
      if (root) walk(root);
      return [...new Set(out)]; })(),
    names: (() => { const out = [];
      const node = n => { out.push(n.name ?? ''); (n.children || []).forEach(node); };
      const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
      const root = globalThis.tiloMount?.state()?.semantics?.root;
      if (root) walk(root);
      return out; })(),
    });
  })()`));
};

const dump = async (tab, name) => {
  const shown = await snapshot(tab);
  shown.membership = JSON.parse(await membership(tab));
  shown.text = await tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').slice(0, 4000)`);
  writeFileSync(`${out}/${name}.json`, JSON.stringify({...shown, log: tab.log}, null, 2));
  const shot = await tab.screenshot();
  if (shot) writeFileSync(`${out}/${name}.png`, shot);
  return shown;
};

// Wait until `condition` (evaluated in the page) holds, or say what the page
// showed instead. A page that never shows something is the failure, not a
// timeout to stretch.
const until = async (tab, expression, what, timeout = 90_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await tab.evaluate(expression).catch(() => false);
    if (value) return value;
    if (Date.now() > deadline) {
      await dump(tab, `timeout-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
      failed(`${what} never appeared`);
    }
    await sleep(250);
  }
};

// Keys as the desktop's search reads them (its own scripts drive it this
// way): the chord, the query, Enter.
const key = async (tab, name, modifiers = 0) => {
  const named = {Enter: 13, Escape: 27, ArrowDown: 40, ArrowUp: 38};
  const code = name.length === 1 ? `Key${name.toUpperCase()}` : name;
  const event = {key: name, code, windowsVirtualKeyCode: named[name] ?? name.toUpperCase().charCodeAt(0), modifiers};
  await tab.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', ...event, ...(name === 'Enter' ? {text: '\r'} : {})});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', ...event});
  await sleep(300);
};
const type = async (tab, text) => {
  for (const ch of text) {
    await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
  }
  await sleep(600);
};
// The names the command palette shows, in order, as the desktop publishes
// them: its query field, its result rows and its actions.
const paletteRows = async tab => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()');
  await sleep(500);
  return JSON.parse(await tab.evaluate(`(() => {
    let rows = null;
    const collect = node => {
      rows = [];
      const walk = semantics => { rows.push(semantics.name ?? ''); (semantics.children || []).forEach(walk); };
      if (node.semantics) walk(node.semantics);
    };
    const find = node => { if (node.widget === 'ActionPalette') collect(node); (node.children || []).forEach(find); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) find(root);
    return JSON.stringify(rows);
  })()`));
};
// A person's search: the chord, the query, and Enter on the row the palette
// offers for it. The rows are read before Enter, so Enter chooses the row the
// query found rather than racing the search that fills it.
const search = async (tab, command, query, row) => {
  await click(tab, 640, 700);
  await key(tab, 'k', command);
  await until(tab, '!!globalThis.tiloMount?.state()?.searchOpen', `search for "${query}"`, 60_000);
  await type(tab, query);
  const deadline = Date.now() + 60_000;
  for (;;) {
    const rows = await paletteRows(tab);
    if (rows && rows.includes(row)) {
      await key(tab, 'Enter');
      return rows;
    }
    if (Date.now() > deadline) {
      await dump(tab, `timeout-rows-for-${query}`);
      failed(`the palette offers no ${row} for "${query}": ${JSON.stringify(rows)}`);
    }
    await sleep(250);
  }
};
// A canvas-drawn desktop takes the pointer; the search chord is Ctrl+K away
// from the keyboard (Meta on a Mac, as the desktop reads it).
const canvasTopLeft = tab => tab.evaluate(`(() => {
  const rect = globalThis.tiloMount?.canvas?.getBoundingClientRect();
  return JSON.stringify(rect ? {x: rect.left, y: rect.top, width: rect.width, height: rect.height} : null);
})()`);
const click = async (tab, x, y) => {
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
  for (const type of ['mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
  }
  await sleep(600);
};
// The centre of the first semantic node named `name`, in page pixels.
const find = (tab, name) => tab.evaluate(`(() => {
  const found = [];
  const node = n => { found.push({name: n.name ?? '', bounds: n.bounds ?? n.rect ?? null}); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  return JSON.stringify(found.find(n => n.name === ${JSON.stringify(name)} && n.bounds) ?? null);
})()`);
const press = async (tab, name) => {
  const node = JSON.parse(await find(tab, name));
  if (!node) return false;
  const [x0, y0, x1, y1] = Array.isArray(node.bounds)
    ? node.bounds
    : [node.bounds.x0 ?? node.bounds.x, node.bounds.y0 ?? node.bounds.y,
       node.bounds.x1 ?? node.bounds.x + node.bounds.width, node.bounds.y1 ?? node.bounds.y + node.bounds.height];
  const corner = JSON.parse(await canvasTopLeft(tab));
  await click(tab, corner.x + (x0 + x1) / 2, corner.y + (y0 + y1) / 2);
  return true;
};

const open = async name => {
  const context = await browser.context();
  const tab = await browser.tab({context});
  // The window is the size the desktop draws for, before it draws: resizing
  // a presented surface suspends and re-presents it, and keys sent between
  // the two are nobody's.
  await tab.send('Emulation.setDeviceMetricsOverride',
    {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false}).catch(() => {});
  await tab.navigate(site);
  await until(tab, `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, `${name} arriving`, 240_000);
  const arrival = JSON.parse(await tab.evaluate(`JSON.stringify({
    arrival: document.documentElement.dataset.arrival ?? null,
    door: document.documentElement.dataset.door ?? null,
    phase: document.documentElement.dataset.phase ?? null,
  })`));
  if (arrival.arrival !== 'done') failed(`${name} did not arrive: ${JSON.stringify(arrival)}`);
  await sleep(2000);
  // What reached the page's outermost key listener, before the desktop's, and
  // what the desktop's own surface was handed.
  await tab.evaluate(`globalThis.__chords = [];
    globalThis.__keys = [];
    window.addEventListener('keydown', event => {
      if (event.key === 'k' || event.code === 'KeyK') globalThis.__chords.push(event);
    }, {capture: true});
    globalThis.tiloMount?.canvas?.addEventListener('keydown',
      event => globalThis.__keys.push({key: event.key, code: event.code, prevented: event.defaultPrevented}));`);
  const corner = JSON.parse(await canvasTopLeft(tab));
  // CDP modifier bits: Ctrl 2, Meta 4.
  const mac = await tab.evaluate('/Mac/.test(navigator.platform)');
  report[name] = {arrival, canvas: corner, mac};
  opened.push({name, tab});
  return {tab, command: mac ? 4 : 2};
};

// SwiftShader's WebGPU, as the app SDK's lane runs the bundle: a host GPU in
// headless Chrome can lose its device mid-run.
const browser = await launch({args: [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  // A job container's /dev/shm is 64 MiB: a renderer that runs out of it dies.
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
]});
try {
  const a = await open('A');
  const b = await open('B');

  // Each context is its own person: its own root, its own device, and a
  // membership in the Lounge's workspace that nobody else holds.
  const joined = async (person, name) => {
    await until(person.tab, `JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')[${JSON.stringify(new URL(origin).origin)}]?.room`,
      `${name} joining the Lounge`, 240_000);
    const kept = JSON.parse(await membership(person.tab));
    if (!kept?.userPublicKey || !kept?.room) failed(`${name} joined nothing: ${JSON.stringify(kept)}`);
    person.membership = kept;
    return kept;
  };
  const keptA = await joined(a, 'A');
  const keptB = await joined(b, 'B');
  report.people = {a: keptA, b: keptB};
  if (keptA.userPublicKey === keptB.userPublicKey) failed('both browsers are one person');
  if (keptA.room !== keptB.room) failed('the two joined different workspaces');
  report.distinct = true;
  console.log(`ok: two browsers joined the Lounge as ${keptA.userPublicKey.slice(0, 8)} and ${keptB.userPublicKey.slice(0, 8)}`);

  // The relay lists both as here: both pages' live planes connected and the
  // host counts them.
  const listed = await directory();
  const lounge = listed.workspaces?.find(workspace => workspace.room === keptA.room);
  if (!lounge) failed(`the directory does not list the workspace both joined: ${JSON.stringify(listed)}`);
  report.listing = lounge;
  const here = lounge.here ?? [];
  report.here = here;
  for (const person of [keptA, keptB]) {
    if (!here.some(face => face.userPublicKey === person.userPublicKey)) {
      failed(`the directory does not show ${person.userPublicKey} as here: ${JSON.stringify(here)}`);
    }
  }

  // A opens the Lounge's chat the way a person does — search, the app's name,
  // Enter — and writes in it: the hello lands in B's.
  await search(a.tab, a.command, 'chat', 'Chat');
  await until(a.tab, `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => (entry.label ?? '').toLowerCase() === 'chat')`,
    "A's own chat window", 120_000);
  await until(a.tab, `JSON.stringify(globalThis.tiloMount?.state()?.semantics ?? {}).includes('Message composer')`,
    "A's chat composer", 120_000);
  const message = `hello from A ${Date.now()}`;
  if (!await press(a.tab, 'Message composer')) failed("A's chat has no message composer", a.tab);
  await type(a.tab, message);
  await key(a.tab, 'Enter');
  await until(b.tab, `JSON.stringify(globalThis.tiloMount?.state()?.semantics ?? {}).includes(${JSON.stringify(message)})`,
    'the chat message in B', 180_000);
  report.chat = message;
  console.log('ok: the chat message from A is in B');
  await dump(a.tab, '3-a-chat');
  await dump(b.tab, '3-b-chat');

  // A window A opens is in B's strip: one desk, every member of it.
  const beforeB = await dump(b.tab, '4-b-before-open');
  await search(a.tab, a.command, 'notes', 'Notes');
  await until(a.tab, `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.label === 'Notes')`,
    "A's own Notes window", 120_000);
  await until(b.tab, `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.label === 'Notes')`,
    "A's Notes window in B", 180_000);
  report.shared = {bBefore: beforeB.windows, bAfter: (await snapshot(b.tab)).windows};
  console.log('ok: the window A opened is in B\'s strip');
  await dump(a.tab, '5-a-open');
  await dump(b.tab, '5-b-open');

  // Each desk shows the other as here and never itself, with a window of
  // theirs focused: the desktop's People list is what presence is on the
  // page, one row per person however many devices they are here from.
  const herePeople = async tab =>
    JSON.parse(await tab.evaluate('JSON.stringify(globalThis.tiloMount?.people?.() ?? null)'));
  const oneOther = async (person, who) => {
    const deadline = Date.now() + 120_000;
    for (;;) {
      const rows = await herePeople(person.tab);
      if (Array.isArray(rows) && rows.length === 1) return rows[0];
      if (Date.now() > deadline) {
        await dump(person.tab, `timeout-people-${who}`);
        failed(`${who} does not show exactly one other person here: ${JSON.stringify(rows)}`, person.tab);
      }
      await sleep(500);
    }
  };
  report.presence = {b: await oneOther(b, 'B'), a: await oneOther(a, 'A')};
  if (report.presence.a.id === report.presence.b.id) {
    failed(`each desk shows the same person: ${JSON.stringify(report.presence)}`);
  }
  console.log('ok: each desk shows the other person here');

  // Nothing failed in either page.
  const both = [...a.tab.log, ...b.tab.log];
  const errors = both.filter(line => /^EXC/.test(line) || /panic/i.test(line));
  report.logs = {a: a.tab.log.slice(-10), b: b.tab.log.slice(-10)};
  if (errors.length) failed(`pages reported errors: ${errors.slice(0, 5).join('; ')}`);

  // The desk one person arranged is the desk the other shows: neither page's
  // write of the shared desktop was refused.
  const refused = both.filter(line => /the workspace refused a change/.test(line));
  report.refused = refused;
  if (refused.length) failed(`a page's change was refused: ${refused.slice(0, 5).join('; ')}`);
  // A frame repainted in part must leave no stale pixel. A page built with the
  // retained-frame check says so where it happens; the check also says when it
  // could not read its two frames back. Both are reported: the pages' frames
  // are compared here, so a run that sees them says what it saw rather than
  // hiding it, and a check that cannot compare on the web is visible too.
  report.mismatched = both.filter(line => /retained frame differs from a full render/.test(line));
  report.unreadable = both.filter(line => /could not read a frame back/.test(line));

  writeFileSync(`${out}/report.json`, JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
} catch (error) {
  await failed(String(error?.stack ?? error));
} finally {
  await browser.close().catch(error => console.error('close:', error.message));
}
process.exit(0);
