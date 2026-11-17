// Chat's "Copy link" on the web desktop, as a person presses it. A note shared
// into a channel carries "Copy link" in its message's menu. Choosing it asks
// the note's own app for its link through the page's session (an action asked
// of the note's record), and Chat says what that app answered: the link was
// copied, or the app's own reason there is none. It never says that
// nothing offers the action, which is what a page's session answered before
// it carried an action asked of a record to the window showing that record.
//
// This page has no workspace host: the Notes window that shows the note
// answers. A page with one hands the same action to its host.
//
// usage: node copy-link.mjs BUNDLE_DIR [OUT_DIR]
//   BUNDLE_DIR  build.sh --bundle desktop output
//   OUT_DIR     where to keep a picture of the message and its control
// Exits non-zero on the first failed expectation, saying what the page showed.
import assert from 'node:assert/strict';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';
import {verb} from '../../tilo-browser-runtime/web/desktop-navigation.mjs';

const [bundle, out] = process.argv.slice(2);
if (!bundle) throw Error('usage: node copy-link.mjs BUNDLE_DIR [OUT_DIR]');
if (out) mkdirSync(out, {recursive: true});
const server = await serve(resolve(bundle));
const browser = await launch();
const tab = await browser.tab();
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
// The control copies inside the press: the page may write the clipboard.
await tab.send('Browser.grantPermissions', {origin: server.url, permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
await tab.navigate(`${server.url}/`);
const {evaluate} = tab;

const said = step => console.log(JSON.stringify(step));
let failure = null;
try {
  await run();
} catch (error) {
  failure = error;
  const shot = out && await tab.screenshot().catch(() => null);
  if (shot) writeFileSync(join(out, 'copy-link-failed.png'), shot);
} finally {
  await browser.close().catch(() => {});
  await server.close().catch(() => {});
}
if (failure) {
  console.error(failure.stack ?? String(failure));
  process.exit(1);
}

async function run() {
  for (let i = 0; ; i++) {
    await sleep(1000);
    if (await evaluate('Boolean(globalThis.tiloMount?.navigation)').catch(() => false)) break;
    assert.ok(i < 90, `the desktop never presented: ${await evaluate("document.getElementById('status')?.textContent")}`);
  }
  const snapshot = () => evaluate('tiloMount.navigation.getSnapshot()');
  const command = action => evaluate(`(async()=>{await tiloMount.navigation.dispatch(${JSON.stringify(action)});return true;})()`);
  const windowsOf = async app => (await snapshot()).entries.filter(entry => entry.app === app);
  // Every drawn semantic node: its role, its name and where it is on the page.
  const nodes = () => evaluate(`(async()=>{
    tiloMount.requestSemantics();
    await new Promise(done => setTimeout(done, 0));
    const state = tiloMount.state(), r = tiloMount.canvas.getBoundingClientRect(), size = state.logicalSize ?? [r.width, r.height];
    const sx = r.width / size[0], sy = r.height / size[1], found = [];
    const walk = (n, rect) => {
      if (!n || typeof n !== 'object') return;
      const s = n.semantics ?? n;
      if (s.state?.hidden) return;
      const own = n.rect ?? rect, b = s.bounds ? [s.bounds.x0, s.bounds.y0, s.bounds.x1 - s.bounds.x0, s.bounds.y1 - s.bounds.y0] : own;
      if (b) found.push({role: typeof s.role === 'string' ? s.role : JSON.stringify(s.role ?? null), name: s.name ?? null,
        x: r.left + b[0] * sx, y: r.top + b[1] * sy, w: b[2] * sx, h: b[3] * sy});
      for (const child of n.children ?? []) walk(child, own);
      if (n.semantics) for (const child of n.semantics.children ?? []) walk(child, own);
    };
    walk(state.semantics?.root ?? state.semantics, null);
    return found;
  })()`);
  const names = async () => [...new Set((await nodes()).map(n => n.name).filter(Boolean))];
  // Every resource the windows publish that offers an action: its id, kind and label, and the
  // actions it offers. The page lists one entry for each action, at its resource.
  const published = async () => {
    const found = new Map();
    for (const offered of await evaluate('tiloMount.publications()')) {
      const id = offered?.address?.resource;
      if (typeof id !== 'string') continue;
      if (!found.has(id)) found.set(id, {id, kind: offered.resource_kind ?? null, label: offered.resource_label ?? null, actions: []});
      found.get(id).actions.push(offered.address.action);
    }
    return [...found.values()];
  };
  // Frames can be far apart in a software-rendered headless browser: wait for
  // the state rather than a fixed time. A step that never comes says what the
  // page published and showed instead.
  async function until(label, condition, tries = 600) {
    for (let i = 0; i < tries; i++) {
      const value = await condition();
      if (value) return value;
      await sleep(100);
    }
    const shown = (await names()).slice(0, 120);
    const offered = (await published()).map(resource => `${resource.kind} ${resource.id} "${resource.label ?? ''}" [${resource.actions.join(',')}]`).slice(0, 80);
    assert.fail(`${label}\nshown: ${JSON.stringify(shown)}\npublished: ${JSON.stringify(offered, null, 1)}\nerror: ${JSON.stringify(await evaluate('tiloMount.state()?.error ?? null'))}`);
  }
  const invoke = async (resource, action, args, window) => JSON.parse(await evaluate(`(async()=>JSON.stringify(await tiloMount.invokeAction(${JSON.stringify(resource)}, ${JSON.stringify(action)}, ${JSON.stringify(args)}, {window: ${JSON.stringify(window)}})))()`));
  const click = async (x, y) => {
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
    for (const type of ['mousePressed', 'mouseReleased'])
      await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
  };
  const press = async node => click(Math.round(node.x + node.w / 2), Math.round(node.y + node.h / 2));
  // The secondary button, as a mouse presses it.
  const pressSecondary = async node => {
    const x = Math.round(node.x + node.w / 2), y = Math.round(node.y + node.h / 2);
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
    for (const type of ['mousePressed', 'mouseReleased'])
      await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'right', buttons: type === 'mousePressed' ? 2 : 0, clickCount: 1});
  };
  const onScreen = async name => {
    const {width, height} = await evaluate('(({width, height}) => ({width, height}))(tiloMount.canvas.getBoundingClientRect())');
    return (await nodes()).find(n => name(n) && n.w > 0 && n.h > 0 && n.x >= 0 && n.y >= 0 && n.x + n.w <= width && n.y + n.h <= height);
  };
  const NOTES = 'so.tilo.notes', CHAT = 'so.tilo.chat';
  const title = `Quarterly plan ${Date.now().toString(36).slice(-4)}`, channel = `plans-${Date.now().toString(36).slice(-4)}`;

  // A note, in Notes' own window, which stays open: it is what answers for the note.
  await command(verb.openApp(NOTES));
  const [notes] = await until('Notes open', async () => { const open = await windowsOf(NOTES); return open.length === 1 && open; });
  const notebook = await until('Notes publishing its notebook', async () => (await published()).find(resource => resource.kind === 'notes.notebook' && resource.actions.includes('new-note')));
  const made = await invoke(notebook.id, 'new-note', {title}, notes.route.tab);
  said({step: 'new-note', answer: made});
  // The note as Notes names it to everyone else: what a message that shares it carries. (What the
  // page lists it under is this window's own handle for it.)
  const noteId = made.output?.value?.resource ?? made.output?.resources?.[0];
  assert.match(String(noteId), /^notes\.notebook\/pages\//, `Notes answers with the note it made: ${JSON.stringify(made)}`);
  const note = await until('the note published', async () => (await published()).find(resource => resource.label === title && resource.kind !== 'notes.notebook'));
  said({step: 'note', kind: note.kind, offers: note.actions.filter(action => /link/.test(action))});

  // Chat, a channel, and the note shared into it.
  await command(verb.openApp(CHAT));
  const [chat] = await until('Chat open', async () => { const open = await windowsOf(CHAT); return open.length === 1 && open; });
  const workspace = await until('Chat publishing its channels', async () => (await published()).find(resource => resource.kind === 'chat.workspace' && resource.actions.includes('create-channel')));
  const opened = await invoke(workspace.id, 'create-channel', {name: channel}, chat.route.tab);
  said({step: 'create-channel', answer: opened});
  // The channel as Chat's own verbs name it: its id, the last part of the resource Chat answers
  // with (`chat.workspace/channels/<id>`).
  const roomId = String(opened.output?.resources?.[0] ?? '').split('/').pop();
  assert.ok(roomId, `Chat answers with the channel it made: ${JSON.stringify(opened)}`);
  const sharing = await until('Chat publishing what shares a resource', async () => (await published()).find(resource => resource.actions.includes('share-resource')));
  said({step: 'share-resource', answer: await invoke(sharing.id, 'share-resource',
    {channel: roomId, attached: {app: NOTES, kind: note.kind, id: noteId, label: title}, text: 'The plan'}, chat.route.tab)});

  // Chat in front, on that channel: the message carries the note's chip, and its menu "Copy link".
  await command(verb.focus(chat.route));
  await until('Chat in front', async () => (await snapshot()).active?.tab === chat.route.tab);
  const room = await until('the channel published', async () => (await published()).find(resource =>
    resource.kind === 'chat.workspace.channels' && (resource.label === channel || resource.label === `#${channel}`)));
  said({step: 'open-channel', answer: await invoke(room.id, 'open-channel', {}, chat.route.tab)});
  const chip = await until('the message showing the shared note', () => onScreen(n => n.name === `Open ${title}`));
  await pressSecondary(chip);
  const copy = await until('the message\'s menu offering "Copy link"', () => onScreen(n => n.name === 'Copy link'));
  if (out) writeFileSync(join(out, 'copy-link-offered.png'), await tab.screenshot());
  await press(copy);
  const told = await until('the control saying what the note\'s app answered', async () =>
    (await names()).find(name => /^Link copied|^Link unavailable/.test(name)));
  if (out) writeFileSync(join(out, 'copy-link.png'), await tab.screenshot());
  const copied = await evaluate('Promise.race([navigator.clipboard.readText().catch(() => null), new Promise(done => setTimeout(() => done(null), 2000))])');
  said({step: 'copy-link', told, copied});

  // The session carried the ask to the note's app: what the control says is that app's answer.
  assert.doesNotMatch(told, /no such action is offered|unknown.action/i, `the page's session knew no such action: ${told}`);
  if (/^Link copied/.test(told)) {
    assert.match(String(copied), /so\.tilo\.notes/, `the clipboard holds the note's link: ${copied}`);
  } else {
    // No host serves this page's workspace, so Notes has no address to give: its own reason.
    assert.match(told, /host|served|web page/i, `the reason is the note's app's own: ${told}`);
  }
  assert.equal((await windowsOf(NOTES)).length, 1, 'asking the note for its link opened no second Notes window');
  assert.equal((await snapshot()).active?.tab, chat.route.tab, 'and Chat stayed in front');
  assert.equal(await evaluate('tiloMount.state()?.error ?? null'), null, 'the desktop kept drawing');
  assert.deepEqual(tab.log.filter(line => line.startsWith('EXC')), [], 'no page exception');
}
