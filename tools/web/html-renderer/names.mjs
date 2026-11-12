// What an app of a bundle calls its controls: every name assistive technology
// reads, with the role and where the render put it. A parity scenario names
// its targets by these names, never by a coordinate, so this is what writing
// or fixing one is done from.
//
// usage: node tools/web/html-renderer/names.mjs BUNDLE_DIR APP [--render dom|gpu] [--width N] [--seed note]
import {readFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../chrome.mjs';

const bundle = resolve(process.argv[2]);
const app = process.argv[3];
const option = name => { const at = process.argv.indexOf(name); return at < 0 ? null : process.argv[at + 1]; };
const render = option('--render') ?? 'dom';
const width = Number(option('--width')) || 1280;
// --seed note: a note made through the app's own import action first, so the
// names of an app whose first screen has none are the ones a scenario needs.
const seed = option('--seed');
if (!app) throw new Error('usage: node names.mjs BUNDLE_DIR APP [--render dom|gpu] [--width N]');
const phone = width < 500;
const known = JSON.parse(readFileSync(join(bundle, 'apps.json'), 'utf8')).applications.map(entry => entry.id);
if (!known.includes(app)) throw new Error(`no app ${app}; the bundle has: ${known.join(' ')}`);

const TREE = `(() => {
  const out = [];
  const walk = (n, depth) => { if (!n) return; const s = n.semantics ?? {};
    const words = [s.label, s.value, s.name].filter(Boolean).join(' ').replace(/\\s+/g, ' ').trim();
    if (words) out.push({role: s.role ?? null, name: words.slice(0, 90), rect: n.rect ? n.rect.map(Math.round) : null, depth});
    for (const child of n.children ?? []) walk(child, depth + 1); };
  walk(window.mount.state()?.semantics?.root, 0);
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
const browser = await launch({args: [`--window-size=${width},${phone ? 844 : 800}`]});
try {
  const tab = await browser.tab();
  await tab.send('Emulation.setDeviceMetricsOverride', {width, height: phone ? 844 : 800, deviceScaleFactor: phone ? 3 : 1, mobile: phone});
  await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: 'light'}]});
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
  for (const node of JSON.parse(await tab.evaluate(TREE))) {
    console.log(`${'  '.repeat(node.depth)}${node.role ?? '-'} · ${node.name}${node.rect ? ` @${node.rect.join(',')}` : ''}`);
  }
} finally {
  await browser.close().catch(() => {});
  await server.close();
}
