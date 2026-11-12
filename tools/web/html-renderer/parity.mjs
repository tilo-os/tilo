// Every app of the desktop bundle shown as HTML against the same app on the
// WebGPU canvas: a pixel diff of the first settled frame, then the app driven
// the way a person drives it — menus opened, a dialog raised, a drag made, a
// stroke drawn on the canvas app, a clip moved on the video editor's timeline
// — with a diff after each step, plus where the semantic elements live, the
// fields the page shows as its own inputs, and the latency of typing into the
// app's first text field and of wheel steps over its middle, against the
// plain-DOM page in the same run. Both renders get the light theme as the
// person's setting, so an app that shows which theme is in use shows the same
// one.
//
// A step names its target the way assistive technology does, never by a
// coordinate: the same expression finds it in both renders, so the driver
// works the app, not this build of it. Where a render put it is reported
// beside the diff, which catches a layout that only one render produced.
//
// usage: node tools/web/html-renderer/parity.mjs BUNDLE_DIR OUT_DIR [--apps id,id] [--scenarios] [--phone]
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../chrome.mjs';
import {decode, diff} from './png.mjs';

const bundle = resolve(process.argv[2]);
const out = resolve(process.argv[3]);
mkdirSync(out, {recursive: true});
const only = process.argv.includes('--apps') ? process.argv[process.argv.indexOf('--apps') + 1].split(',') : null;
// --scenarios runs just the apps with a scenario, for the driver's own loop.
const scenariosOnly = process.argv.includes('--scenarios');
// --phone is the same apps at a phone's width and three device pixels per
// point, where text is set finer and the layout is the app's own narrow one.
const phone = process.argv.includes('--phone');
const apps = JSON.parse(readFileSync(join(bundle, 'apps.json'), 'utf8')).applications
  .map(app => app.id).filter(id => !only || only.includes(id));
const gpuArgs = process.platform === 'linux' ? ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader'] : [];
const sandbox = process.getuid?.() === 0 ? ['--no-sandbox'] : [];
const [width, height] = phone ? [390, 844] : [1280, 800];
const scale = phone ? 3 : 1;
const tag = phone ? `-${width}` : '';

const pct = (values, p) => {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return Math.round(sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] * 10) / 10;
};

const MARKDOWN = Array.from({length: 40}, (_, i) => i % 10 === 0
  ? `## Section ${i / 10 + 1}`
  : `Paragraph ${i}: the quick brown fox jumps over the lazy dog while notes keep every thought close at hand.`).join('\n\n');
// A note made through the app's own action — the path the page's own import
// takes — then waited for by the name the sidebar gives it.
const SEED_NOTE = `(async () => {
  const actions = window.mount.actions();
  const importing = actions.find(a => /import/i.test(a.address.action) && /markdown/i.test(a.address.action));
  if (!importing) return 'no import action: ' + actions.map(a => a.address.action).join(',');
  await window.mount.invokeAction(importing.address.resource, importing.address.action,
    {title: ${JSON.stringify('Field notes')}, markdown: ${JSON.stringify(MARKDOWN)}, images: [], note: null});
  return 'seeded';
})()`;

// In the page: a mark at the first animation frame after each change, as
// measure.mjs takes it.
const INSTRUMENT = `(() => {
  const m = window.renderMarks ??= {presented: []};
  m.inputs = []; m.painted = [];
  const changed = () => requestAnimationFrame(() => m.painted.push(performance.now()));
  if (m.presented) { const push = m.presented.push.bind(m.presented); m.presented.push = (...v) => { changed(); return push(...v); }; }
  for (const type of ['keydown', 'wheel']) addEventListener(type, e => m.inputs.push([type, e.timeStamp]), {capture: true, passive: true});
  document.addEventListener('scroll', changed, true);
  document.addEventListener('input', changed, true);
  return true;
})()`;
const LATENCIES = `(() => {
  const m = window.renderMarks, out = {keydown: [], wheel: []};
  for (const [type, at] of m.inputs) { const painted = m.painted.find(t => t >= at); if (painted !== undefined) out[type].push(painted - at); }
  m.inputs = []; m.painted = [];
  return JSON.stringify(out);
})()`;
// The first text field on screen: its centre, its role, and whether the page
// shows it as its own input.
const FIELD = `(() => {
  const walk = n => { if (!n) return null;
    const s = n.semantics;
    if (s && ['TextInput', 'MultilineTextInput'].includes(s.role) && !s.state?.disabled && n.rect && n.rect[2] > 20 && n.rect[3] > 8
      && n.rect[0] >= 0 && n.rect[1] >= 0 && n.rect[0] + n.rect[2] <= innerWidth && n.rect[1] + n.rect[3] <= innerHeight)
      return {x: n.rect[0] + Math.min(n.rect[2] / 2, 60), y: n.rect[1] + n.rect[3] / 2, role: s.role, autofill: s.state?.autofill ?? null};
    for (const c of n.children ?? []) { const r = walk(c); if (r) return r; } return null; };
  return JSON.stringify(walk(window.mount.state()?.semantics?.root));
})()`;
// What a step's target is, and where this render put it: the app is worked by
// the name assistive technology reads, never by a coordinate.
const FIND = (text, role) => `(() => {
  const root = window.mount.state()?.semantics?.root;
  const needle = ${JSON.stringify(text)}.toLowerCase();
  const wanted = ${JSON.stringify(role ?? null)};
  const hits = [];
  const walk = n => { if (!n) return;
    const s = n.semantics ?? {};
    const words = [s.label, s.value, s.name].filter(Boolean).join(' ').toLowerCase();
    if (words.includes(needle) && (!wanted || s.role === wanted) && n.rect && n.rect[2] > 0 && n.rect[3] > 0)
      hits.push({x: n.rect[0] + n.rect[2] / 2, y: n.rect[1] + n.rect[3] / 2, rect: [n.rect[0], n.rect[1], n.rect[2], n.rect[3]], role: s.role ?? null, name: words.slice(0, 80)});
    for (const c of n.children ?? []) walk(c);
  };
  walk(root);
  return JSON.stringify(hits.slice(0, 6));
})()`;
// Every name the tree holds: what a step that found nothing is told about.
const NAMES = `(() => {
  const out = [];
  const walk = n => { if (!n) return; const s = n.semantics ?? {};
    const words = [s.label, s.value, s.name].filter(Boolean).join(' ').trim();
    if (words) out.push(words.slice(0, 40));
    for (const c of n.children ?? []) walk(c); };
  walk(window.mount.state()?.semantics?.root);
  return JSON.stringify(out.slice(0, 40));
})()`;
const PAGE = `JSON.stringify({
  elements: document.querySelectorAll('.tilo-dom *').length,
  texts: document.querySelectorAll('.tilo-dom span').length,
  hosted: document.querySelectorAll('.tilo-dom [data-tilo-semantic]').length,
  mirrored: document.querySelectorAll('[data-tilo-semantic]').length - document.querySelectorAll('.tilo-dom [data-tilo-semantic]').length,
  paintedTextsExposed: [...document.querySelectorAll('.tilo-dom span:not([data-tilo-semantic])')].filter(s => !s.closest('[aria-hidden="true"]')).length,
  fields: [...document.querySelectorAll('input[data-tilo-field]')].map(i => ({type: i.type, autocomplete: i.getAttribute('autocomplete')})),
  placeholders: document.querySelectorAll('.tilo-dom div[style*="repeating-linear-gradient"]').length,
  frameMs: window.__tiloDom?.stats.lastMs ?? null,
})`;

// Frames stop, and nothing is still on its way: an app whose tiles or photos
// arrive after its frames settled was compared once with them and once
// without, which read as a 27 % pixel difference that was only the network.
const LOADED = 'JSON.stringify({frames: window.renderMarks.presented.length, resources: performance.getEntriesByType("resource").length})';
async function settle(tab) {
  await tab.waitFor('!!window.renderMarks', {timeout: 60000});
  await tab.waitFor('window.renderMarks.presented[0]', {timeout: 90000});
  let last = '';
  let steady = 0;
  for (let i = 0; i < 60; i++) {
    await sleep(250);
    const now = await tab.evaluate(LOADED);
    steady = now === last ? steady + 1 : 0;
    last = now;
    // Two quiet passes, and at least one frame has been drawn since the
    // last thing that arrived.
    if (steady >= 3) return;
  }
}

async function open(server, page) {
  const browser = await launch({args: [`--window-size=${width},${height}`, ...gpuArgs, ...sandbox]});
  const tab = await browser.tab();
  await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: scale, mobile: phone});
  if (phone) await tab.send('Emulation.setTouchEmulationEnabled', {enabled: true, maxTouchPoints: 5});
  await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: 'light'}]});
  await tab.navigate(`${server.url}/${page}`);
  return {browser, tab};
}

// Typing into the first field and wheel steps over the middle: each input's
// latency to the first painted change after it.
async function interact(tab, field) {
  await tab.evaluate(INSTRUMENT);
  const result = {};
  if (field) {
    for (const type of ['mousePressed', 'mouseReleased']) await tab.send('Input.dispatchMouseEvent', {type, x: field.x, y: field.y, button: 'left', clickCount: 1});
    await sleep(400);
    await tab.evaluate(LATENCIES);
    for (const ch of 'Parity typing') {
      await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch, unmodifiedText: ch});
      await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
      await sleep(60);
    }
    await sleep(300);
    const typed = JSON.parse(await tab.evaluate(LATENCIES)).keydown;
    result.typing = {n: typed.length, p50: pct(typed, 0.5), p90: pct(typed, 0.9)};
    result.focus = await tab.evaluate('document.activeElement?.localName ?? null');
  }
  await tab.evaluate(LATENCIES);
  for (let i = 0; i < 12; i++) {
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseWheel', x: width / 2, y: height / 2, deltaX: 0, deltaY: i < 6 ? 120 : -120});
    await sleep(60);
  }
  await sleep(300);
  const wheel = JSON.parse(await tab.evaluate(LATENCIES)).wheel;
  result.wheel = {n: wheel.length, p50: pct(wheel, 0.5), p90: pct(wheel, 0.9)};
  return result;
}

// What a person does to an app, as steps both renders take:
//   {wait: 'name'}                wait until the app shows a control with that name
//   {click: 'name'}               press and release its centre ({role} narrows it)
//   {click: ['name', dx, dy]}     the same, offset from that centre
//   {click: [x, y]}               press and release a point
//   {rightClick: 'name'}          its context menu
//   {type: 'text'}                keys into whatever has the keyboard
//   {key: 'Escape'}               one key
//   {drag: [from, to]}            press from, move to to, release, each named
//                                 or a point, as `click` takes them
//   {wheel: dy}                   wheel steps over the middle of the window
//   {shot: 'name'}                a screenshot of both renders, compared
//   {seed: 'note'}                a note made through the app's own import action
const SCENARIOS = {
  'so.tilo.notes': [
    {seed: 'note'},
    {wait: 'Field notes'},
    {click: 'Field notes', role: 'ListItem'},
    // The note's body is painted text: the page names the title, and the
    // text under it is reached from where the title is.
    {click: ['Field notes Page title', 0, 150]},
    {type: 'A line typed into the note. '},
    {shot: 'typed'},
    {drag: [['Field notes Page title', 0, 150], ['Field notes Page title', 0, 330]]},
    {shot: 'selected'},
    // The text menu a right click raises over a paragraph.
    {rightClick: ['Field notes Page title', 0, 150]},
    {shot: 'text-menu'},
    {key: 'Escape'},
    {shot: 'menu-closed'},
  ],
  'so.tilo.canvas': [
    // The first screen's sheet: a dialog raised from it, then a sheet put
    // away, then two strokes drawn on the blank artwork.
    {click: 'New artwork…'},
    {shot: 'new-artwork'},
    {key: 'Escape'},
    {shot: 'dialog-closed'},
    {click: 'Put the welcome away'},
    {drag: [[420, 320], [700, 470]]},
    {shot: 'stroke'},
    {drag: [[470, 520], [760, 260]]},
    {shot: 'two-strokes'},
  ],
  'so.tilo.video-editor': [
    // Its whole window is one picture: the timeline is driven where it lies,
    // and the frames are compared as pictures.
    {drag: [[400, 700], [760, 700]]},
    {shot: 'timeline-drag'},
  ],
  'so.tilo.calendar': [
    // The month picker: a popover over the window, which both renders must
    // place and paint alike.
    {click: '2026-11-11 Month'},
    {shot: 'month-picker'},
    {key: 'Escape'},
    {shot: 'picker-closed'},
  ],
};

// One step of a scenario, worked in whichever render the tab shows. Returns
// what it found, so the two renders can be compared step by step.
async function step(tab, name, index, shot) {
  const where = async (target, role) => {
    if (Array.isArray(target) && typeof target[0] === 'number') return {x: target[0], y: target[1], given: target};
    // A name, and where from its centre the step lands: the body of a note is
    // painted text, whose only anchor is a control the page does name.
    const [name, dx = 0, dy = 0] = Array.isArray(target) ? target : [target];
    const hits = JSON.parse(await tab.evaluate(FIND(name, role)));
    if (!hits.length) {
      const names = JSON.parse(await tab.evaluate(NAMES));
      throw new Error(`nothing named ${JSON.stringify(name)}${role ? ` of role ${role}` : ''}; the tree names: ${names.join(' | ')}`);
    }
    return {...hits[0], x: hits[0].x + dx, y: hits[0].y + dy, matches: hits.length};
  };
  if (name.wait !== undefined) {
    // Polled rather than slept through: an app that seeds a note answers in
    // its own time, and the driver must wait for the name, not for a clock.
    for (let i = 0; i < 60; i++) {
      const hits = JSON.parse(await tab.evaluate(FIND(name.wait)));
      if (hits.length) return {found: hits[0]};
      await sleep(250);
    }
    const names = JSON.parse(await tab.evaluate(NAMES));
    throw new Error(`waited 15 s for ${JSON.stringify(name.wait)}; the tree names: ${names.join(' | ')}`);
  }
  if (name.seed !== undefined) {
    const seeded = await tab.evaluate(SEED_NOTE);
    return {seeded};
  }
  if (name.click !== undefined || name.rightClick !== undefined) {
    const at = await where(name.click ?? name.rightClick, name.role);
    const button = name.rightClick !== undefined ? 'right' : 'left';
    // Where the pointer is, before it presses: an app that opens a menu at
    // the pointer reads the last move it saw.
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x: at.x, y: at.y, button: 'none'});
    for (const type of ['mousePressed', 'mouseReleased']) {
      await tab.send('Input.dispatchMouseEvent', {type, x: at.x, y: at.y, button, clickCount: 1});
    }
    await settleFrames(tab);
    return {at: [Math.round(at.x), Math.round(at.y)], rect: at.rect, matches: at.matches};
  }
  if (name.drag !== undefined) {
    const [from, to] = [await where(name.drag[0]), await where(name.drag[1])];
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x: from.x, y: from.y, button: 'none'});
    await tab.send('Input.dispatchMouseEvent', {type: 'mousePressed', x: from.x, y: from.y, button: 'left', clickCount: 1});
    for (let k = 1; k <= 10; k++) {
      await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x: from.x + (to.x - from.x) * k / 10, y: from.y + (to.y - from.y) * k / 10, button: 'left', buttons: 1});
      await sleep(16);
    }
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: to.x, y: to.y, button: 'left', clickCount: 1});
    await settleFrames(tab);
    return {from: [Math.round(from.x), Math.round(from.y)], to: [Math.round(to.x), Math.round(to.y)]};
  }
  if (name.type !== undefined) {
    for (const ch of name.type) {
      await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch, unmodifiedText: ch});
      await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
      await sleep(40);
    }
    await settleFrames(tab);
    return {typed: name.type.length};
  }
  if (name.key !== undefined) {
    const MODIFIERS = {Alt: 1, Control: 2, Meta: 4, Shift: 8};
    const modifiers = (name.modifiers ?? []).reduce((bits, held) => bits | (MODIFIERS[held] ?? 0), 0);
    for (const type of ['keyDown', 'keyUp']) {
      await tab.send('Input.dispatchKeyEvent', {type, key: name.key, modifiers, windowsVirtualKeyCode: name.key === 'Escape' ? 27 : undefined});
    }
    await settleFrames(tab);
    return {key: name.key};
  }
  if (name.wheel !== undefined) {
    for (let i = 0; i < 6; i++) {
      await tab.send('Input.dispatchMouseEvent', {type: 'mouseWheel', x: width / 2, y: height / 2, deltaX: 0, deltaY: name.wheel});
      await sleep(50);
    }
    await settleFrames(tab);
    return {wheel: name.wheel};
  }
  if (name.shot !== undefined) {
    await settleFrames(tab);
    if (shot) writeFileSync(join(out, shot), await tab.screenshot());
    return {shot: name.shot};
  }
  throw new Error(`unknown step ${JSON.stringify(name)}`);
}

// A step's frames, drawn and painted: the next step acts on what the app
// shows, not on what it showed before the step.
async function settleFrames(tab) {
  let last = -1;
  for (let i = 0; i < 12; i++) {
    await sleep(150);
    const n = await tab.evaluate('window.renderMarks.presented.length');
    if (n === last) return;
    last = n;
  }
}

const server = await serve(bundle);
const results = [];
try {
  // The plain-DOM page: what typing and scrolling should feel like.
  {
    const {browser, tab} = await open(server, 'baseline.html');
    try {
      await sleep(1000);
      const baseline = {app: 'baseline'};
      baseline.interaction = await interact(tab, {x: 600, y: 120});
      console.log(JSON.stringify(baseline));
      results.push(baseline);
    } finally { await browser.close().catch(() => {}); }
  }
  const planned = scenariosOnly ? apps.filter(app => SCENARIOS[app]) : apps;
  for (const app of planned) {
    const result = {app, width};
    // A scenario's drags are points of the desktop layout: at a phone's
    // width only the first screen is compared, which is what the phone
    // measurement is for.
    const scenario = phone ? null : SCENARIOS[app];
    if (scenario) result.scenario = [];
    for (const render of ['dom', 'gpu']) {
      const {browser, tab} = await open(server, `app.html?app=${app}&render=${render}&theme=light`);
      try {
        await settle(tab);
        writeFileSync(join(out, `${app}${tag}-${render}.png`), await tab.screenshot());
        if (render === 'dom') {
          result.page = JSON.parse(await tab.evaluate(PAGE));
          result.field = JSON.parse(await tab.evaluate(FIELD));
        }
        // The same steps, in the same order, in both renders; the shots are
        // compared after both have run.
        for (const [index, name] of (scenario ?? []).entries()) {
          const shot = name.shot !== undefined ? `${app}${tag}-${render}-${name.shot}.png` : null;
          try {
            const outcome = await step(tab, name, index, shot);
            if (render === 'dom') (result.scenario[index] ??= {step: name}).dom = outcome;
            else (result.scenario[index] ??= {step: name}).gpu = outcome;
          } catch (error) {
            const message = String(error?.message ?? error).slice(0, 1200);
            if (render === 'dom') (result.scenario[index] ??= {step: name}).domError = message;
            else (result.scenario[index] ??= {step: name}).gpuError = message;
            // A step that cannot be taken leaves the rest of the scenario
            // meaningless: stop this render's run, keeping what it did.
            break;
          }
        }
        if (render === 'dom') {
          result.pageAfter = JSON.parse(await tab.evaluate(PAGE));
          writeFileSync(join(out, `${app}${tag}-dom-after.png`), await tab.screenshot());
          if (!scenario) result.interaction = await interact(tab, result.field);
          result.field = result.field ?? JSON.parse(await tab.evaluate(FIELD));
        }
        // What arrived over the network: an app whose tiles or photos load
        // in one render and not the other differs by them, not by the
        // renderer, and the counts say so.
        result[`${render}Remote`] = JSON.parse(await tab.evaluate(`JSON.stringify(
          performance.getEntriesByType('resource').filter(e => !e.name.startsWith(location.origin)).length)`));
        const errors = tab.log.filter(line => /^(EXC|ERROR)/.test(line)).slice(0, 6);
        if (errors.length) result[`${render}Errors`] = errors;
      } catch (error) {
        result[`${render}Error`] = String(error?.stack ?? error).slice(0, 2000);
      } finally { await browser.close().catch(() => {}); }
    }
    try {
      const compared = diff(decode(readFileSync(join(out, `${app}${tag}-dom.png`))), decode(readFileSync(join(out, `${app}${tag}-gpu.png`))));
      if (compared.image) writeFileSync(join(out, `${app}${tag}-diff.png`), compared.image);
      result.parity = {differingShare: compared.differingShare, meanDifference: compared.meanDifference, error: compared.error};
    } catch (error) { result.parity = {error: String(error)}; }
    // Each scenario step's frames, compared: a step whose renders differ is
    // named by the step that made them differ.
    for (const entry of result.scenario ?? []) {
      if (entry.step.shot === undefined) continue;
      try {
        const [dom, gpu] = ['dom', 'gpu'].map(render => join(out, `${app}${tag}-${render}-${entry.step.shot}.png`));
        const compared = diff(decode(readFileSync(dom)), decode(readFileSync(gpu)));
        if (compared.image) writeFileSync(join(out, `${app}${tag}-diff-${entry.step.shot}.png`), compared.image);
        entry.diff = {differingShare: compared.differingShare, meanDifference: compared.meanDifference, error: compared.error};
      } catch (error) { entry.diff = {error: String(error)}; }
    }
    console.log(JSON.stringify(result));
    results.push(result);
    writeFileSync(join(out, 'parity.json'), JSON.stringify(results, null, 1));
  }
} finally {
  await server.close();
}
