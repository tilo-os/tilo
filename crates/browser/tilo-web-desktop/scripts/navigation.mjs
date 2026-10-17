// The desktop's own navigation, driven through the browser as a person does
// it: a spin of a mouse's sideways wheel pans the strip and focus follows
// to the window it stopped on; the top bar's workspace menu switches
// workspaces; the desktop shortcuts
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
// A page that stops answering fails the run rather than holding the runner.
const watchdog = setTimeout(() => {
  console.error(`navigation: no result within ten minutes; last step: ${progress}`);
  process.exit(1);
}, 600_000);
let progress = 'loading';
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
// Waits for what an input asked for: `read` returns `expected`. A change
// lands on a drawn frame, and this debug bundle draws in software: in CI a
// frame takes seconds (ten and more while the runner's CPUs are busy), and
// headless Chrome draws only about once a second at best. So a step gives
// up only once its tries are spent *and* the page has presented `frames`
// more frames without showing the change; a page that stops presenting
// fails at `STALLED_MS`.
const STALLED_MS = 240_000;
async function polls(what, read, expected, tries = 150, frames = 3) {
  progress = what;
  const started = {at: Date.now(), presented: await presented()};
  let got;
  for (let i = 1; ; i++) {
    got = await read();
    if (JSON.stringify(got) === JSON.stringify(expected)) {
      console.log(JSON.stringify({settled: what, ms: Date.now() - started.at, tries: i,
        presented: [started.presented, await presented()]}));
      return;
    }
    if (i >= tries && ((await presented()).frames - started.presented.frames >= frames
      || Date.now() - started.at > STALLED_MS)) break;
    await sleep(100);
  }
  if (process.env.SHOTS_DIR) writeFileSync(join(process.env.SHOTS_DIR, 'navigation-failed.png'), await tab.screenshot());
  // Whether the page kept presenting frames while it waited (a frame that
  // never came reads differently from one that came without the change),
  // and where the last click landed against where its target is now.
  console.error(JSON.stringify({waited: what, ms: Date.now() - started.at, tries,
    presented: [started.presented, await presented()],
    lastClick: lastClick && {...lastClick, now: await bounds(lastClick.name, lastClick.described)}}));
  // What the page said last (a module that did not load, a panic) and the
  // modules it fetched.
  console.error(tab.log.slice(-30).join('\n'));
  console.error(await evaluate(`JSON.stringify(performance.getEntriesByType('resource')
    .filter(e => e.name.endsWith('.wasm')).map(e => e.name.split('/').pop()))`).catch(String));
  assert.deepEqual(got, expected, what);
}
// Waits for the navigation snapshot to settle on what an input asked for.
const settles = async (what, read, expected, tries) => polls(what, async () => read(await snapshot()), expected, tries);
// The first semantics node with this accessible name (and, when given, a
// description starting with `described`) in the tree the page holds now:
// the host is asked for a fresh tree first, so it is the last laid-out
// frame's.
const bounds = (name, described = '') => evaluate(`(async()=>{tiloMount.requestSemantics();
  await new Promise(done => setTimeout(done, 0));
  const state=tiloMount.state(),canvas=tiloMount.canvas;
  if(state.phase!=='presented'||!state.frames||canvas.closest('[inert]'))return null;
  const walk=n=>{if(!n||typeof n!=='object'||n.state?.hidden||n.state?.disabled)return null;
  if(n.name===${JSON.stringify(name)}&&n.bounds&&(n.description??'').startsWith(${JSON.stringify(described)}))return n.bounds;
  for(const v of Object.values(n)){const r=walk(v);if(r)return r}return null};
  const b=walk(state.semantics);
  return b&&b.x1>b.x0&&b.y1>b.y0?b:null})()`);
// The frames the page has presented.
const presented = () => evaluate('(({frames, phase}) => ({frames, phase}))(tiloMount.state())').catch(String);
let lastClick;
// Semantics use logical canvas coordinates; CDP takes viewport CSS pixels.
// The backing store already accounts for DPR, so do not multiply by it.
const centre = b => evaluate(`(()=>{
  const state=tiloMount.state(),canvas=tiloMount.canvas;
  if(state.phase!=='presented'||!state.frames||canvas.closest('[inert]'))return null;
  const b=${JSON.stringify(b)},size=state.logicalSize,r=canvas.getBoundingClientRect();
  if(!size||!(size[0]>0)||!(size[1]>0)||!r.width||!r.height)return null;
  const x=r.left+(b.x0+b.x1)/2*r.width/size[0];
  const y=r.top+(b.y0+b.y1)/2*r.height/size[1];
  if(!Number.isFinite(x)||!Number.isFinite(y)||x<r.left||x>=r.right||y<r.top||y>=r.bottom
    ||document.elementFromPoint(x,y)!==canvas)return null;
  return [x,y];
})()`);
// Where to press a control the desktop shows, as a person does: point at it
// and press once it is still under the pointer. A frame lays out where the
// control is at that moment, and windows ease into place over several
// frames; in CI those frames come seconds apart, so the place the last
// frame reported can be one the control is about to leave. Moving the
// pointer makes the page draw again, and a control that sits in the same
// place across a presented frame has come to rest.
async function named(name, described = '') {
  let found, at, point;
  for (let i = 0; i < 150 && !point; i++) {
    if (i) await sleep(100);
    at = await presented();
    found = await bounds(name, described);
    point = found && await centre(found);
  }
  assert.ok(point, `the desktop presents an interactive "${name}"`);
  for (const deadline = Date.now() + STALLED_MS; ;) {
    progress = `pointing at "${name}"`;
    // Two moves, so the pointer moves even if it was already there.
    point = await centre(found);
    assert.ok(point, `the desktop presents an interactive "${name}"`);
    const [x, y] = point;
    await mouse('mouseMoved', x - 1, y);
    await mouse('mouseMoved', x, y);
    let now;
    do {
      assert.ok(Date.now() < deadline, `the desktop presents a frame after pointing at "${name}"`);
      await sleep(100);
      now = await presented();
    } while (!(now.frames > at.frames));
    const again = await bounds(name, described);
    assert.ok(again, `the desktop still shows "${name}"`);
    if (JSON.stringify(again) === JSON.stringify(found)) break;
    [found, at] = [again, now];
  }
  lastClick = {name, described, bounds: found, presented: at};
  point = await centre(found);
  assert.ok(point, `the desktop presents an interactive "${name}"`);
  return point;
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
const typing = what => polls(`the note holds the keyboard${what}`, () => evaluate(`({
  element:document.activeElement?.tagName,
  editor:tiloMount.state().semantics?.keyboard_focus_owner?.endsWith('/BlockEditor[0]')??false,
})`), {element:'TEXTAREA',editor:true});

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

// Sideways wheel: a spin's notches pan the strip, a line (40 px) a notch,
// and focus follows to the window it stopped on. The three
// windows overflow the strip, each two thirds of it wide, and the last one
// opened shows at its right end.
await settles('the last opened window is active', label, order[2]);
const pan = async () => (await snapshot()).desktopPan;
// A spin's notches leave back to back, none waiting for the page to take
// the one before: headless Chrome answers a wheel event only with a later
// frame, which it draws about once a second. Chrome merges notches queued
// up that way, summing their pixels and their `wheelDelta`, and answers
// some before the page has them: the rest reach it later. So the page
// counts the notches that reach it, by their pixels (120 a notch here) and
// by `wheelDelta`, and the lines the host hands the runtime for them, and
// a spin is checked once all of them are in.
await evaluate(`(()=>{const seen=window.wheelSeen={events:0,notches:0,ticks:0,lines:0};
  addEventListener('wheel',e=>{if(e.isTrusted){seen.events++;seen.notches+=e.deltaX/120;seen.ticks+=-e.wheelDeltaX/120}},{capture:true,passive:true});
  tiloMount.canvas.addEventListener('wheel',e=>{if(!e.isTrusted&&e.deltaMode===1)seen.lines+=e.deltaX},{passive:true});
  // Each frame where the pan, the notches in or the focused window changed,
  // for a failure to show how the strip got where it is.
  const trace=window.wheelTrace=[],t0=performance.now();let last='';
  (function frame(){const n=tiloMount.navigation.getSnapshot(),row=[n.desktopPan?.offset??null,seen.notches,n.active?.group??null];
    const key=JSON.stringify(row);if(key!==last){last=key;trace.push([Math.round(performance.now()-t0),...row]);if(trace.length>400)trace.shift()}
    requestAnimationFrame(frame);})();})()`);
const seen = () => evaluate('({...window.wheelSeen})');
const spin = async (notches, deltaX) => {
  progress = `a spin of ${notches} notches (${deltaX})`;
  const before = await seen();
  await Promise.all(Array.from({length: notches}, () => notch(cx, cy, deltaX)));
  return {sent: notches * Math.sign(deltaX), before};
};
await mouse('mouseMoved', cx, cy);
// The strip eases the last opened window into view; a spin's travel is
// measured from where it comes to rest, the same offset a second apart.
progress = 'the strip comes to rest';
let start = (await pan()).offset;
for (let same = 0; same < 3;) {
  await sleep(1000);
  const now = (await pan()).offset;
  same = Math.abs(now - start) < 0.01 ? same + 1 : 0;
  start = now;
}
assert.ok(start > 600, `the strip shows the last window, panned ${start}`);
// Waits for every notch of a spin to reach the page, each as a line, and
// the strip to pan from `from` the way they went. How far it pans is left
// to the surface's and the host's replay tests: in headless Chrome, whose
// frames and wheel answers come about a second apart, two runs in three
// panned 20 notches about 28 lines.
async function travels(what, from, {sent, before}) {
  progress = what;
  let got, moved;
  const deadline = Date.now() + 240_000;
  for (;;) {
    const now = await seen();
    got = {sent, events: now.events - before.events, notches: now.notches - before.notches,
      ticks: now.ticks - before.ticks, lines: now.lines - before.lines};
    const {offset, max} = await pan();
    moved = {lines: (offset - from) / 40, offset, max};
    if (got.notches === sent && moved.lines * Math.sign(sent) >= 1) break;
    if (Date.now() > deadline) {
      if (process.env.SHOTS_DIR) writeFileSync(join(process.env.SHOTS_DIR, 'navigation-failed.png'), await tab.screenshot());
      assert.fail(`${what}: ${JSON.stringify({moved, got})} from ${from}; [ms, offset, notches, group]: ${JSON.stringify(await evaluate('wheelTrace'))}; windows: ${JSON.stringify((await snapshot()).entries.map(e => [e.label, e.route.group]))}`);
    }
    await sleep(250);
  }
  console.log(JSON.stringify({step: 'wheel', spin: got, moved}));
  assert.equal(got.ticks, sent, `${what}: wheelDelta counts every notch: ${JSON.stringify(got)}`);
  assert.ok(Math.abs(got.lines - sent) < 0.01, `${what}: each notch is a line: ${JSON.stringify(got)}`);
}
await travels('a spin left pans the strip left', start, await spin(20, -120));
await settles('and focus follows to a window it brought into view', s => order.slice(0, 2).includes(label(s)), true);
// Every notch of it reaches the page before anything else is checked, so
// none still arriving lands on the steps after.
const {sent, before} = await spin(40, 120);
progress = 'a long spin right reaches the page whole and pans the strip to its end';
for (const deadline = Date.now() + 240_000; ;) {
  const [now, {offset, max}] = [await seen(), await pan()];
  if (now.notches - before.notches === sent && Math.abs(offset - max) < 0.5) break;
  assert.ok(Date.now() < deadline, `${progress}: ${JSON.stringify({notches: now.notches - before.notches, sent, offset, max})}`);
  await sleep(250);
}
await settles('and focus follows to the last window', label, order[2]);
console.log(JSON.stringify({step: 'wheel', ok: true}));

// The workspace's name in the top bar opens its menu: the workspaces to go
// to, the overview and a new desk.
async function openWorkspaceMenu() {
  const s = await snapshot();
  const shown = s.workspaces.find(w => w.id === s.activeWorkspace).label;
  await click(...await named(`${shown}: workspaces, overview and desks`));
  await sleep(500);
}
// Goes to the workspace at `index` through that menu.
async function chooseWorkspace(index) {
  const {label: name} = (await snapshot()).workspaces[index];
  await openWorkspaceMenu();
  await click(...await named(name));
}
// A second workspace, through the overview the menu opens; then the menu
// to go between them.
await openWorkspaceMenu();
await click(...await named('Overview'));
await sleep(1500);
await click(...await named('New workspace'));
await settles('a new workspace opens', s => [s.workspaces.length, workspace(s)], [2, 1]);
await key('Escape');
await sleep(1000);
await chooseWorkspace(0);
await settles('the workspace menu goes to the first workspace', workspace, 0);
await chooseWorkspace(1);
await settles('and to the second one', workspace, 1);
await chooseWorkspace(0);
await settles('back in the first workspace', workspace, 0);
console.log(JSON.stringify({step: 'workspace menu', ok: true}));


// The shortcuts while an app's text field holds the keyboard: a new note.
await command({kind: 'select', route: (await snapshot()).entries.find(e => e.label === 'Notes').route});
await settles('Notes is active', label, 'Notes');
await sleep(1000);
// The empty notebook's own button, which opens the note in its editor.
await click(...await named('New note', 'Create the first note'));
await typing('');
await polls('one pointer activation creates exactly one note', () => evaluate(`(()=>{
  const walk=n=>{if(!n)return null;if(n.test_id==='page_tree')return n;
    for(const c of n.children??[]){const found=walk(c);if(found)return found}return null};
  const tree=walk(tiloMount.state().semantics?.root);
  return tree?.semantics?.children?.filter(n=>n.role==='TreeItem').length??null;
})()`), 1);
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
clearTimeout(watchdog);
