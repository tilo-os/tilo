// Checks that a control which only arrives on a screen the app draws after
// the page is already up can be operated by the name assistive technology
// reads, with the interface shown as HTML (`dom: true`): the semantic
// element is adopted into the painted element of the widget it describes, and
// the check looks for it in the page's own tree, presses every element that
// carries the name, and reads back what the runtime answered. It fails —
// exit code 1 — when no control arrived late, when none could be pressed, or
// when a press was not performed: a screen reader that presses a control
// expects it to run, not to be told it cannot.
//
// usage: node tools/web/html-renderer/check-activation.mjs BUNDLE_DIR [--names LABEL,LABEL] [--dump]
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../chrome.mjs';

const bundle = resolve(process.argv[2]);
const out = process.argv[3] ?? bundle;
// The roles a screen reader presses: a control, rather than the container it
// stands in or the label beside it.
const CONTROL_ROLES = ['Button', 'ToggleButton', 'MenuItem', 'Tab', 'TreeItem', 'ListBoxOption', 'ListItem', 'Checkbox', 'Switch'];
const isControl = role => CONTROL_ROLES.includes(role);
const namesAt = process.argv.indexOf('--names');
const wanted = namesAt < 0 ? null : process.argv[namesAt + 1].split(',').filter(Boolean);
const dump = process.argv.includes('--dump');
// `--render gpu` runs the same presses on the WebGPU canvas, to say whether a
// refusal is the page's doing or the runtime's.
const render = process.argv.includes('--render') ? process.argv[process.argv.indexOf('--render') + 1] : 'dom';
mkdirSync(out, {recursive: true});
const sandbox = process.getuid?.() === 0 ? ['--no-sandbox'] : [];

// What the note the check opens is made of: long enough that a screen the
// page did not draw at its first frame arrives with it.
const MARKDOWN = Array.from({length: 24}, (_, i) => i % 8 === 0
  ? `## Section ${i / 8 + 1}`
  : `Paragraph ${i}: the quick brown fox jumps over the lazy dog while notes keep every thought close at hand.`).join('\n\n');

const SEED = `(async () => {
  const actions = window.mount.actions();
  const importing = actions.find(a => /markdown/i.test(a.address.action) && /import/i.test(a.address.action));
  if (!importing) return 'no import action';
  const answer = await window.mount.invokeAction(importing.address.resource, importing.address.action,
    {title: 'Field notes', markdown: ${JSON.stringify(MARKDOWN)}, images: [], note: null});
  return JSON.stringify(answer);
})()`;

const OPEN = `(() => {
  const root = window.mount.state()?.semantics?.root;
  const walk = node => {
    const name = node?.semantics?.name;
    if (name === 'Field notes' && node.rect) return [node.rect[0] + Math.min(node.rect[2] / 2, 80), node.rect[1] + node.rect[3] / 2];
    for (const child of node.children ?? []) { const found = walk(child); if (found) return found; }
    return null;
  };
  return walk(root);
})()`;

// Every control the page's tree offers, by the name assistive technology
// reads, and whether its element sits inside the painted element of the widget
// it describes (one tree) or in the mirror's own layer.
const MIRROR = `(() => {
  const rows = [];
  const walk = node => {
    const s = node?.semantics;
    if (s?.name) rows.push([String(s.role), s.name]);
    for (const child of node.children ?? []) walk(child);
  };
  walk(window.mount.state()?.semantics?.root);
  const elements = [...document.querySelectorAll('[data-tilo-semantic]')];
  return {
    names: rows,
    adopted: elements.filter(e => e.closest('.tilo-dom')).length,
    elements: elements.length,
  };
})()`;

// Press the controls the name reads as, where a screen reader finds them:
// the page's own elements, every one of them when a widget and a label in a
// bar say the same words. A disabled control is not pressed — there is
// nothing there to perform. Returns what the runtime answered each.
const PRESS = wanted => `(async () => {
  const wanted = ${JSON.stringify(wanted)};
  const elements = [...document.querySelectorAll('[data-tilo-semantic]')]
    .filter(e => e.getAttribute('aria-label') === wanted);
  if (!elements.length) return {found: false, pressed: 0, answers: []};
  const answers = [];
  for (const element of elements) {
    const role = element.getAttribute('role');
    const adopted = Boolean(element.closest('.tilo-dom'));
    if (element.getAttribute('aria-disabled') === 'true' || element.disabled) {
      answers.push({role, adopted, result: 'disabled'});
      continue;
    }
    const before = window.renderMarks.reports.length;
    element.click();
    for (let i = 0; i < 40 && window.renderMarks.reports.length === before; i++) await new Promise(done => setTimeout(done, 25));
    const report = window.renderMarks.reports.slice(before).find(row => row.semanticAction === 'activate');
    answers.push({role, adopted, result: report?.semanticResult ?? null});
  }
  return {found: true, pressed: answers.filter(answer => answer.result !== 'disabled').length, answers};
})()`;

const browser = await launch({args: ['--window-size=1280,800', ...sandbox]});
const server = await serve(bundle);
const result = {};
try {
  const tab = await browser.tab();
  await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
  await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: 'light'}]});
  await tab.navigate(`${server.url}/app.html?app=so.tilo.notes&render=${render}`);
  await tab.waitFor('!!window.mount && !!window.renderMarks', {timeout: 60000});
  let last = -1;
  for (let i = 0; i < 60; i++) {
    await sleep(250);
    const seen = await tab.evaluate('window.renderMarks.presented.length');
    if (seen === last && i > 4) break;
    last = seen;
  }
  // What the page offers before the note arrives.
  const first = await tab.evaluate(MIRROR);
  result.seed = await tab.evaluate(SEED);
  await sleep(1500);
  const item = await tab.evaluate(OPEN);
  result.item = item;
  if (item) {
    for (const type of ['mousePressed', 'mouseReleased']) {
      await tab.send('Input.dispatchMouseEvent', {type, x: item[0], y: item[1], button: 'left', clickCount: 1});
    }
    await sleep(1500);
  }
  // What it offers once the note's screen — a tab drawn after the page was up
  // — has arrived.
  const shown = await tab.evaluate(MIRROR);
  result.controls = {before: first.names.length, after: shown.names.length, adoptedBefore: first.adopted, adoptedAfter: shown.adopted, elements: shown.elements};
  if (dump) writeFileSync(join(out, 'activation-names.json'), JSON.stringify(shown, null, 1));
  const key = ([role, name]) => JSON.stringify([role, name]);
  const had = new Set(first.names.map(key));
  const late = shown.names.filter(row => !had.has(key(row)));

  result.late = late.map(([role, name]) => `${role}: ${name}`);
  const operable = late.filter(([role]) => isControl(role));
  const labels = wanted ?? (operable.length ? operable : late).slice(0, 8).map(([, name]) => name);
  result.pressed = [];
  for (const label of labels) {
    const answer = await tab.evaluate(PRESS(label));
    result.pressed.push({label, ...answer});
    await sleep(400);
  }
  result.log = tab.log.slice(0, 10);
} catch (error) {
  result.error = String(error?.stack ?? error);
} finally {
  await server.close();
  await browser.close().catch(() => {});
}
console.log(JSON.stringify(result, null, 1));
// A control that arrives after the page is up is operable by the name a
// screen reader reads, or this check fails: pressing it must reach the
// runtime and be performed there, not be found missing or refused.
const late = result.late ?? [];
const refused = (result.pressed ?? []).flatMap(row => row.answers.filter(answer => answer.result !== 'accepted' && answer.result !== 'disabled').map(answer => `${row.label} (${answer.role ?? 'no role'}: ${answer.result ?? 'no answer'})`));
const missing = (result.pressed ?? []).filter(row => !row.found).map(row => row.label);
const pressed = (result.pressed ?? []).reduce((count, row) => count + row.pressed, 0);
const refusals = [];
if (result.error) refusals.push(`the page failed: ${result.error}`);
if (!late.length) refusals.push('no control arrived after the page was up, so nothing was proved');
if (!result.pressed?.length) refusals.push('no control was pressed');
if (missing.length) refusals.push(`names not on the page: ${missing.join(', ')}`);
if (!pressed) refusals.push('no element was found for any of the names');
if (refused.length) refusals.push(`presses the runtime did not perform: ${refused.join(', ')}`);
console.error(`${late.length} controls arrived after the page was up (${late.filter(row => isControl(row.split(':')[0])).length} of them controls); ` +
  `pressed ${pressed} element(s) by name: ${(result.pressed ?? []).map(row => `${row.label} (${row.answers.map(answer => answer.result ?? 'none').join('/')})`).join(', ')}`);
if (refusals.length) {
  console.error(`activation by name failed: ${refusals.join('; ')}`);
  process.exitCode = 1;
} else {
  console.error('every control the page offered by name was pressed and performed');
}
