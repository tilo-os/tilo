// What an app of a bundle calls its controls: every name assistive technology
// reads, with the role and where the render put it. A parity scenario names
// its targets by these names, never by a coordinate, so this is what writing
// or fixing one is done from.
//
// The names are read from the semantic elements themselves — the same tree a
// screen reader walks, and the one a scenario drives — not from the widget
// tree the page keeps for inspection, which names containers and stops there.
//
// `--unnamed` lists the other half: what a person can act on and assistive
// technology has no name for, which is both what a scenario cannot reach and
// what a screen reader reads as an anonymous button.
//
// usage: node tools/web/html-renderer/names.mjs BUNDLE_DIR APP [--render dom|gpu] [--width N] [--seed note] [--unnamed]
import {readFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {serve} from '../chrome.mjs';
import {open, sleep} from './engines.mjs';

const bundle = resolve(process.argv[2]);
const app = process.argv[3];
const option = name => { const at = process.argv.indexOf(name); return at < 0 ? null : process.argv[at + 1]; };
const render = option('--render') ?? 'dom';
const width = Number(option('--width')) || 1280;
// --seed note: a note made through the app's own import action first, so the
// names of an app whose first screen has none are the ones a scenario needs.
const seed = option('--seed');
const unnamed = process.argv.includes('--unnamed');
if (!app) throw new Error('usage: node names.mjs BUNDLE_DIR APP [--render dom|gpu] [--width N] [--seed note] [--unnamed]');
const phone = width < 500;
const known = JSON.parse(readFileSync(join(bundle, 'apps.json'), 'utf8')).applications.map(entry => entry.id);
if (!known.includes(app)) throw new Error(`no app ${app}; the bundle has: ${known.join(' ')}`);

// Every semantic element on screen, with what a screen reader would say about
// it. An element a person can act on has a role a reader announces as
// pressable, or takes the keyboard.
const NAMED = `(() => {
  const out = [];
  for (const element of document.querySelectorAll('[data-tilo-semantic]')) {
    const r = element.getBoundingClientRect();
    const words = [element.getAttribute('aria-label'), element.value].filter(Boolean).join(' ').replace(/\\s+/g, ' ').trim();
    out.push({
      role: (element.getAttribute('role') ?? element.localName).toLowerCase(),
      name: words,
      rect: [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)],
      target: element.dataset.tiloSemantic ?? null,
      tabIndex: element.tabIndex,
      disabled: element.getAttribute('aria-disabled') === 'true',
      onScreen: r.width > 0 && r.height > 0 && r.right > 0 && r.bottom > 0 && r.left < innerWidth && r.top < innerHeight,
    });
  }
  return JSON.stringify(out);
})()`;
const MARKDOWN = Array.from({length: 20}, (_, i) => i % 10 === 0
  ? `## Section ${i / 10 + 1}`
  : `Paragraph ${i}: the quick brown fox jumps over the lazy dog while notes keep every thought close at hand.`).join('\n\n');
const SEED_NOTE = `(async () => {
  const actions = window.mount.actions();
  const importing = actions.find(a => /import/i.test(a.address.action) && /markdown/i.test(a.address.action));
  if (!importing) return 'no import action: ' + actions.map(a => a.address.action).join(',');
  await window.mount.invokeAction(importing.address.resource, importing.address.action,
    {title: 'Field notes', markdown: ${JSON.stringify(MARKDOWN)}, images: [], note: null});
  return 'seeded';
})()`;

const server = await serve(bundle);
const opened = await open({engine: 'chromium', width, height: phone ? 844 : 800, scale: phone ? 3 : 1, mobile: phone});
const tab = opened.tab;
try {
  await tab.colourScheme('light');
  await tab.navigate(`${server.url}/app.html?app=${app}&render=${render}&theme=light`);
  await tab.waitFor('window.renderMarks.presented[0]', {timeout: 90000});
  // Settled: what the first screen holds, not what it held on the way there.
  for (let i = 0, last = -1; i < 40; i++) {
    await sleep(250);
    const n = await tab.evaluate('window.renderMarks.presented.length');
    if (n === last) break;
    last = n;
  }
  if (seed === 'note') {
    console.log(`seed: ${await tab.evaluate(SEED_NOTE)}`);
    await sleep(2000);
  }
  const all = JSON.parse(await tab.evaluate(NAMED)).filter(node => node.onScreen);
  const ACTS = ['button', 'togglebutton', 'checkbox', 'radio', 'switch', 'tab', 'menuitem', 'option', 'listitem', 'treeitem', 'link', 'slider', 'textbox', 'searchbox', 'combobox', 'spinbutton'];
  if (unnamed) {
    const silent = all.filter(node => !node.name && (ACTS.includes(node.role) || node.tabIndex >= 0));
    for (const node of silent) console.log(`${node.role.padEnd(12)} unnamed @${node.rect.join(',')} target=${node.target ?? '-'}`);
    console.log(`${silent.length} unnamed of ${all.length} semantic elements`);
  } else {
    for (const node of all) console.log(`${node.role.padEnd(12)} ${node.name || '(no name)'} @${node.rect.join(',')}`);
    console.log(`${all.length} semantic elements`);
  }
} finally {
  await opened.close();
  await server.close();
}
