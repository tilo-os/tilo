// The desktop's own navigation, driven through the browser as a person does
// it: a mouse's sideways wheel steps one window per notch, the way a native
// wheel does; the top bar's arrows switch workspaces; the desktop shortcuts
// move between workspaces and windows even while an app's text field holds
// the keyboard. Every input is a trusted event from Chrome's input pipeline.
//
// usage: node navigation.mjs BUNDLE_DIR
//   BUNDLE_DIR  build.sh --bundle desktop output
//   SHOTS_DIR   where a screenshot of a failed step goes, when set
// Exits non-zero on the first failed expectation.
import assert from 'node:assert/strict';
import {existsSync, readFileSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const bundle = process.argv[2];
if (!bundle) throw Error('usage: node navigation.mjs BUNDLE_DIR');
const server = await serve(resolve(bundle));
// Software Vulkan, as the CI mount test runs Chrome.
const browser = await launch({args: ['--no-sandbox', '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader']});
const tab = await browser.tab();
const {evaluate} = tab;
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
await tab.navigate(`${server.url}/`);
for (let i = 0; ; i++) {
  await sleep(1000);
  if (await evaluate('Boolean(globalThis.tiloMount?.navigation)').catch(() => false)) break;
  assert.ok(i < 90, `the desktop never presented: ${await evaluate("document.getElementById('status')?.textContent")}`);
}

// A panic stops the runtime without an exception; say so in the page's log.
await evaluate(`(() => {
  const report = globalThis.__tiloRuntimePanic;
  globalThis.__tiloRuntimePanic = raw => { console.error('runtime panic ' + raw); report?.(raw); };
})()`);
const snapshot = () => evaluate('tiloMount.navigation.getSnapshot()');
const command = body => evaluate(`(async()=>{const s=tiloMount.navigation.getSnapshot();await tiloMount.navigation.dispatch({...${JSON.stringify(body)},generation:s.generation});return true;})()`);
const label = s => s.entries.find(e => e.route.workspace === s.active.workspace && e.route.strip === s.active.strip && e.route.tab === s.active.tab)?.label;
const workspace = s => s.workspaces.findIndex(w => w.id === s.activeWorkspace);
// Waits for what an input asked for: `read` returns `expected`. Some changes
// land on the next drawn frame, and headless Chrome may draw only about once
// a second.
async function polls(what, read, expected, tries = 150) {
  let got;
  for (let i = 0; i < tries; i++) {
    got = await read();
    if (JSON.stringify(got) === JSON.stringify(expected)) return;
    await sleep(100);
  }
  if (process.env.SHOTS_DIR) writeFileSync(join(process.env.SHOTS_DIR, 'navigation-failed.png'), await tab.screenshot());
  // What the page said last (a module that did not load, a panic) and the
  // modules it fetched.
  console.error(tab.log.slice(-30).join('\n'));
  console.error(await evaluate(`JSON.stringify(performance.getEntriesByType('resource')
    .filter(e => e.name.endsWith('.wasm')).map(e => e.name.split('/').pop()))`).catch(String));
  assert.deepEqual(got, expected, what);
}
// Waits for the navigation snapshot to settle on what an input asked for.
const settles = async (what, read, expected, tries) => polls(what, async () => read(await snapshot()), expected, tries);
// The centre of the first semantics node with this accessible name (and,
// when given, a description starting with `described`), once the desktop
// shows it.
async function named(name, described = '') {
  let bounds;
  for (let i = 0; i < 150 && !bounds; i++) {
    if (i) await sleep(100);
    bounds = await evaluate(`(()=>{const walk=n=>{if(!n||typeof n!=='object')return null;
      if(n.name===${JSON.stringify(name)}&&n.bounds&&(n.description??'').startsWith(${JSON.stringify(described)}))return n.bounds;
      for(const v of Object.values(n)){const r=walk(v);if(r)return r}return null};
      return walk(tiloMount.state().semantics)})()`);
  }
  assert.ok(bounds, `the desktop shows "${name}"`);
  return [(bounds.x0 + bounds.x1) / 2, (bounds.y0 + bounds.y1) / 2];
}
const mouse = (type, x, y, extra = {}) => tab.send('Input.dispatchMouseEvent', {type, x, y, ...extra});
async function click(x, y) {
  await mouse('mouseMoved', x, y);
  await mouse('mousePressed', x, y, {button: 'left', buttons: 1, clickCount: 1});
  await mouse('mouseReleased', x, y, {button: 'left', buttons: 0, clickCount: 1});
}
// One notch of a mouse wheel; Chrome reports it in pixels.
const notch = (x, y, deltaX, deltaY = 0) => mouse('mouseWheel', x, y, {deltaX, deltaY});
const KEYS = {ArrowLeft: 37, ArrowRight: 39, PageUp: 33, PageDown: 34, Escape: 27};
async function key(name, modifiers = 0) {
  const base = {modifiers, key: name, code: name, windowsVirtualKeyCode: KEYS[name]};
  await tab.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', ...base});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', ...base});
}
// The desktop's stand-in chord off a Mac: Alt+Shift.
const chord = name => key(name, 1 | 8);
const typing = what => polls(`the note holds the keyboard${what}`, () => evaluate('document.activeElement?.tagName'), 'TEXTAREA');

const apps = (await snapshot()).applications.map(a => a.id);
for (const app of ['so.tilo.clocks', 'so.tilo.calculator', 'so.tilo.notes']) {
  assert.ok(apps.includes(app), `${app} is in this bundle: ${apps}`);
  const before = (await snapshot()).entries.length;
  await command({kind: 'launch', app});
  await settles(`${app} opens`, s => s.entries.length, before + 1);
  await sleep(1000);
}
const order = (await snapshot()).entries.map(e => e.label);
const {width, height} = await evaluate('(({width,height})=>({width,height}))(tiloMount.canvas.getBoundingClientRect())');
const [cx, cy] = [Math.round(width / 2), Math.round(height * 0.6)];

// Sideways wheel: a notch right is the next window, a notch left the previous.
await settles('the last opened window is active', label, order[2]);
await mouse('mouseMoved', cx, cy);
await notch(cx, cy, -120);
await settles('a notch left steps to the previous window', label, order[1]);
await notch(cx, cy, -120);
await settles('another notch left steps once more', label, order[0]);
await notch(cx, cy, 120);
await settles('a notch right steps back', label, order[1]);
console.log(JSON.stringify({step: 'wheel', ok: true}));

// A second workspace, through the overview; then the top bar's arrows.
await click(...await named('Open overview'));
await sleep(1500);
await click(...await named('New workspace'));
await settles('a new workspace opens', s => [s.workspaces.length, workspace(s)], [2, 1]);
await key('Escape');
await sleep(1000);
await click(...await named('Previous workspace'));
await settles('the top bar goes to the previous workspace', workspace, 0);
await click(...await named('Next workspace'));
await settles('and to the next one', workspace, 1);
await click(...await named('Previous workspace'));
await settles('back in the first workspace', workspace, 0);
console.log(JSON.stringify({step: 'workspace buttons', ok: true}));

// The shortcuts while an app's text field holds the keyboard: a new note.
await command({kind: 'select', route: (await snapshot()).entries.find(e => e.label === 'Notes').route});
await settles('Notes is active', label, 'Notes');
await sleep(1000);
// The empty notebook's own button, which opens the note in its editor.
await click(...await named('New note', 'Create the first note'));
await sleep(1500);
await typing('');
await chord('PageDown');
await settles('Alt+Shift+PageDown from a text field goes to the next workspace', workspace, 1);
await chord('PageUp');
await settles('Alt+Shift+PageUp goes back', workspace, 0);
await settles('to Notes', label, 'Notes');
await sleep(500);
await typing(' again');
await chord('ArrowLeft');
await settles('Alt+Shift+Left from a text field steps to the previous window', label, order[order.indexOf('Notes') - 1]);
await chord('ArrowRight');
await settles('Alt+Shift+Right steps back', label, 'Notes');
console.log(JSON.stringify({step: 'shortcuts from a text field', ok: true}));

// Workspace Activity over the windows opened above. The pane is its own
// module (src/activity.rs), fetched the first time it opens; the request
// stays open until the pane draws it, on the frame after the module
// arrived. That is one frame more than a view already in the start module
// takes, and a frame of this software-rendered debug bundle can take over
// ten seconds in CI.
const active = (await snapshot()).activeWorkspace;
await command({kind: 'workspace-activity', workspace: active, view: 'explore'});
await settles('Explore opens in the desktop once the pane has loaded',
  s => s.workspaceActivity && [s.workspaceActivity.view, s.workspaceActivity.inSurface], ['explore', true], 600);
// A split bundle fetched it now; a bundle left whole (`build.sh --no-split`)
// has it in place.
const split = join(resolve(bundle), 'pkg', 'split.json');
if (existsSync(split) && JSON.parse(readFileSync(split, 'utf8')).modules.some(m => m.module === 'activity')) {
  assert.ok(
    (await evaluate("performance.getEntriesByType('resource').map(e => e.name)")).some(name => name.endsWith('.activity.wasm')),
    'the pane came from its own module');
}
await command({kind: 'workspace-activity', workspace: active, view: 'catch-up'});
await settles('the open pane switches to Catch up',
  s => s.workspaceActivity && [s.workspaceActivity.view, s.workspaceActivity.inSurface], ['catch-up', true]);
await command({kind: 'close-workspace-activity', workspace: active});
await settles('and closes', s => s.workspaceActivity, null);
console.log(JSON.stringify({step: 'workspace activity', ok: true}));

const errors = tab.log.filter(line => line.startsWith('EXC') || /panic/i.test(line));
assert.deepEqual(errors, [], 'no page exception');
assert.equal(await evaluate('tiloMount.state().error ?? null'), null, 'the desktop kept drawing');
await browser.close();
await server.close();
