// Two people in the Lounge, as two real browsers: each browser opens the web
// entry, which joins the Lounge through its door as its own person (the
// deployment admits everyone as an editor). Then one sends a chat message and
// opens a window, and the other must see both, each shown present to the
// other — the live plane's presence, the shared desk, and the chat all at
// once.
//
// Then one of them is made someone the workspace lets invite, while their page
// is open (the deployment's roles file is written under them, as an operator
// does), and invites a third person from the page's own sheet. That person
// opens the link the sheet handed over — `https://<site>/i/<handle>#<key>` — in
// a browser of their own, is told who invited them, to what and as what, and
// joins with no one in the workspace answering for them: an invitation is its
// inviter's approval.
//
// The page is read through the desktop's own snapshots (its state, its window
// list, its People list and the semantic tree a screen reader reads), not
// through glyphs: what is drawn is a picture, and what a person can reach is
// the snapshot.
//
// usage: node people.mjs SITE_URL RELAY_ORIGIN WORKSPACE_ORIGIN OUT_DIR
// ROLES_FILE
// ROLES_FILE is the file the deployment mounts as the Lounge's roles file,
// which this run writes the inviter into.
// With EARLIER as the seventh argument the run is the other way round: one
// person, who was not here when the workspace last started, joins and reads the
// chat message EARLIER — written before it by pages that are gone — and writes
// in it. What a page sees of the past is the workspace's, not a store beside it.
//
// Prints a JSON report and saves what each page showed; exits non-zero when
// the two people did not meet, when the invitation took more than an
// invitation, or when a person joining later finds no chat history.
import {mkdirSync, writeFileSync} from 'node:fs';
import {request} from 'node:https';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import * as waits from './waits.mjs';

const [site, relay, origin, out, rolesFile, earlier] = process.argv.slice(2);
if (!site || !relay || !origin || !out || !rolesFile) {
  console.error('usage: node people.mjs SITE_URL RELAY_ORIGIN WORKSPACE_ORIGIN OUT_DIR ROLES_FILE [EARLIER]');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
// Brief: the steps that caught what broke between two people's pages (chat,
// presence, a call and the names on its card, the strip over a few apps),
// for a check that runs in minutes. The gate runs it all: every app, the
// menus, an invitation.
const brief = process.env.TILO_PEOPLE_BRIEF === '1';
const BRIEF_APPS = ['Notes', 'Sheets', 'Calculator', 'Clocks', 'Weather', 'Tasks'];

const report = {site, relay, origin, at: new Date().toISOString()};
// Every page this run opened, so a failure leaves what each one said behind.
const opened = [];
const failed = async (message, tab) => {
  report.error = message;
  if (tab) report.shown = await snapshot(tab).catch(() => null);
  if (tab && !opened.some(page => page.tab === tab)) writeFileSync(`${out}/failed-page.log`, (tab.log ?? []).join('\n'));
  for (const page of opened) {
    writeFileSync(`${out}/${page.name}.log`, (page.tab.log ?? []).join('\n'));
    await dump(page.tab, page.name).catch(() => {});
  }
  writeFileSync(`${out}/report.json`, JSON.stringify(report, null, 2));
  // What each page's console kept, so the run's own log says why it stopped
  // without anyone having to open the files it left behind. The page that
  // failed is said too: one that never arrived never got as far as opening.
  const said = [...opened.map(page => [page.name, page.tab]), ...(tab && !opened.some(page => page.tab === tab) ? [['the page', tab]] : [])];
  for (const [name, page] of said) {
    const lines = (page.log ?? []).slice(-40);
    if (lines.length) console.error(`--- ${name} said:\n${lines.join('\n')}`);
  }
  console.error(`FAIL: ${message}`);
  process.exit(1);
};

// The relay's public directory: who is in the Lounge now, as the relay
// counts them. Its `here` is the relay's HTTPS page sessions, not the live
// plane's members: a page the plane never carried still counts here.
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
        // Frames the GPU left unreported since it last reported one: what a
        // page that stopped drawing while it stayed alive says.
        gpuStalls: s.gpuStalls ?? 0,
        title: document.title}; })(),
    windows: safe(() => globalThis.tiloMount?.navigation?.getSnapshot?.().entries?.map(entry => entry.label) ?? []),
    people: safe(() => globalThis.tiloMount?.people?.() ?? null),
    live: safe(() => globalThis.tiloMount?.live?.() ?? null),
    call: safe(() => globalThis.tiloMount?.call?.() ?? null),
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
  // Whether the desk is still stepping, or only what it last drew: a page that
  // stops stepping answers nothing, whatever it shows.
  const frames = () => tab.evaluate('globalThis.tiloMount?.state()?.frames ?? null').catch(() => null);
  const before = await frames();
  await sleep(1500);
  shown.stepping = {before, after: await frames()};
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
      // Where the page stopped, in the failure line itself: the step it was
      // in says more than the wait that ran out.
      const shown = await tab.evaluate(`JSON.stringify({
        arrival: document.documentElement.dataset.arrival ?? null,
        door: document.documentElement.dataset.door ?? null,
        phase: document.documentElement.dataset.phase ?? null,
        progress: document.documentElement.dataset.arrivalProgress ?? null,
        status: document.getElementById('status')?.textContent ?? null,
      })`).catch(() => 'unreadable');
      await dump(tab, `timeout-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
      await failed(`${what} never appeared: ${shown}`, tab);
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
// The desk presented `by` more frames: what waits on the desk's own work
// waits on that, not on a stopwatch, however slow presenting is. A page
// drawing a megapixel through a software rasteriser draws a few frames a
// second, and every wait here is a number of frames for that reason.
//
// A desk with nothing to draw presents none — it waits for what changes —
// so a frame count is progress only while the desk owes work. `owed` says
// whether it still does: the wait ends when the frames came or the desk says
// it has nothing left to draw, and never outlives the work it waits for.
const settle = (tab, by = 2, owed = () => true) => waits.settle({
  evaluate: tab.evaluate.bind(tab),
  by,
  owed,
  sleep,
  timeoutMs: 120_000,
  pollMs: 100,
  onTimeout: ({from, by}) => until(tab, `(globalThis.tiloMount?.state()?.frames ?? 0) >= ${from + by}`,
    `the desk stepping ${by} more frame(s)`, 1),
});
// Whether the open search still owes rows: the desk's own answer, which a
// settled frame would not carry, as a settled desk presents none.
const searching = tab => waits.searching(tab);
// A person's search: the chord, the query, and Enter on the row the palette
// offers for it. The host answers a query a moment after the last key, and the
// rows it shows before that answer are the previous query's: they are read
// until two reads agree and the row asked for is the one Enter acts on — the
// first result under the palette's heading, which is what the highlight sits
// on. Reading only for the row's presence raced that answer and sent Enter to
// a row left over from the query before.
const search = async (tab, command, query, row, opens = null) => {
  // The desk keeps the search up until what it opened has arrived, and the
  // chord toggles it: it is opened only when it is down.
  if (!await tab.evaluate('!!globalThis.tiloMount?.state()?.searchOpen')) {
    await click(tab, 640, 700);
    await key(tab, 'k', command);
  }
  // A desk presenting a megapixel through software draws a few frames a
  // second: waits here are counted in frames, so they are sized for a page
  // that is slow rather than quiet.
  await until(tab, '!!globalThis.tiloMount?.state()?.searchOpen', `search for "${query}"`, 120_000);
  await type(tab, query);
  const first = rows => (rows ? rows[rows.indexOf('Results') + 1] ?? null : null);
  let chosen = null;
  const deadline = Date.now() + 120_000;
  // The text is in before its answer is: two reads a frame apart, agreeing,
  // are the palette's answer to this query and not a frame of the one before.
  // A settled desk presents no frame, so the wait between the two reads is
  // the desk's search saying it owes no more rows as much as a frame.
  await settle(tab, 2, () => searching(tab));
  for (;;) {
    const rows = await paletteRows(tab);
    await settle(tab, 1, () => searching(tab));
    const again = await paletteRows(tab);
    if (first(rows) === row && JSON.stringify(rows) === JSON.stringify(again)) {
      // Enter is what opens the app: the watch is in the page before the press,
      // so the frame it closes search and the frame the app's window arrives on
      // are told apart on the page's own clock.
      if (opens) await armOpen(tab, opens);
      await key(tab, 'Enter');
      chosen = rows;
      break;
    }
    if (Date.now() > deadline) {
      await dump(tab, `timeout-rows-for-${query}`);
      failed(`the palette offers no ${row} first for "${query}": ${JSON.stringify(again)}`);
    }
  }
  await until(tab, '!globalThis.tiloMount?.state()?.searchOpen', `the search closing after ${row}`, 180_000);
  return chosen;
};
// A page-side clock around the person's Enter. What a person waits for is the
// desk showing the app's window, and this desk paints into one canvas: the
// window is on screen when the frame that carries it is presented. So both
// moments are frames the page presented — the press, stamped where the key
// arrives, and the first presented frame after it that lists the window —
// kept on the page's own clock, where a round trip from here never lands in
// the number.
//
// The press is stamped by a `keydown` listener, not by reading the desk's
// state: a MutationObserver runs only after the frame the key caused has
// already published, so a watch that took its press from the first status
// showing search closed dated the press at or after the frame it was meant to
// precede, and every open read as 0 ms.
//
// The frame that counts is the first presented one after the press whose
// navigation lists the app's window: this desk draws a window in the pass that
// publishes it, so that frame carries the window's pixels, where the entry
// alone can be published a frame before its pixels are shown.
const armOpen = (tab, app) => tab.evaluate(`(() => {
  const mount = globalThis.tiloMount;
  const host = mount.canvas.parentElement;
  const watch = globalThis.__tiloOpen = {app: ${JSON.stringify(app)}, pressedAt: null,
    framesAtPress: null, shownAt: null, framesShown: null};
  if (!globalThis.__tiloOpenKeys) {
    globalThis.__tiloOpenKeys = true;
    addEventListener('keydown', event => {
      const w = globalThis.__tiloOpen;
      if (!w || w.pressedAt !== null) return;
      if (event.key !== 'Enter' && event.key !== '\\r') return;
      w.pressedAt = performance.now();
      w.framesAtPress = Number(mount.canvas.parentElement.dataset.frames ?? 0);
    }, true);
  }
  const read = () => {
    const w = globalThis.__tiloOpen;
    if (!w || w.pressedAt === null || w.shownAt !== null) return;
    if (host.dataset.phase !== 'presented') return;
    if (Number(host.dataset.frames ?? 0) <= w.framesAtPress) return;
    if ((mount.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === w.app)) {
      w.shownAt = performance.now();
      w.framesShown = Number(host.dataset.frames ?? 0);
    }
  };
  new MutationObserver(read).observe(host, {attributes: true, attributeFilter: ['data-frames', 'data-phase']});
  // The window's entry can be published after the desk's last frame, with no
  // frame after it: read again as the navigation changes too, and once more
  // as the marks are taken. Shown is then never earlier than both a frame
  // after the key and the entry, so an open is never measured faster than it was.
  mount.navigation?.subscribe?.(read);
  watch.read = read;
  return true;
})()`);
// What the open cost, in the desk's own unit: the window is drawn on the frame
// right after the one that took the key, so the open is worth no more frames
// than that. The milliseconds ride along as what this machine's rasteriser
// took to put that frame on screen — a megapixel through software costs about
// half a second here and near nothing on a display, and neither says anything
// about whether the desk heard the person.
//
// What was not measured is `ok: false`, never a number: an open the watch
// never saw begun or never saw end is no reading at all, and the caller fails
// on it rather than counting `0` as fast.
const openMarks = tab => tab.evaluate(`(() => {
  const watch = globalThis.__tiloOpen;
  watch?.read?.();
  globalThis.__tiloOpen = undefined;
  if (!watch || watch.pressedAt === null || watch.shownAt === null) {
    // What the watch saw, so an unmeasured open says which condition held it back.
    const host = globalThis.tiloMount?.canvas?.parentElement;
    return JSON.stringify({ok: false, pressed: watch?.pressedAt != null, shown: watch?.shownAt != null,
      phase: host?.dataset.phase ?? null, frames: Number(host?.dataset.frames ?? NaN), framesAtPress: watch?.framesAtPress ?? null,
      entry: (globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === watch?.app),
      subscribes: typeof globalThis.tiloMount?.navigation?.subscribe === 'function'});
  }
  return JSON.stringify({ok: true,
    presentMs: Math.round(watch.shownAt - watch.pressedAt),
    frames: watch.framesShown - watch.framesAtPress});
})()`).then(JSON.parse);

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
// The page point at the centre of a node's `bounds`, in canvas coordinates.
const centreOfNode = async (tab, node) => {
  const [x0, y0, x1, y1] = Array.isArray(node.bounds)
    ? node.bounds
    : [node.bounds.x0 ?? node.bounds.x, node.bounds.y0 ?? node.bounds.y,
       node.bounds.x1 ?? node.bounds.x + node.bounds.width, node.bounds.y1 ?? node.bounds.y + node.bounds.height];
  const corner = JSON.parse(await canvasTopLeft(tab));
  return {x: Math.round(corner.x + (x0 + x1) / 2), y: Math.round(corner.y + (y0 + y1) / 2)};
};
// Where Notes' New note button is drawn: in Notes' window, wherever on the
// strip the search brought it.
const notesPoint = async tab => {
  for (let tries = 0; tries < 20; tries++) {
    await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    await sleep(200);
    const node = JSON.parse(await find(tab, 'New note'));
    if (node) return centreOfNode(tab, node);
  }
  return null;
};
// Where Sheets' cell B2 is drawn, once the desktop publishes it.
const sheetCell = async tab => {
  for (let tries = 0; tries < 20; tries++) {
    await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    await sleep(200);
    const node = JSON.parse(await find(tab, 'B2'));
    if (node) return centreOfNode(tab, node);
  }
  return null;
};
// The names of the nodes drawn at page point (x, y), innermost first.
const namesAt = async (tab, x, y) => {
  const corner = JSON.parse(await canvasTopLeft(tab));
  return JSON.parse(await tab.evaluate(`(() => {
    const [px, py] = [${x - corner.x}, ${y - corner.y}];
    const hits = [];
    const box = b => Array.isArray(b) ? b : [b.x0 ?? b.x, b.y0 ?? b.y, b.x1 ?? b.x + b.width, b.y1 ?? b.y + b.height];
    // As \`find\` reads the tree: each widget's semantics, and theirs below.
    const node = s => {
      const b = s.bounds ?? s.rect;
      if (b) { const [x0, y0, x1, y1] = box(b); if (px >= x0 && px <= x1 && py >= y0 && py <= y1) hits.push([(x1 - x0) * (y1 - y0), s.name ?? '']); }
      (s.children || []).forEach(node);
    };
    const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) walk(root);
    return JSON.stringify(hits.sort((a, b) => a[0] - b[0]).map(h => h[1]).filter(Boolean).slice(0, 6));
  })()`).catch(() => '[]'));
};
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
// Press the control `name` once the desktop publishes it: the tree is read
// again until the control is there, then pressed where it is drawn.
const pressWhen = async (tab, name, what, timeout = 120_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    if (await press(tab, name)) return;
    if (Date.now() > deadline) {
      await dump(tab, `timeout-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
      failed(`${what} never appeared: ${name}`);
    }
    await sleep(300);
  }
};
// Who stands under the navigation row named `name`: a list publishes its rows
// flat, each with the level it stands at, so a row's people are the rows that
// follow it one level deeper, up to the next row at its own.
const rowsUnder = (tab, name) => tab.evaluate(`(() => {
  let found = null;
  const collect = node => {
    if (found && found.length) return;
    const children = node.children || [];
    const at = children.findIndex(child => child.name === ${JSON.stringify(name)} && child.level != null);
    if (at < 0) return;
    const level = children[at].level;
    const under = [];
    for (const row of children.slice(at + 1)) {
      if (row.level == null || row.level <= level) break;
      under.push(row.name ?? '');
    }
    found = under;
  };
  const walk = n => { if (n.semantics) collect(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  return JSON.stringify(found);
})()`);
// The call as the page reads it: its phase, whether its microphone runs and
// what left and arrived over the plane.
const callOn = async tab => JSON.parse(await tab.evaluate('JSON.stringify(globalThis.tiloMount?.call?.() ?? null)'));

// A person's browser, its window the size the desktop draws for before it
// draws: resizing a presented surface suspends and re-presents it, and keys
// sent between the two are nobody's. A browser paints the tab it shows and
// leaves the others as they were, so each person gets one of their own: a
// second tab in one browser is a desk that stops answering while the other
// person is looked at.
// People who come back to the site: a browser new to it opens on the first
// screen (set up with an agent, or skip to one's own desktop), which these
// people have been past already, so their page opens the Lounge itself.
const returning = tab => tab.send('Page.addScriptToEvaluateOnNewDocument',
  {source: "try { localStorage.setItem('tilo-first-run', '1'); } catch {}"});
const anotherBrowser = async () => {
  const launched = await launch({args: browserArgs});
  browsers.push(launched);
  const tab = await launched.tab();
  await tab.send('Emulation.setDeviceMetricsOverride',
    {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false}).catch(() => {});
  await returning(tab);
  await recordLoading(launched, tab);
  return tab;
};
// What a page's loading said beside its console's errors: requests that
// failed and the browser's own log (a resource that did not load is said
// there, not in the console), and the page's console in full, so a page that
// stops while it loads leaves why.
const recordLoading = async (launched, tab) => {
  const asked = new Map();
  launched.on(({sessionId, method, params}) => {
    if (sessionId !== tab.sessionId) return;
    if (method === 'Network.requestWillBeSent') asked.set(params.requestId, params.request.url);
    else if (method === 'Network.loadingFailed') {
      tab.log.push(`NET ${asked.get(params.requestId) ?? params.requestId}: ${params.errorText}${params.canceled ? ' (canceled)' : ''}`);
    } else if (method === 'Network.responseReceived' && params.response.status >= 400) {
      tab.log.push(`HTTP ${params.response.status} ${params.response.url}`);
    } else if (method === 'Log.entryAdded') {
      tab.log.push(`LOG ${params.entry.level} ${params.entry.text}${params.entry.url ? ` (${params.entry.url})` : ''}`.slice(0, 600));
    } else if (method === 'Runtime.consoleAPICalled' && !['error', 'warning'].includes(params.type)) {
      tab.log.push(`${params.type.toUpperCase()} ${params.args.map(a => a.value ?? a.description ?? '').join(' ')}`.slice(0, 600));
    }
  });
  await tab.send('Network.enable', {}).catch(() => {});
  await tab.send('Log.enable', {}).catch(() => {});
};
// What a browser that reached the desktop leaves with the run: its arrival,
// the keys reaching it, and its canvas.
const mounted = async (tab, name) => {
  const arrival = JSON.parse(await tab.evaluate(`JSON.stringify({
    arrival: document.documentElement.dataset.arrival ?? null,
    door: document.documentElement.dataset.door ?? null,
    phase: document.documentElement.dataset.phase ?? null,
    // Why a page stopped: refused, failed or unsupported, in the words it shows.
    entry: document.documentElement.dataset.entry ?? null,
    status: document.getElementById('status')?.textContent?.trim().slice(0, 300) ?? null,
  })`));
  if (arrival.arrival !== 'done') await failed(`${name} did not arrive: ${JSON.stringify(arrival)}`, tab);
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
const open = async name => {
  const tab = await anotherBrowser();
  await tab.navigate(site);
  await until(tab, `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, `${name} arriving`, 240_000);
  return mounted(tab, name);
};
// The invitations a site hands over: `https://<site>/i/<handle>#<key>`, the
// handle and the key alone, the key never in the part a site's logs see.
const sealedLink = /^https:\/\/[^/]+\/i\/[A-Za-z0-9_-]{16}#[A-Za-z0-9_-]{43}$/;
// The page's words over the page, as a person at a link reads them.
const pageText = tab => tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim()`).catch(() => '');
// A person following a link: before anything is mounted, the page says who
// invited them, to what and as what, and only then offers to join.
const followLink = async (name, link) => {
  const tab = await anotherBrowser();
  await tab.navigate(link);
  const deadline = Date.now() + 120_000;
  for (;;) {
    const card = await pageText(tab);
    if (card.includes('invites you to')) {
      writeFileSync(`${out}/${name}-card.txt`, card);
      const buttons = JSON.parse(await tab.evaluate(
        `JSON.stringify([...document.querySelectorAll('button')].map(button => button.textContent.trim()))`));
      return {tab, card, buttons};
    }
    if (Date.now() > deadline) {
      writeFileSync(`${out}/${name}-card.txt`, card);
      await failed(`${name}'s link showed no invitation: ${card.slice(0, 400)}`, tab);
    }
    await sleep(250);
  }
};
// The first semantic node named `name`, once the page shows it.
const waitFor = async (tab, name, what, timeout = 120_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const node = JSON.parse(await find(tab, name));
    if (node) return node;
    if (Date.now() > deadline) await failed(`${what} never appeared`, tab);
    await sleep(250);
  }
};
// Every value the desktop's semantic tree carries, as the sheet's link is one.
const nodeValues = tab => tab.evaluate(`JSON.stringify((() => {
  const out = [];
  const node = n => { if (n.value) out.push(n.value); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  return out;
})())`).then(JSON.parse);

// SwiftShader's WebGPU, as the app SDK's lane runs the bundle: a host GPU in
// headless Chrome can lose its device mid-run.
const browserArgs = [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  // A person's call is heard through a microphone and played through a
  // speaker: this machine has no person at it, so the browser carries both —
  // a synthetic microphone the call really encodes and sends, and no prompt
  // before a person would have pressed Allow.
  '--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream',
  '--autoplay-policy=no-user-gesture-required',
  // A job container's /dev/shm is 64 MiB: a renderer that runs out of it dies.
  '--disable-dev-shm-usage',
  // A browser a person is not looking at still answers: a desk slowed down to
  // save power answers the next key late.
  '--disable-background-timer-throttling', '--disable-renderer-backgrounding',
  '--disable-backgrounding-occluded-windows',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
];
const browsers = [];
try {
  if (earlier) {
    // A person arriving now: the chat window and the composer are the app's,
    // the message in it was written before this start by a page that is gone.
    const visitor = await open('V');
    await search(visitor.tab, visitor.command, 'chat', 'Chat');
    const showing = `JSON.stringify(globalThis.tiloMount?.state()?.semantics ?? {})`;
    await until(visitor.tab,
      `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === 'so.tilo.chat')`,
      "V's own chat window", 120_000);
    await until(visitor.tab, `${showing}.includes('Message composer')`, "V's chat composer", 120_000);
    await until(visitor.tab, `${showing}.includes(${JSON.stringify(earlier)})`,
      'the chat message the workspace kept from before this start', 180_000);
    report.earlier = earlier;
    console.log(`ok: the chat the workspace kept shows "${earlier}" to a person who was not here`);
    const reply = `here now ${Date.now()}`;
    if (!await press(visitor.tab, 'Message composer')) failed("V's chat has no message composer", visitor.tab);
    await type(visitor.tab, reply);
    await key(visitor.tab, 'Enter');
    await until(visitor.tab, `${showing}.includes(${JSON.stringify(reply)})`, 'the reply in the chat', 180_000);
    report.reply = reply;
    await dump(visitor.tab, 'earlier-chat');
    console.log('ok: the person joining after the restart wrote in the same chat');
    writeFileSync(`${out}/report.json`, JSON.stringify(report, null, 2));
    console.log(JSON.stringify(report, null, 2));
    await browser.close().catch(error => console.error('close:', error.message));
    process.exit(0);
  }
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

  // The relay describes both as here: both pages opened their HTTPS sessions.
  // That is the host's own count, not the live plane's — the plane is read
  // from each page's channel diagnostics below.
  // A page counts there once its session is open, a moment after it joined:
  // the directory is read until both are, or a minute has passed.
  const bothHere = here => [keptA, keptB].every(person => here.some(face => face.userPublicKey === person.userPublicKey));
  let listed;
  let lounge;
  for (const deadline = Date.now() + 60_000; ;) {
    listed = await directory();
    lounge = listed.workspaces?.find(workspace => workspace.room === keptA.room);
    if ((lounge && bothHere(lounge.here ?? [])) || Date.now() > deadline) break;
    await sleep(500);
  }
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
  await until(a.tab, `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === 'so.tilo.chat')`,
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

  // Each page joined the live plane over the host's own iroh: its channel
  // diagnostics name the plane and the devices it reads. A page whose plane
  // never opened lists nobody and says so there instead, so this is the
  // host's address, its relay and the plane all read from the page that used
  // them.
  const joinedPlane = async (person, who) => {
    const deadline = Date.now() + 120_000;
    for (;;) {
      const channels = await person.tab.evaluate('globalThis.tiloChannels?.() ?? ""');
      const joined = channels
        .split('\n')
        .find(entry => /^plane \d+: epoch \d+, reads \d+ device\(s\)/.test(entry));
      if (joined) return joined;
      if (Date.now() > deadline) {
        await dump(person.tab, `timeout-plane-${who}`);
        failed(`${who} joined no live plane: ${channels}`, person.tab);
      }
      await sleep(500);
    }
  };
  report.planes = {a: await joinedPlane(a, 'A'), b: await joinedPlane(b, 'B')};
  console.log('ok: both pages joined the live plane');

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

  // A call on the page both people share. A starts it from Chat's own call
  // control — the workspace's voice channel, joined muted, as every Tilo call
  // starts — and B, whose channels now list the call with A in it, opens the
  // same call from its own window. Each browser's microphone is its synthetic
  // device, so the pages carry real Opus between them over the live plane:
  // what each page sent and what the other heard is the call's own count.
  // Both read the call's card — who it is with, the run time its title bar
  // draws, and its controls — and hanging up ends it for both.
  const nameA = report.presence.b.label; // who B sees: the person A
  const nameB = report.presence.a.label;
  // The same shared window in front on both desks, so its controls are where
  // a person reaches them.
  await search(a.tab, a.command, 'chat', 'Chat');
  await search(b.tab, b.command, 'chat', 'Chat');
  await until(b.tab, `(() => { globalThis.tiloMount?.requestSemantics?.();
    return JSON.stringify(globalThis.tiloMount?.state()?.semantics ?? {}).includes('Message composer'); })()`,
    "B's chat composer", 180_000);

  // A starts the call the way a person does — the desk's search, Chat's own
  // action — and Chat joins the workspace's voice channel muted, as every
  // Tilo call starts, with the call's card as the window's own reading of it.
  const cardReads = tab => tab.evaluate(`(() => {
    globalThis.tiloMount?.requestSemantics?.();
    const found = [];
    const node = n => { found.push(n); (n.children || []).forEach(node); };
    const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) walk(root);
    return JSON.stringify(found.filter(n => n.value && /^\\d+:\\d{2}$/.test(n.value)) ?? []);
  })()`);
  // The call's card as this page reads it: the person it names and the run
  // time it counts, once the card is there and counting.
  const untilCard = async (person, who, what, timeout = 180_000) => {
    const deadline = Date.now() + timeout;
    let cards = [];
    for (;;) {
      cards = JSON.parse(await cardReads(person.tab));
      const card = cards.find(node => node.name === who);
      if (card) return card;
      if (Date.now() > deadline) {
        await dump(person.tab, `timeout-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
        // Who this page shows as here, by device and by person: a card that
        // cannot name someone says whether their person was known at all.
        const here = await herePeople(person.tab).catch(() => null);
        failed(`${what} does not read ${JSON.stringify(who)} with a run time: ${JSON.stringify(cards)}; here: ${JSON.stringify(here)}`);
      }
      await sleep(300);
    }
  };
  const noCard = async (person, what, timeout = 120_000) => {
    const deadline = Date.now() + timeout;
    for (;;) {
      const cards = JSON.parse(await cardReads(person.tab));
      if (!cards.length) return;
      if (Date.now() > deadline) {
        await dump(person.tab, `timeout-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
        failed(`${what}: the call's card still reads ${JSON.stringify(cards)}`);
      }
      await sleep(300);
    }
  };
  const untilLive = async (person, what, timeout = 180_000) => {
    const deadline = Date.now() + timeout;
    let call = null;
    for (;;) {
      call = await callOn(person.tab);
      if (call?.phase === 'live') return call;
      if (Date.now() > deadline) {
        await dump(person.tab, `timeout-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
        failed(`${what}: the call never went live (${JSON.stringify(call)})`);
      }
      await sleep(300);
    }
  };
  // Both microphones running: the page's audio session captures the
  // browser's synthetic device, which is what puts frames on the plane.
  const untilCapture = async (person, what, timeout = 120_000) => {
    const deadline = Date.now() + timeout;
    let call = null;
    for (;;) {
      call = await callOn(person.tab);
      if (call?.phase === 'live' && call.captureActive) return call;
      if (Date.now() > deadline) {
        await dump(person.tab, `timeout-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
        failed(`${what}: the microphone never started (${JSON.stringify(call)})`);
      }
      await sleep(300);
    }
  };
  // The call this page takes part in, once it is over.
  const untilNotLive = async (person, what, timeout = 120_000) => {
    const deadline = Date.now() + timeout;
    let call = null;
    for (;;) {
      call = await callOn(person.tab);
      if (call?.phase !== 'live') return call;
      if (Date.now() > deadline) failed(`${what}: the call is still live (${JSON.stringify(call)})`);
      await sleep(300);
    }
  };
  // What `sender`'s microphone published and `listener` heard of it: one
  // count per page, the frames the plane carried between them.
  const flows = async (sender, listener, what, timeout = 180_000) => {
    const deadline = Date.now() + timeout;
    let observed = null;
    for (;;) {
      const sent = await callOn(sender.tab);
      const heard = await callOn(listener.tab);
      observed = {sent: sent?.counters ?? null, heard: heard?.counters ?? null};
      if ((sent?.counters?.frames ?? 0) > 20 && (heard?.counters?.heard ?? 0) > 20) return observed;
      if (Date.now() > deadline) {
        await dump(sender.tab, `timeout-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
        await dump(listener.tab, `timeout-hearing-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
        failed(`${what}: no audio crossed (${JSON.stringify(observed)})`);
      }
      await sleep(500);
    }
  };

  await search(a.tab, a.command, 'start a call', 'Start a call');
  const startedA = await untilCard(a, 'Just you', "A's call card, alone in the call");
  console.log('ok: A started a call from Chat');

  // B is rung: the workspace's voice channel, in B's own channel list, now
  // names A in it.
  const untilChannelPeople = async (person, channel, who, what, timeout = 180_000) => {
    const deadline = Date.now() + timeout;
    let people = null;
    for (;;) {
      await person.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
      people = JSON.parse(await rowsUnder(person.tab, channel));
      if (people && people.includes(who)) return people;
      if (Date.now() > deadline) {
        await dump(person.tab, `timeout-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
        failed(`${what}: ${channel} lists ${JSON.stringify(people)}`);
      }
      await sleep(300);
    }
  };
  const ring = await untilChannelPeople(b, 'voice', nameA, "the call's channel in B");
  console.log(`ok: B is rung: voice lists ${ring.join(', ')}`);
  // Nobody is left in the channel: what each desk sees once the call ended.
  const untilChannelEmpty = async (person, channel, what, timeout = 120_000) => {
    const deadline = Date.now() + timeout;
    let people = null;
    for (;;) {
      await person.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
      people = JSON.parse(await rowsUnder(person.tab, channel));
      if (!people || !people.length) return;
      if (Date.now() > deadline) {
        await dump(person.tab, `timeout-${what.replace(/[^a-z0-9]+/gi, '-')}`).catch(() => {});
        failed(`${what}: ${channel} still lists ${JSON.stringify(people)}`);
      }
      await sleep(300);
    }
  };

  // B opens the same call the same way: joining a call that is on is
  // opening it.
  await search(b.tab, b.command, 'start a call', 'Start a call');
  const liveA = await untilLive(a, "A's call going live");
  const liveB = await untilLive(b, "B's call going live");
  console.log('ok: both pages are in the call');
  // Unmuted, both: each page's microphone is the browser's synthetic device,
  // so unmuting is what puts audio on the plane.
  await pressWhen(a.tab, 'Unmute', "A's microphone control");
  await pressWhen(b.tab, 'Unmute', "B's microphone control");
  await untilCapture(a, "A's microphone running");
  await untilCapture(b, "B's microphone running");
  console.log('ok: both microphones are running');

  // The call's card names the other person, counts, and offers the way out:
  // on each page the same reading of the same call.
  const cardA = await untilCard(a, nameB, "A's call card");
  const cardB = await untilCard(b, nameA, "B's call card");
  for (const [who, person, card] of [['A', a, cardA], ['B', b, cardB]]) {
    if (!/^\d+:\d{2}$/.test(card.value ?? '')) failed(`${who}'s call card shows no run time: ${JSON.stringify(card)}`);
    if (card.description !== 'Connected') failed(`${who}'s call card is not connected: ${JSON.stringify(card.description)}`);
    // The card's hang-up is drawn; the mute control is what unmuting left.
    if (!JSON.parse(await find(person.tab, 'Mute'))) failed(`${who}'s call card offers no mute control`);
    if (!JSON.parse(await find(person.tab, 'Leave call'))) failed(`${who}'s call card offers no hang-up control`);
  }
  report.call = {startedA, cardA, cardB, ring};

  const fromA = await flows({tab: a.tab}, {tab: b.tab}, "A's audio in B");
  const fromB = await flows({tab: b.tab}, {tab: a.tab}, "B's audio in A");
  report.call.audio = {fromA, fromB};
  report.call.live = {a: liveA, b: liveB};
  console.log(`ok: each page sent audio the other heard (${JSON.stringify(report.call.audio)})`);
  await dump(a.tab, '6-a-call');
  await dump(b.tab, '6-b-call');

  // A hangs up: A's card goes, and B's call has A no longer in it.
  await pressWhen(a.tab, 'Leave call', "A's hang-up control");
  await noCard(a, "A's call card after hanging up");
  await untilNotLive(a, 'A hanging up');
  await untilCard(b, 'Just you', "B's call card after A hung up");
  console.log('ok: A hung up, and B is alone in the call');
  await dump(a.tab, '7-a-hung-up');

  // B hangs up too: the call is over on both desks, and no one is in the
  // channel's call any more.
  await pressWhen(b.tab, 'Leave call', "B's hang-up control");
  await noCard(b, "B's call card after hanging up");
  await untilNotLive(b, 'B hanging up');
  await untilChannelEmpty(a, 'voice', "A's voice channel after the call");
  await untilChannelEmpty(b, 'voice', "B's voice channel after the call");
  console.log('ok: hanging up ended the call for both');
  await dump(b.tab, '7-b-hung-up');
  // The desk as a laptop drives it: the launcher's search opens every app the
  // Lounge offers, two fingers sideways pan the strip over an app that does
  // not scroll that way and scroll the app over one that does, a pinch is no
  // pan, and the window menu, a right-click menu, Report a problem and the
  // search chord are the desk's own. Nothing any of it does reaches the
  // browser: no page scroll, zoom or history.
  const nav = tab => tab.evaluate('JSON.stringify(globalThis.tiloMount?.navigation?.getSnapshot?.() ?? null)').then(JSON.parse);
  const panOf = async tab => (await nav(tab))?.desktopPan ?? null;
  // Where the strip comes to rest: the same place on three reads a few frames apart.
  const restingPan = async tab => {
    let last = null;
    let same = 0;
    for (let reads = 0; reads < 100 && same < 2; reads++) {
      await settle(tab, 2);
      const now = await panOf(tab);
      same = now && last && now.offset === last.offset ? same + 1 : 0;
      last = now;
    }
    return last;
  };
  const namesOf = async tab => {
    await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    await settle(tab, 1);
    return (await snapshot(tab)).names;
  };
  const choose = async (tab, pattern) => {
    const name = (await namesOf(tab)).find(candidate => pattern.test(candidate));
    if (!name) return null;
    return await press(tab, name) ? name : null;
  };
  const picture = async tab => (await tab.screenshot())?.toString('base64') ?? '';
  const wheel = (tab, x, y, {deltaX = 0, deltaY = 0, modifiers = 0} = {}) =>
    tab.send('Input.dispatchMouseEvent',
      {type: 'mouseWheel', x, y, deltaX, deltaY, ...(modifiers ? {modifiers} : {})});
  // One gesture: a trackpad's fingers send their travel many events per
  // frame, and the desk gives a gesture to its first event's answer, so the
  // events go in together and only the gesture's effect is waited for.
  const sidewaysSwipe = async (tab, x, y, forward = true, during = null) => {
    // The pointer rests where the fingers are, as a person's does: a wheel
    // says where it is, and the desk routes it to that place.
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
    await settle(tab, 1);
    const steps = [6, 12, 18, 24, 18, 12, 6];
    for (const [index, step] of steps.entries()) {
      await wheel(tab, x, y, {deltaX: forward ? step : -step});
      // What the desk shows while the fingers are still on it, not only where
      // the gesture left it: a pause mid-gesture, one frame drawn, the rest.
      if (during && index === 2) {
        await settle(tab, 1);
        await during();
      }
    }
    await settle(tab, 2);
  };
  const rightClick = async (tab, x, y) => {
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
    for (const type of ['mousePressed', 'mouseReleased']) {
      await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'right', clickCount: 1});
    }
    await settle(tab, 1);
  };
  const centreOf = async tab => {
    const corner = JSON.parse(await canvasTopLeft(tab));
    return {x: corner.x + Math.round(corner.width / 2), y: corner.y + Math.round(corner.height / 2), corner};
  };
  const history = await a.tab.evaluate('history.length');

  // The window menu, from a secondary click in the window's own bar: what the
  // desktop can do to that window, and choosing a row does it. Asked before
  // any app opens or the strip is swiped, so the menu is tested whatever those
  // steps find, and asked twice, the second row giving back what the first
  // took, so the desk the later steps meet is the one they would have met.
  // A window bar on screen: with windows the strip scrolled out of view, the
  // first Maximize in the tree can be one of theirs.
  const shownRow = name => a.tab.evaluate(`(() => {
    const found = [];
    const box = b => Array.isArray(b) ? b : [b.x0 ?? b.x, b.y0 ?? b.y, b.x1 ?? b.x + b.width, b.y1 ?? b.y + b.height];
    const node = n => { const b = n.bounds ?? n.rect; if (n.name === ${JSON.stringify(name)} && b) found.push(box(b)); (n.children || []).forEach(node); };
    const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) walk(root);
    const r = globalThis.tiloMount.canvas.getBoundingClientRect();
    const shown = found.find(([x0, y0, x1, y1]) => x0 >= 0 && y0 >= 0 && x1 <= r.width && y1 <= r.height);
    return JSON.stringify(shown ? {bounds: shown} : null);
  })()`).then(JSON.parse);
  // Right-click a window's bar and choose the menu's strip row: the row it
  // offered. The menu acts on the window whose bar it was opened from, and its
  // strip row says that window's state ("Fill the strip" while it does not,
  // "Give the strip back" while it does), so the row offered the second time
  // says whether the first choice did anything.
  const stripRow = async (what, bar) => {
    await rightClick(a.tab, bar.x, bar.y);
    // The menu is the frame after the click's, and a busy desk draws a few a
    // second: its rows are read until they are there.
    let row = null;
    for (const deadline = Date.now() + 15_000; !row && Date.now() < deadline;) {
      row = await choose(a.tab, /^(Fill the strip|Give the strip back)$/);
    }
    if (!row) {
      failed(`${what}: a secondary click in the window bar opens no window menu: at ${JSON.stringify(bar)} over ${JSON.stringify(await namesAt(a.tab, bar.x, bar.y))}; shown: ${JSON.stringify((await namesOf(a.tab)).slice(0, 40))}`);
    }
    await settle(a.tab, 1);
    // A row the click took closes the menu: still listed, the click never reached it.
    const open = (await namesOf(a.tab)).some(name => /^(Fill the strip|Give the strip back)$/.test(name));
    return {row, open};
  };
  const desk = await centreOf(a.tab);
  const barRow = await shownRow('Maximize') ?? await shownRow('Close');
  if (!barRow) failed(`no window bar on screen to open the window menu from: ${JSON.stringify((await namesOf(a.tab)).slice(0, 40))}`);
  const [bx0, by0, bx1, by1] = barRow.bounds;
  const bar = {x: desk.corner.x + (bx0 + bx1) / 2, y: desk.corner.y + (by0 + by1) / 2};
  const first = await stripRow('the window menu', bar);
  const second = await stripRow('the window menu again', bar);
  const [filled, given] = [first.row, second.row];
  if (given === filled) failed(`the window menu's "${filled}" changed nothing: the menu offered it again ${JSON.stringify({bar, openAfterClick: first.open})}`);
  report.windowMenu = {filled, given};
  console.log(`ok: the window menu opens from the window bar, and its rows fill the strip and give it back (${filled}, then ${given})`);

  // Every app the Lounge offers opens from the launcher's search.
  const offered = (await nav(a.tab))?.applications ?? [];
  const applications = brief ? offered.filter(application => BRIEF_APPS.includes(application.label)) : offered;
  if (!applications.length) failed('the Lounge offers no app');
  const opened = [];
  // What a person waits for: Enter on the app's row, to the desk showing its
  // window. Opening an app is a window on the desk the person already has;
  // the app's own code arriving is that window filling in, and it must not
  // hold the window back. The desk draws the window on the frame after the one
  // that took the key, and that is the whole wait: past it the desk reads as
  // not having heard the person. How long that frame took to reach the screen
  // is this machine's rasteriser, so it is reported and not held against the
  // desk.
  const OPEN_FRAMES = 1;
  const timings = [];
  const started = Date.now();
  for (const application of applications) {
    await search(a.tab, a.command, application.label.toLowerCase(), application.label, application.id);
    await until(a.tab, `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === ${JSON.stringify(application.id)})`,
      `the window of ${application.label}`, 180_000);
    const marks = await openMarks(a.tab);
    // An open the page's own clock never saw begun or never saw end is not a
    // fast one: it is no reading, and the gate says so instead of counting a
    // missing number as zero frames.
    if (!marks.ok) {
      failed(`${application.label} opened, but Enter to its window was not measured (${JSON.stringify(marks)}); an unmeasured open is not a fast one`, a.tab);
    }
    timings.push({app: application.id, ...marks});
    const shot = await a.tab.screenshot();
    if (shot) writeFileSync(`${out}/app-${application.id.replace(/[^a-z0-9]+/gi, '-')}.png`, shot);
    opened.push(application.id);
  }
  const spent = Math.round((Date.now() - started) / 1000);
  report.applications = {offered: applications.map(application => application.id), opened, seconds: spent, presented: timings};
  const slow = timings.filter(timing => timing.frames > OPEN_FRAMES);
  console.log(`ok: each of the ${applications.length} apps the Lounge offers opened from the search (${spent}s)`);
  for (const timing of timings) {
    console.log(`  ${timing.app}: ${timing.presentMs === null ? 'on the frame that took its key' : `${timing.presentMs} ms from Enter to its window`}`);
  }
  if (slow.length) {
    failed(`${slow.length} of ${timings.length} apps showed their window more than ${OPEN_FRAMES} frame after Enter: ${slow.map(timing => `${timing.app} ${timing.frames}`).join(', ')}`);
  }

  // Two fingers sideways over Notes, which does not scroll that way: the
  // strip pans by the gesture and the windows stay as they were.
  const centre = await centreOf(a.tab);
  await search(a.tab, a.command, 'notes', 'Notes');
  await settle(a.tab, 2);
  // Where Notes' own window is once the strip is at rest: with a few windows
  // open the middle of the desk can be another window's, one whose content
  // scrolls sideways itself.
  const keptNotes = await restingPan(a.tab);
  const notesButton = await notesPoint(a.tab);
  if (!notesButton) failed('Notes shows no New note button to find its window by');
  // In Notes' column, at the desk's middle height: its body, not the window's
  // bar, whose row of tabs scrolls sideways itself.
  const overNotes = {x: notesButton.x, y: centre.y};
  const windowsBefore = (await snapshot(a.tab)).windows;
  const panBefore = keptNotes ?? await panOf(a.tab);
  if (!panBefore) failed('the strip does not overflow, so nothing can pan');
  // The way the strip still has room: a gesture at its end pans nothing.
  const forward = panBefore.offset < panBefore.max / 2;
  const frameBefore = await picture(a.tab);
  await sidewaysSwipe(a.tab, overNotes.x, overNotes.y, forward, async () => {
    const midScroll = await a.tab.screenshot();
    if (midScroll) writeFileSync(`${out}/strip-mid-scroll.png`, midScroll);
  });
  // Where the swipe left the strip, once it has come to rest: a reveal still
  // sliding is no pan.
  const panAfter = await restingPan(a.tab);
  const moved = panAfter.offset - panBefore.offset;
  report.sideways = {forward, before: panBefore, after: panAfter, at: overNotes, under: await namesAt(a.tab, overNotes.x, overNotes.y)};
  if (Math.abs(moved) < 1) {
    failed(`two fingers sideways do not pan the strip over Notes: ${JSON.stringify(report.sideways)}`, a.tab);
  }
  if (Math.sign(moved) !== (forward ? 1 : -1)) {
    failed(`two fingers sideways panned the strip against the fingers: ${JSON.stringify(report.sideways)}`);
  }
  if ((await snapshot(a.tab)).windows.join() !== windowsBefore.join()) failed('a sideways pan changed the windows');
  if (await picture(a.tab) === frameBefore) failed('the strip did not repaint as it panned');
  console.log(`ok: two fingers sideways pan the strip over Notes (${panBefore.offset} to ${panAfter.offset})`);

  // The same gesture over Sheets, whose grid scrolls sideways itself: the app
  // takes it and the strip stays where it was.
  // The fingers rest on Sheets' grid, wherever on the strip the search
  // brought its window: the middle of the desk can be another window's then,
  // whose sideways wheel the strip takes, as it should.
  // The search slides the strip to Sheets: the swipe waits until it is at rest.
  await search(a.tab, a.command, 'sheets', 'Sheets');
  await settle(a.tab, 2);
  // Where B2 is, read once the strip is at rest: read while it still slides,
  // the fingers would land wherever B2 was, on another window by then.
  const keptPan = await restingPan(a.tab);
  const cell = await sheetCell(a.tab);
  if (!cell) failed('Sheets shows no cell B2 to swipe over');
  const sheetBefore = await picture(a.tab);
  // Toward the later columns: a sheet opens on its first one, so that is where
  // its room is. Which way the strip has room says nothing about the grid —
  // a swipe the columns cannot follow is the strip's, and would pan it.
  await sidewaysSwipe(a.tab, cell.x, cell.y, true);
  const sheetPan = await panOf(a.tab);
  report.sheets = {before: keptPan, after: sheetPan, cell, underCentre: await namesAt(a.tab, centre.x, centre.y)};
  if ((sheetPan?.offset ?? 0) !== (keptPan?.offset ?? 0)) {
    failed(`the strip panned under a sideways wheel Sheets scrolled with: ${JSON.stringify(report.sheets)}`, a.tab);
  }
  if (await picture(a.tab) === sheetBefore) failed('Sheets did not scroll sideways with the fingers', a.tab);
  console.log('ok: the same gesture scrolls Sheets and leaves the strip');

  let invited = null;
  if (!brief) {
    // A pinch, which a browser sends as Ctrl+wheel, is the window's zoom and
    // never a pan, with or without a sideways part.
    const beforePinch = await panOf(a.tab);
    for (let i = 0; i < 6; i++) await wheel(a.tab, centre.x, centre.y, {deltaY: -40, modifiers: 2});
    await settle(a.tab, 2);
    for (let i = 0; i < 6; i++) await wheel(a.tab, centre.x, centre.y, {deltaX: 10, deltaY: -20, modifiers: 2});
    await settle(a.tab, 2);
    const afterPinch = await panOf(a.tab);
    report.pinch = {before: beforePinch, after: afterPinch};
    if ((afterPinch?.offset ?? 0) !== (beforePinch?.offset ?? 0)) {
      failed(`a pinch moved the strip: ${JSON.stringify(report.pinch)}`);
    }
    console.log('ok: a pinch zooms and never pans the strip');

    // A right-click in the chat's composer is the text's menu.
    await search(a.tab, a.command, 'chat', 'Chat');
    await settle(a.tab, 2);
    const composer = JSON.parse(await find(a.tab, 'Message composer'));
    if (!composer) failed('the chat has no composer to right-click');
    const [cx0, cy0, cx1, cy1] = Array.isArray(composer.bounds)
      ? composer.bounds
      : [composer.bounds.x0 ?? composer.bounds.x, composer.bounds.y0 ?? composer.bounds.y,
         composer.bounds.x1 ?? composer.bounds.x + composer.bounds.width,
         composer.bounds.y1 ?? composer.bounds.y + composer.bounds.height];
    await rightClick(a.tab, centre.corner.x + (cx0 + cx1) / 2, centre.corner.y + (cy0 + cy1) / 2);
    const textRow = (await namesOf(a.tab)).find(name => /^(Paste|Paste and match style|Copy|Cut|Select all)/) ?? null;
    if (!textRow) failed('a right-click in a text field offers no text menu');
    report.contextMenu = textRow;
    console.log(`ok: a right-click in a text field offers the text menu (${textRow})`);
    await key(a.tab, 'Escape');

    // Report a problem: the bar's own avatar, then the card's row.
    await press(a.tab, (await namesOf(a.tab)).find(name => /\(you\)/.test(name)) ?? 'Profile');
    const reportRow = await choose(a.tab, /^Report a problem/);
    if (!reportRow) failed('the profile card offers no Report a problem');
    // Picking what to report takes every event: nothing under the pointer
    // reacts to it, and the strip the desk shows stays where it was.
    await settle(a.tab, 2);
    const pickingPan = await panOf(a.tab);
    await sidewaysSwipe(a.tab, centre.x, centre.y, true);
    if ((await panOf(a.tab))?.offset !== pickingPan?.offset) {
      failed('a sideways wheel panned the strip under the report picker');
    }
    report.reportProblem = reportRow;
    console.log(`ok: Report a problem opens its picker (${reportRow})`);
    await key(a.tab, 'Escape');

    // The launcher's chord opens the search, and Escape closes it opening
    // nothing: the keys a browser leaves to the page.
    await click(a.tab, 640, 700);
    await key(a.tab, 'k', a.command);
    await until(a.tab, '!!globalThis.tiloMount?.state()?.searchOpen', 'the search chord', 30_000);
    await key(a.tab, 'Escape');
    await until(a.tab, '!globalThis.tiloMount?.state()?.searchOpen', 'the search closing', 30_000);
    console.log('ok: the search chord opens the search and Escape closes it');

    // None of it reached the browser.
    const browser = {
      scrollY: await a.tab.evaluate('window.scrollY'),
      scrollX: await a.tab.evaluate('window.scrollX'),
      scale: await a.tab.evaluate('globalThis.visualViewport?.scale ?? 1'),
      history: await a.tab.evaluate('history.length'),
    };
    report.browser = {...browser, historyAtArrival: history};
    if (browser.scrollY !== 0 || browser.scrollX !== 0) failed(`the page scrolled: ${JSON.stringify(browser)}`);
    if (Math.abs(browser.scale - 1) > 0.001) failed(`the page zoomed: ${JSON.stringify(browser)}`);
    if (browser.history !== history) failed(`the page navigated: ${JSON.stringify(browser)}`);
    console.log('ok: no wheel, key or click reached the browser');

    // A person the workspace lets invite, from the page itself: the workspace
    // declares them one while their page stands open (the roles file the
    // deployment mounts, which the Lounge takes in again as it changes), and
    // the bar offers inviting without a reload, as a promotion reaches the
    // native desk.
    const offeredBefore = (await snapshot(a.tab)).names.filter(name => name === 'Invite');
    report.inviteBefore = offeredBefore.length;
    if (offeredBefore.length) await failed('the bar offered inviting before the workspace allowed it', a.tab);
    writeFileSync(rolesFile, JSON.stringify({owners: [keptA.userPublicKey], moderators: []}), {mode: 0o644});
    const allowedAt = Date.now();
    await waitFor(a.tab, 'Invite', "the bar's invite button");
    report.inviteAfterMs = Date.now() - allowedAt;
    console.log(`ok: the page offers inviting ${report.inviteAfterMs} ms after the workspace allowed it, with no reload`);

    // The page's own sheet, the same one a desk shows: the role a link brings
    // someone in at, chosen before the link is made.
    if (!await press(a.tab, 'Invite')) await failed("the bar's invite button could not be pressed", a.tab);
    await waitFor(a.tab, 'Invite people', 'the invite sheet');
    if (!await press(a.tab, 'Commenter')) await failed('the invite sheet offers no Commenter part', a.tab);
    await until(a.tab, `JSON.stringify(globalThis.tiloMount?.state()?.semantics ?? {}).includes('add comments, but cannot edit it')`,
      "the sheet's chosen role under the choice", 60_000);
    if (!await press(a.tab, 'Create link')) await failed('the invite sheet offers no Create link', a.tab);
    const made = await (async () => {
      const deadline = Date.now() + 120_000;
      for (;;) {
        const link = (await nodeValues(a.tab)).find(value => sealedLink.test(value));
        if (link) return link;
        if (Date.now() > deadline) {
          const handed = (await nodeValues(a.tab)).filter(value => /^https?:/.test(value));
          await failed(`the sheet handed over no short link: ${JSON.stringify(handed)}`, a.tab);
        }
        await sleep(250);
      }
    })();
    report.link = made;
    if (new URL(made).origin !== new URL(site).origin) await failed(`the link is not at this site: ${made}`, a.tab);
    const long = (await nodeValues(a.tab)).find(value => typeof value === 'string' && value.includes('invite='));
    if (long) await failed(`the sheet shows the invitation's own text beside the link: ${long.slice(0, 120)}`, a.tab);
    console.log(`ok: the sheet made a link a site keeps: handle in the path, key in the fragment (${made})`);
    await dump(a.tab, '8-a-invite-sheet');

    // The workspace as the invitation describes it, before this person joins:
    // the name and how many people it has, which the host counts itself.
    const listedNow = (await directory()).workspaces?.find(workspace => workspace.room === keptA.room);
    if (!listedNow) await failed('the directory stopped listing the workspace', a.tab);
    const ownName = (await snapshot(a.tab)).names
      .find(name => / \(you\): your profile$/.test(name))?.replace(/ \(you\): your profile$/, '') ?? null;
    if (!ownName) await failed("A's page names no person of its own", a.tab);
    report.inviter = ownName;

    // Someone else opens the link the sheet handed over, in a browser of their
    // own: the page says who invited them, to what, how many people are in it
    // and what the part lets them do — all from the host's verification of the
    // invitation, not from the link alone.
    const c = await followLink('C', made);
    invited = c;
    const people = listedNow.members === 1 ? '1 person' : `${listedNow.members} people`;
    for (const expected of [`${ownName} invites you to`, listedNow.name, people,
      'Commenter — you can add comments, but not edit shared work.']) {
      if (!c.card.includes(expected)) {
        await failed(`the invitation card does not say "${expected}": ${c.card.slice(0, 400)}`, c.tab);
      }
    }
    if (!c.buttons.includes('Join') || !c.buttons.includes('Not now')) {
      await failed(`the invitation card offers neither Join nor Not now: ${JSON.stringify(c.buttons)}`, c.tab);
    }
    report.card = c.card;
    console.log(`ok: the link says who invited them, where and as what: ${c.card}`);
    await dump(c.tab, '9-c-invitation-card');

    // Joining is the whole of it: the invitation is its inviter's approval, so
    // no one in the workspace is offered a chance to let this person in while
    // they arrive.
    const pressed = await c.tab.evaluate(`(() => {
      const card = [...document.querySelectorAll('form.tilo-entry-card')].at(-1);
      const button = card && [...card.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
      if (button) button.click();
      return !!button;
    })()`);
    if (!pressed) await failed('the invitation card went away before Join', c.tab);
    const arrivalDeadline = Date.now() + 240_000;
    let offered = null;
    for (;;) {
      offered ??= (await snapshot(a.tab)).names.find(name => /^Let .+ in$/.test(name)) ?? null;
      const arrival = await c.tab.evaluate(`document.documentElement.dataset.arrival ?? null`).catch(() => null);
      if (arrival === 'done') break;
      if (arrival === 'stopped') await failed('the invitation did not take: the page stopped', c.tab);
      if (Date.now() > arrivalDeadline) await failed('the invitation arriving', c.tab);
    }
    if (offered) {
      await failed(`the page asked its person to let the invitation's person in (${offered}); an invitation is its inviter's approval`, a.tab);
    }
    await mounted(c.tab, 'C');
    const keptC = JSON.parse(await membership(c.tab));
    report.invited = keptC;
    if (!keptC?.userPublicKey || keptC.room !== keptA.room) {
      await failed(`the invitation's person joined ${JSON.stringify(keptC)}, not ${keptA.room}`, c.tab);
    }
    if (keptC.userPublicKey === keptA.userPublicKey || keptC.userPublicKey === keptB.userPublicKey) {
      await failed("the invitation's person is someone who was already here", c.tab);
    }
    console.log(`ok: ${keptC.userPublicKey.slice(0, 8)} arrived from the link in A's workspace, with no one letting them in`);
    await dump(c.tab, '10-c-arrived');

    // The desk each person is in is the desk the new person is on too: A's
    // People list names them beside B.
    const rowsAfter = await (async () => {
      const deadline = Date.now() + 120_000;
      for (;;) {
        const rows = await herePeople(a.tab);
        if (Array.isArray(rows) && rows.length === 2) return rows;
        if (Date.now() > deadline) {
          await dump(a.tab, 'timeout-people-invited');
          await failed(`A's desk does not show the invitation's person beside B: ${JSON.stringify(rows)}`, a.tab);
        }
        await sleep(500);
      }
    })();
    report.hereAfter = rowsAfter.map(row => ({id: row.id, user: row.user ?? null, label: row.label}));
    const invitedRow = rowsAfter.find(row => row.id !== report.presence.a.id);
    if (!invitedRow) await failed('the two rows A sees are one person', a.tab);
    if (invitedRow.user && invitedRow.user !== keptC.userPublicKey) {
      await failed(`A's desk shows the invitation's arrival as ${invitedRow.user}`, a.tab);
    }
    console.log(`ok: A's desk shows ${invitedRow.label} beside B`);
    await dump(a.tab, '11-a-people-invited');
  }

  // Nothing failed in either page.
  const both = [...a.tab.log, ...b.tab.log, ...(invited?.tab.log ?? [])];
  const errors = both.filter(line => /^EXC/.test(line) || /panic/i.test(line));
  report.logs = {a: a.tab.log.slice(-10), b: b.tab.log.slice(-10), c: invited?.tab.log.slice(-10) ?? null};
  if (errors.length) failed(`pages reported errors: ${errors.slice(0, 5).join('; ')}`);

  // The desk one person arranged is the desk the other shows: neither page's
  // write of the shared desktop was refused.
  const refused = both.filter(line => /the workspace refused a change/.test(line));
  report.refused = refused;
  if (refused.length) failed(`a page's change was refused: ${refused.slice(0, 5).join('; ')}`);
  // A frame repainted in part must leave no stale pixel. A page built with the
  // retained-frame check says so where it happens; the check also says when it
  // could not read its two frames back. A stale pixel on either page fails the
  // run: the desk a person sees is the frame that repaint left behind.
  report.mismatched = both.filter(line => /retained frame differs from a full render/.test(line));
  report.unreadable = both.filter(line => /could not read a frame back/.test(line));
  if (report.mismatched.length) {
    failed(`a page showed a stale frame: ${report.mismatched.slice(0, 3).join('; ')}`);
  }

  writeFileSync(`${out}/report.json`, JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
} catch (error) {
  await failed(String(error?.stack ?? error));
} finally {
  for (const browser of browsers) await browser.close().catch(error => console.error('close:', error.message));
}
process.exit(0);
