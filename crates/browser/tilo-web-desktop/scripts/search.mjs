// Desktop search in headless Chrome, driven the way a person does: the search
// shortcut, typed queries and Enter, with real DOM key events.
//
// 1. The shortcut opens search and the browser never sees it; `sheets` and
//    Enter opens Sheets.
// 2. `notes` and Enter opens Notes; the "New note" action, found by typing,
//    runs with its argument and makes a note.
// 3. Search finds that note by its title, and Enter opens it.
// 4. An app the web cannot run (Firewall: Tilo OS only; Terminal: Linux and
//    macOS only) is a result that says why, and Enter opens nothing.
//
// usage: node search.mjs BUNDLE_DIR
//   BUNDLE_DIR  build.sh --bundle desktop output
// Writes screenshots to SHOTS_DIR when set. Exits non-zero on any failure.
import {writeFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const [bundle] = process.argv.slice(2);
if (!bundle) throw Error('usage: node search.mjs BUNDLE_DIR');
const shots = process.env.SHOTS_DIR;
const title = `Quarterly plan ${Date.now().toString(36).slice(-4)}`;

const server = await serve(resolve(bundle));
const browser = await launch();
const tab = await browser.tab();
const failures = [];
const check = (ok, what) => {
  console.log(`${ok ? 'ok' : 'FAIL'} ${what}`);
  if (!ok) failures.push(what);
};

const state = () => tab.evaluate('globalThis.tiloMount?.state() ?? null');
const windows = () => tab.evaluate('globalThis.tiloMount.navigation.getSnapshot().entries.map(e => e.label)');
const active = () => tab.evaluate(`(() => {
  const s = globalThis.tiloMount.navigation.getSnapshot();
  return s.entries.find(e => JSON.stringify(e.route) === JSON.stringify(s.active))?.label ?? null;
})()`);
// Every semantic node's name, from the frame's semantic snapshot.
const names = async () => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()');
  await sleep(500);
  return tab.evaluate(`(() => {
    const out = [];
    const node = n => { out.push(n.name ?? ''); (n.children || []).forEach(node); };
    const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) walk(root);
    return out;
  })()`);
};
// The palette's result rows, in order.
const results = async () => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()');
  await sleep(500);
  return tab.evaluate(`(() => {
    let rows = null;
    const find = n => { if (n.widget === 'CommandPalette') rows = n.semantics?.children?.[1]?.children?.map(r => r.name) ?? []; (n.children || []).forEach(find); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) find(root);
    return rows;
  })()`);
};
// The palette's first result row, every semantic field serialized.
const firstRow = async () => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()');
  await sleep(500);
  return tab.evaluate(`(() => {
    let row = null;
    const find = n => { if (n.widget === 'CommandPalette') row = n.semantics?.children?.[1]?.children?.[0] ?? null; (n.children || []).forEach(find); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) find(root);
    return row && {name: row.name, text: JSON.stringify(row)};
  })()`);
};

async function key(name, modifiers = 0) {
  const named = {Enter: 13, Escape: 27, ArrowDown: 40, ArrowUp: 38};
  const code = name.length === 1 ? `Key${name.toUpperCase()}` : name;
  const event = {key: name, code, windowsVirtualKeyCode: named[name] ?? name.toUpperCase().charCodeAt(0), modifiers};
  await tab.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', ...event, ...(name === 'Enter' ? {text: '\r'} : {})});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', ...event});
  await sleep(300);
}

async function type(text) {
  for (const ch of text) {
    await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
  }
  await sleep(600);
}

async function until(what, predicate, timeout = 20000) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await predicate().catch(() => undefined);
    if (value) return value;
    if (Date.now() > deadline) return undefined;
    await sleep(250);
  }
}

try {
  await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
  await tab.navigate(`${server.url}/index.html`);
  await tab.waitFor("/frames [1-9]/.test(document.getElementById('status')?.textContent ?? '')", {timeout: 300000});
  await sleep(2000);
  const mac = await tab.evaluate('/Mac/.test(navigator.platform)');
  // CDP modifier bits: Ctrl 2, Meta 4.
  const command = mac ? 4 : 2;
  // Watch the chord from the window's capture phase, before the page host.
  await tab.evaluate(`globalThis.__chords = []; window.addEventListener('keydown', e => { if (e.key === 'k') globalThis.__chords.push(e); }, {capture: true})`);
  await tab.send('Input.dispatchMouseEvent', {type: 'mousePressed', x: 640, y: 700, button: 'left', clickCount: 1});
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 640, y: 700, button: 'left', clickCount: 1});
  await sleep(500);

  // 1. Shortcut, `sheets`, Enter.
  await key('k', command);
  check(await until('search opens', async () => (await state())?.searchOpen), `${mac ? 'Cmd' : 'Ctrl'}+K opens search`);
  check(await tab.evaluate('globalThis.__chords.length > 0 && globalThis.__chords.every(e => e.defaultPrevented)'), 'the browser does not act on the chord');
  if (shots) writeFileSync(`${shots}/search-open.png`, await tab.screenshot());
  await type('sheets');
  const sheetsRows = await results();
  console.log(`  rows for "sheets": ${JSON.stringify(sheetsRows)}`);
  check(sheetsRows?.[0] === 'Open Sheets', 'the Sheets launch leads the results');
  if (shots) writeFileSync(`${shots}/search-sheets.png`, await tab.screenshot());
  await key('Enter');
  check(await until('Sheets opens', async () => (await active()) === 'Sheets'), 'Enter opens Sheets, focused');
  check(!(await state())?.searchOpen, 'choosing a result closes search');

  // 2. Open Notes, then run its "New note" action from search.
  await key('k', command);
  await until('search opens', async () => (await state())?.searchOpen);
  await type('notes');
  await key('Enter');
  check(await until('Notes opens', async () => (await active()) === 'Notes'), 'search opens Notes');
  await sleep(4000);
  await key('k', command);
  await until('search opens', async () => (await state())?.searchOpen);
  await type('new note');
  const actionRows = await results();
  console.log(`  rows for "new note": ${JSON.stringify(actionRows)}`);
  check(actionRows?.includes('New note'), 'Notes\' "New note" action is a result');
  // The action row, not an application launch that also matches "note".
  const index = actionRows?.indexOf('New note') ?? -1;
  for (let i = 0; i < index; i++) await key('ArrowDown');
  await key('Enter');
  await type(title);
  if (shots) writeFileSync(`${shots}/search-action-argument.png`, await tab.screenshot());
  await key('Enter');
  check(await until('the note exists', async () => (await names()).some(name => name.includes(title))), `the action ran: a note "${title}" exists`);

  // 3. Find that note by its title.
  await key('k', command);
  await until('search opens', async () => (await state())?.searchOpen);
  await type(title.split(' ').slice(0, 2).join(' ').toLowerCase());
  const noteRows = await results();
  console.log(`  rows for the note: ${JSON.stringify(noteRows)}`);
  check(noteRows?.some(row => row === title), 'search finds the note by its title');
  if (shots) writeFileSync(`${shots}/search-note.png`, await tab.screenshot());
  const noteIndex = noteRows?.indexOf(title) ?? -1;
  for (let i = 0; i < noteIndex; i++) await key('ArrowDown');
  await key('Enter');
  check(await until('search closes', async () => !(await state())?.searchOpen), 'Enter on the note closes search');
  check((await active()) === 'Notes', 'the note opens in Notes');
  console.log(`  windows: ${JSON.stringify(await windows())}`);

  // 4. Apps this desktop cannot run are listed with why and open nothing.
  for (const [query, name, reason] of [
    ['firewall', 'Firewall', 'Tilo OS only'],
    ['terminal', 'Terminal', 'Linux and macOS only'],
  ]) {
    const before = await windows();
    await key('k', command);
    await until('search opens', async () => (await state())?.searchOpen);
    await type(query);
    const row = await firstRow();
    console.log(`  first row for "${query}": ${row?.text?.slice(0, 300)}`);
    check(row?.name === name, `${name} is listed on the web`);
    check(row?.text.includes(reason), `the ${name} row says "${reason}"`);
    if (shots) writeFileSync(`${shots}/search-unavailable-${query}.png`, await tab.screenshot());
    await key('Enter');
    await sleep(1000);
    check(JSON.stringify(await windows()) === JSON.stringify(before), `Enter on ${name} opens nothing`);
    if ((await state())?.searchOpen) await key('Escape');
  }
  const errors = tab.log.filter(line => line.startsWith('EXC') || /panic/i.test(line));
  check(errors.length === 0, `no exceptions or panics${errors.length ? `: ${errors[0]}` : ''}`);
} catch (error) {
  failures.push(String(error?.message ?? error));
  console.error(error);
} finally {
  await tab.close().catch(() => {});
  await browser.close();
  await server.close();
}
console.log(failures.length ? `${failures.length} failed` : 'all passed');
process.exit(failures.length ? 1 : 0);
