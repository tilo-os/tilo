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
import {existsSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const bundle = process.argv[2];
if (!bundle) throw Error('usage: node navigation.mjs BUNDLE_DIR');
if (process.env.SHOTS_DIR) mkdirSync(process.env.SHOTS_DIR, {recursive: true});
// A page that stops answering fails the run rather than holding the runner.
const watchdog = setTimeout(() => {
  console.error(`navigation: no result within ten minutes; last step: ${progress}`);
  process.exit(1);
}, 600_000);
let progress = 'loading';
const server = await serve(resolve(bundle));
// Software Vulkan, as the CI mount test runs Chrome.
const browser = await launch({args: ['--no-sandbox', '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader']});
// A tab showing the desktop at `deviceScaleFactor`, once it has presented.
async function desktop(deviceScaleFactor, options) {
  const tab = await browser.tab(options);
  await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor, mobile: false});
  await tab.navigate(`${server.url}/`);
  for (let i = 0; ; i++) {
    await sleep(1000);
    if (await tab.evaluate('Boolean(globalThis.tiloMount?.navigation)').catch(() => false)) break;
    assert.ok(i < 90, `the desktop never presented: ${await tab.evaluate("document.getElementById('status')?.textContent")}`);
  }
  return tab;
}
const tab = await desktop(1);
const {evaluate} = tab;

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
// The accessibility mirror lies over the canvas: each uniquely named button
// the frame lays out has its DOM element at the rect it is drawn at, in the
// page's CSS pixels, where magnifiers, touch exploration and element
// geometry look for it. Layout units are CSS pixels whatever the device
// pixel ratio, so it holds at 2 as at 1.
async function mirrorLiesOverDrawnButtons(evaluate, deviceScaleFactor) {
  progress = `the accessibility mirror lies over the drawn buttons at device scale ${deviceScaleFactor}`;
  let placed;
  for (let i = 0; i < 50; i++) {
    if (i) await sleep(200);
    // The host lays out semantics when asked (or when the mirror takes
    // focus); ask, so a page no step has looked at yet has a tree.
    placed = await evaluate(`(async()=>{
      tiloMount.requestSemantics();
      await new Promise(done => setTimeout(done, 0));
      const state=tiloMount.state(),canvas=tiloMount.canvas,size=state.logicalSize,r=canvas.getBoundingClientRect();
      if(state.phase!=='presented'||!size||!(size[0]>0)||!(size[1]>0))return null;
      if(devicePixelRatio!==${deviceScaleFactor})throw Error('the page draws at device scale '+devicePixelRatio);
      const drawn=new Map();
      const walk=n=>{
        const s=n?.semantics;if(s?.state?.hidden)return;
        if(s?.role==='Button'&&s.name){
          const b=s.bounds?[s.bounds.x0,s.bounds.y0,s.bounds.x1-s.bounds.x0,s.bounds.y1-s.bounds.y0]:n.rect;
          if(b&&b[2]>0&&b[3]>0)drawn.set(s.name,drawn.has(s.name)?null:b);
        }
        for(const child of n?.children??[])walk(child);
      };
      walk(state.semantics?.root);
      const sx=r.width/size[0],sy=r.height/size[1],out=[];
      for(const [name,b] of drawn){
        const mirrored=[...tiloMount.semantics.querySelectorAll('[role=button]')].filter(e=>e.getAttribute('aria-label')===name);
        if(!b||mirrored.length!==1)continue;
        const d=mirrored[0].getBoundingClientRect();
        out.push({name,drawn:[r.left+b[0]*sx,r.top+b[1]*sy,b[2]*sx,b[3]*sy],mirror:[d.left,d.top,d.width,d.height]});
      }
      return out;
    })()`);
    if (placed?.length && placed.every(p => p.drawn.every((v, k) => Math.abs(v - p.mirror[k]) <= 1))) break;
  }
  assert.ok(placed?.length, `the desktop mirrors at least one drawn button at device scale ${deviceScaleFactor}`);
  for (const p of placed) assert.ok(p.drawn.every((v, k) => Math.abs(v - p.mirror[k]) <= 1),
    `at device scale ${deviceScaleFactor}, "${p.name}" is mirrored at ${JSON.stringify(p.mirror)}, drawn at ${JSON.stringify(p.drawn)}`);
  console.log(JSON.stringify({settled: progress, buttons: placed.length}));
}
await mirrorLiesOverDrawnButtons(evaluate, 1);
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
      // What the page said last: a frame it skipped, a panic.
      console.error(tab.log.slice(-30).join('\n'));
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
// to, the overview and a new strip.
async function openWorkspaceMenu() {
  const s = await snapshot();
  const shown = s.workspaces.find(w => w.id === s.activeWorkspace).label;
  await click(...await named(`${shown}: workspaces, overview and strips`));
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
await click(...await named('New note', 'Creates the first note'));
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

// The same at device scale 2, in a profile of its own so the two desktops do
// not share storage: the canvas's backing store doubles while the mirror
// stays in CSS pixels. There every control of a window with a spread of
// them is checked, whatever its role, and a text field's editable element,
// the one the caret and input methods use, is checked again once it holds
// the keyboard.
//
// Where the mirror of each drawn semantics node is placed, against where the
// node is drawn, both in page CSS pixels. A node without bounds of its own
// stands for the area it is drawn in, as in the mirror. A node is matched to
// its element by its semantic key or action target (the last part of the
// element's data-tilo-semantic key) when that is unique, else by an
// accessible name unique among nodes of its role (a page's heading may share
// its name with the place it heads); a node neither identifies is not checked.
const MIRROR_PLACEMENT = `(async()=>{
  tiloMount.requestSemantics();
  await new Promise(done => setTimeout(done, 0));
  const state=tiloMount.state(),canvas=tiloMount.canvas,size=state.logicalSize,r=canvas.getBoundingClientRect();
  if(state.phase!=='presented'||!size||!(size[0]>0)||!(size[1]>0))return null;
  const sx=r.width/size[0],sy=r.height/size[1],nodes=[],roles={};
  const role=s=>s.role&&typeof s.role==='object'?Object.keys(s.role)[0]:s.role;
  const visit=(s,enclosing)=>{
    if(!s||s.state?.hidden)return;
    const b=s.bounds?[s.bounds.x0,s.bounds.y0,s.bounds.x1-s.bounds.x0,s.bounds.y1-s.bounds.y0]:enclosing;
    if(role(s)){
      roles[role(s)]=(roles[role(s)]??0)+1;
      if(b&&b[2]>0&&b[3]>0)nodes.push({role:role(s),name:s.name,id:s.semantic_key??s.action_target,b,focused:Boolean(s.state?.focused)});
    }
    for(const child of s.children??[])visit(child,b);
  };
  const walk=n=>{
    if(!n)return;
    if(n.semantics){if(n.semantics.state?.hidden)return;visit(n.semantics,n.rect);}
    for(const child of n.children??[])walk(child);
  };
  walk(state.semantics?.root);
  const named=n=>n.name?n.role+'|'+n.name:undefined;
  const tally=key=>{const m=new Map();for(const n of nodes){const k=key(n);if(k)m.set(k,(m.get(k)??0)+1);}return m;};
  const ids=tally(n=>n.id),names=tally(named);
  const mirrors=[...tiloMount.semantics.querySelectorAll('[data-tilo-semantic]')].map(e=>{
    let id;try{id=JSON.parse(e.dataset.tiloSemantic).at(-1);}catch{}
    return {e,id,named:named({role:e.tiloRole&&role({role:e.tiloRole}),name:e.getAttribute('aria-label')})};
  });
  const controls=[];
  for(const n of nodes){
    let mirrored=[];
    if(typeof n.id==='string'&&ids.get(n.id)===1)mirrored=mirrors.filter(m=>m.id===n.id);
    if(mirrored.length!==1&&named(n)&&names.get(named(n))===1)mirrored=mirrors.filter(m=>m.named===named(n));
    if(mirrored.length!==1)continue;
    const e=mirrored[0].e,d=e.getBoundingClientRect(),b=n.b;
    controls.push({name:n.name??n.id,role:n.role,editable:e.localName==='textarea',focused:n.focused,active:document.activeElement===e,
      drawn:[r.left+b[0]*sx,r.top+b[1]*sy,b[2]*sx,b[3]*sy],mirror:[d.left,d.top,d.width,d.height]});
  }
  return {controls,roles};
})()`;
const lies = c => c.drawn.every((v, k) => Math.abs(v - c.mirror[k]) <= 1);
const describe = c => `${c.role} "${c.name}" is mirrored at ${JSON.stringify(c.mirror.map(Math.round))}, drawn at ${JSON.stringify(c.drawn.map(Math.round))}`;
const tally = controls => {
  const counts = {};
  for (const c of controls) counts[c.role] = (counts[c.role] ?? 0) + 1;
  return counts;
};
const enough = (counts, minimum) => Object.entries(minimum).every(([role, least]) => (counts[role] ?? 0) >= least);
// Waits until the window has drawn its controls and every checked control's
// mirror lies over it (an app's module arrives after its window opens, and
// frames of this software-drawn bundle come seconds apart), then requires
// both: every mirror within a pixel, and at least `minimum[role]` controls
// of each role checked, so the check cannot pass over a window that lost
// its controls.
async function mirrorLiesOverEveryControl(page, what, minimum, accept = () => true) {
  progress = what;
  let placed;
  for (const deadline = Date.now() + 60_000; ;) {
    placed = await page.evaluate(MIRROR_PLACEMENT);
    if (placed && placed.controls.every(lies) && enough(tally(placed.controls), minimum) && accept(placed)) break;
    if (Date.now() > deadline) break;
    await sleep(250);
  }
  assert.ok(placed, `${what}: the desktop presents a semantics tree`);
  const counts = tally(placed.controls);
  console.log(JSON.stringify({checking: what, checked: counts, present: placed.roles}));
  assert.deepEqual(placed.controls.filter(c => !lies(c)).map(describe), [],
    `${what}: every control's mirror lies within a pixel of where it is drawn`);
  for (const [role, least] of Object.entries(minimum))
    assert.ok((counts[role] ?? 0) >= least,
      `${what}: at least ${least} ${role} checked, got ${counts[role] ?? 0} (checked ${JSON.stringify(counts)}, present ${JSON.stringify(placed.roles)})`);
  return placed.controls;
}
// The windows, one at a time, and the controls each must have mirrored: the
// whole desktop is checked each time, so these count its top bar and window
// frame too. Calendar's month has a button a day, tabs (its five views and
// the window's tab), a combo box and a navigation tree; Tasks has a text
// field, its places tree and the page's heading.
const HIDPI_WINDOWS = [
  ['so.tilo.calendar', {Button: 45, ToggleButton: 2, ComboBox: 1, Tree: 1, TreeItem: 4, Tab: 6, Separator: 1},
    [['Tree', 'Calendar navigation']]],
  ['so.tilo.tasks', {Button: 10, ToggleButton: 2, TextInput: 1, Tree: 1, TreeItem: 2, Heading: 1, Separator: 1},
    [['Tree', 'Task views'], ['Heading', 'Issues']]],
];
{
  const hidpi = await desktop(2, {context: await browser.context()});
  assert.equal(await hidpi.evaluate('devicePixelRatio'), 2, 'the second desktop draws at device scale 2');
  const navigate = body => hidpi.evaluate(`(async()=>{const s=tiloMount.navigation.getSnapshot();
    await tiloMount.navigation.dispatch({...${JSON.stringify(body)},generation:s.generation});})()`);
  const entries = async () => (await hidpi.evaluate('tiloMount.navigation.getSnapshot()')).entries;
  for (const [app, minimum, named] of HIDPI_WINDOWS) {
    for (const entry of await entries()) await navigate({kind: 'close', route: entry.route});
    await polls(`no window is open before ${app} at device scale 2`, async () => (await entries()).length, 0);
    await navigate({kind: 'launch', app});
    await polls(`${app} opens at device scale 2`, async () => (await entries()).length, 1);
    const controls = await mirrorLiesOverEveryControl(hidpi, `${app}'s controls are mirrored where they are drawn at device scale 2`, minimum);
    for (const [role, name] of named)
      assert.ok(controls.some(c => c.role === role && c.name === name), `${app}'s ${role} "${name}" is mirrored where it is drawn at device scale 2`);
    if (!minimum.TextInput) continue;
    const field = controls.find(c => c.role === 'TextInput' && c.editable);
    assert.ok(field, `${app}'s text field is mirrored by an editable element: ${JSON.stringify(controls.filter(c => c.role === 'TextInput'))}`);
    // Focus it as assistive technology or a tap does: through its mirror.
    await hidpi.evaluate(`[...tiloMount.semantics.querySelectorAll('textarea')].find(e=>e.getAttribute('aria-label')===${JSON.stringify(field.name)}).focus()`);
    const focused = await mirrorLiesOverEveryControl(hidpi, `${app}'s focused text field "${field.name}" keeps its editable mirror over it at device scale 2`,
      minimum, p => p.controls.some(c => c.name === field.name && c.focused && c.active));
    const now = focused.find(c => c.name === field.name);
    assert.ok(now?.editable && now.focused && now.active,
      `${app}'s text field "${field.name}" holds the keyboard in the app and in the page: ${JSON.stringify(now)}`);
  }
  const backing = await hidpi.evaluate('(c=>c.width/c.getBoundingClientRect().width)(tiloMount.canvas)');
  assert.ok(Math.abs(backing - 2) < 0.01, `at device scale 2 the canvas draws ${backing} device pixels a CSS pixel`);
  const errors = hidpi.log.filter(line => line.startsWith('EXC') || /panic/i.test(line));
  assert.deepEqual(errors, [], 'no page exception at device scale 2');
  await hidpi.close();
}
await browser.close();
await server.close();
clearTimeout(watchdog);
