// Every menu of a page, opened in Chromium in both themes, set in the faces the page serves:
// fails when a label is cut short once the menu has arrived, when a row draws what it only says
// (an ask-first row shows its label and its marker, its sentence is for assistive technology),
// or when the page's face did not load, since a measurement in a fallback face says nothing
// about the product.
//
// usage: node tools/web/page-menus.mjs DIR
//
// DIR holds what `cargo run --example page-menus -- DIR` wrote (`light.html`, `dark.html`,
// `menus.json`) and the page's web fonts in `DIR/fonts`.
import {readFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve} from './chrome.mjs';

const dir = resolve(process.argv[2] ?? '');
const menus = JSON.parse(readFileSync(join(dir, 'menus.json'), 'utf8'));
// The page's own patch steps, as crates/core/tilo-server/src/web/page/page.mjs applies them.
const APPLY = `(steps) => {
  const element = id => document.querySelector('[data-p="' + id + '"]');
  const fromHtml = html => { const t = document.createElement('template'); t.innerHTML = html; return t.content.firstElementChild; };
  for (const step of steps) {
    if (step.replace) element(step.replace.id)?.replaceWith(fromHtml(step.replace.html));
    else if (step.insert) { const parent = element(step.insert.parent); parent.insertBefore(fromHtml(step.insert.html), parent.children[step.insert.at] ?? null); }
    else if (step.remove) element(step.remove.id)?.remove();
  }
}`;
const ROWS = `[...document.querySelectorAll('[data-layer] [role=menu] [role=menuitem]')].map(row => {
  const words = row.querySelector('.tilo-title') ?? row;
  return {label: row.textContent.trim(), title: words.textContent.trim(), shown: words.clientWidth, needs: words.scrollWidth,
          says: row.getAttribute('aria-description'), name: row.getAttribute('aria-label') ?? '',
          trailing: row.querySelector('.tilo-detail')?.textContent.trim() ?? null};
})`;

const server = await serve(dir);
const browser = await launch();
const failures = [];
try {
  for (const scheme of ['light', 'dark']) {
    for (const {menu, patches} of menus) {
      const tab = await browser.tab({});
      await tab.send('Emulation.setDeviceMetricsOverride', {width: 720, height: 480, deviceScaleFactor: 1, mobile: false});
      await tab.navigate(`${server.url}/${scheme}.html`);
      await tab.waitFor(`document.readyState === 'complete'`, {timeout: 20000});
      await tab.evaluate(`document.fonts.ready.then(() => true)`);
      await tab.evaluate(`(${APPLY})(${JSON.stringify(patches)})`);
      // The menu as it rests: every motion it arrived by run to its end.
      await tab.evaluate(`document.fonts.ready.then(() => { document.getAnimations().forEach(a => a.finish()); return true; })`);
      const face = await tab.evaluate(`[...document.fonts].some(f => f.family.replace(/"/g, '') === 'Tilo Sans' && f.status === 'loaded')`);
      const rows = await tab.evaluate(ROWS);
      const cut = rows.filter(row => row.needs > row.shown + 0.5);
      console.log(`${scheme} ${menu}: ${rows.map(r => `${r.label} ${r.shown}/${r.needs}`).join(', ')}`);
      if (!face) failures.push(`${scheme} ${menu}: the page's face did not load`);
      if (!rows.length) failures.push(`${scheme} ${menu}: the menu showed no rows`);
      for (const row of cut) failures.push(`${scheme} ${menu}: "${row.label}" is cut short (${row.shown} of ${row.needs} px)`);
      // A shortcut is drawn at the row's trailing edge and said too; nothing else it says is drawn.
      for (const row of rows.filter(row => row.says && row.says !== row.trailing && row.label.includes(row.says))) {
        failures.push(`${scheme} ${menu}: "${row.title}" draws what it only says: "${row.says}"`);
      }
      // An ask-first row says so to assistive technology and shows only its label and marker.
      for (const row of rows.filter(row => row.says?.startsWith('Asks first'))) {
        if (row.label !== `${row.name}\u2026`) failures.push(`${scheme} ${menu}: an ask-first row shows "${row.label}", not "${row.name}…"`);
      }
      await tab.close();
    }
  }
} finally {
  await browser.close();
  await server.close();
}
if (failures.length) {
  console.error(failures.join('\n'));
  process.exit(1);
}
