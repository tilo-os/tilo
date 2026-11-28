// Every menu of a page, opened in Chromium in light, dark and high contrast, set in the faces
// the page serves: fails when a label is cut short once the menu has arrived, when a row draws
// what it only says (an ask-first row shows its label and its marker, its sentence is for
// assistive technology), when the page's face did not load, since a measurement in a fallback
// face says nothing about the product, or when the page draws the menu otherwise than the app:
// its rules between sections, a toggle's check, a nested menu's chevron, a destructive row's
// danger ink, one column its labels start at, its labels as wide as the app sets them, the
// panel's own edge and size, its button held and chevroned, and the edge of the buttons at rest.
//
// usage: node tools/web/page-menus.mjs DIR
//
// DIR holds what `cargo run --example page-menus -- DIR` wrote (`light.html`, `dark.html`,
// `contrast.html`, `menus.json`) and the page's web fonts in `DIR/fonts`.
import {readFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve} from './chrome.mjs';

const dir = resolve(process.argv[2] ?? '');
const menus = JSON.parse(readFileSync(join(dir, 'menus.json'), 'utf8'));
// The page's own patch steps, as crates/core/tilo-runtime/src/device/keep/web/page/page.mjs applies them.
const APPLY = `(steps) => {
  const element = id => document.querySelector('[data-p="' + id + '"]');
  const fromHtml = html => { const t = document.createElement('template'); t.innerHTML = html; return t.content.firstElementChild; };
  for (const step of steps) {
    if (step.replace) element(step.replace.id)?.replaceWith(fromHtml(step.replace.html));
    else if (step.insert) { const parent = element(step.insert.parent); parent.insertBefore(fromHtml(step.insert.html), parent.children[step.insert.at] ?? null); }
    else if (step.remove) element(step.remove.id)?.remove();
  }
}`;
const ROWS = `[...document.querySelectorAll('[data-layer] [role=menu] [role^=menuitem]')].map(row => {
  const words = row.querySelector('.tilo-title') ?? row;
  return {label: row.textContent.trim(), title: words.textContent.trim(), shown: words.clientWidth, needs: words.scrollWidth,
          says: row.getAttribute('aria-description'), name: row.getAttribute('aria-label') ?? '',
          trailing: row.querySelector('.tilo-detail')?.textContent.trim() ?? null};
})`;

// What the open menu draws, row by row in order, and the colours it should draw them in.
const DRAWN = `(() => {
  const menu = document.querySelector('[data-layer] > [role=menu]');
  const root = getComputedStyle(document.documentElement);
  const resolved = (property, value) => {
    const probe = document.createElement('div');
    probe.style[property] = value;
    document.body.append(probe);
    const answer = getComputedStyle(probe)[property];
    probe.remove();
    return answer;
  };
  const token = name => root.getPropertyValue(name).trim();
  const rows = [...menu.children].filter(el => el.matches('hr, [role^=menuitem]')).map(el => {
    if (el.tagName === 'HR') return {separator: true};
    const first = el.firstElementChild, last = el.lastElementChild;
    return {
      separator: false,
      label: el.querySelector('.tilo-title')?.textContent.trim(),
      checked: el.hasAttribute('aria-checked') ? el.getAttribute('aria-checked') === 'true' : null,
      lead: first?.matches('.tilo-glyph') ? (first.querySelector('path') ? 'glyph' : 'empty') : 'none',
      chevron: !!last?.matches('svg.tilo-glyph') && last !== first,
      danger: el.hasAttribute('data-danger'),
      color: getComputedStyle(el).color,
      textX: el.querySelector('.tilo-title').getBoundingClientRect().left,
      width: (range => (range.selectNodeContents(el.querySelector('.tilo-title')), range.getBoundingClientRect().width))(document.createRange()),
    };
  });
  const trigger = document.querySelector('button[aria-expanded=true]:not([role^=menuitem])');
  return {
    rows,
    danger: resolved('color', token('--tilo-danger')),
    edge: resolved('color', token('--tilo-floating-edge')),
    held: resolved('backgroundColor', token('--tilo-held')),
    panel: (({width, height}) => [width, height])(menu.getBoundingClientRect()),
    trigger: trigger && getComputedStyle(trigger).backgroundColor,
    // The other buttons at rest wear the face's edge (high contrast's hairline, else none).
    faceEdge: token('--tilo-face-edge') === 'transparent' ? null : resolved('color', token('--tilo-face-edge')),
    restShadows: [...document.querySelectorAll('button[aria-expanded=false]:not([role^=menuitem])')].map(b => getComputedStyle(b).boxShadow),
    triggerChevron: !!trigger?.lastElementChild?.matches('svg.tilo-glyph'),
    triggerChevronSize: (box => box && [box.width, box.height])(trigger?.lastElementChild?.getBoundingClientRect()),
    iconSize: parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--tilo-menu-icon')),
    shadow: getComputedStyle(menu).boxShadow,
  };
})()`;

const server = await serve(dir);
const browser = await launch();
const failures = [];
try {
  for (const scheme of ['light', 'dark', 'contrast']) {
    for (const {menu, rows: items, panel, patches} of menus) {
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
      const drawn = await tab.evaluate(DRAWN);
      const fail = words => failures.push(`${scheme} ${menu}: ${words}`);
      if (drawn.rows.length !== items.length) {
        fail(`draws ${drawn.rows.length} rows and rules where the app has ${items.length}`);
      }
      items.forEach((item, index) => {
        const row = drawn.rows[index];
        if (!row) return;
        if (item.separator !== row.separator) {
          fail(`row ${index + 1} is ${row.separator ? 'a rule' : `"${row.label}"`} where the app draws ${item.separator ? 'a rule' : 'a row'}`);
          return;
        }
        if (item.separator) return;
        if (item.checked !== null && row.checked !== item.checked) fail(`"${row.label}" is not shown ${item.checked ? 'on' : 'off'}`);
        if (item.checked === true && row.lead !== 'glyph') fail(`"${row.label}" is on but draws no check`);
        if (item.checked === false && row.lead === 'glyph') fail(`"${row.label}" is off but draws a mark`);
        // The page sets a label as the app does: same face, size, weight and spacing, unhinted.
        if (item.width !== null && Math.abs(row.width - item.width) > 1) {
          fail(`"${row.label}" is set ${row.width.toFixed(1)} px wide where the app sets it ${item.width.toFixed(1)} px`);
        }
        if (item.submenu !== row.chevron) fail(`"${row.label}" ${item.submenu ? 'opens a menu but draws no chevron' : 'draws a chevron it does not open'}`);
        if (item.danger !== (row.color === drawn.danger)) fail(`"${row.label}" is ${item.danger ? 'not drawn' : 'drawn'} in danger ink (${row.color})`);
      });
      const labelled = drawn.rows.filter(row => !row.separator);
      if (labelled.some(row => row.lead !== 'none') && labelled.some(row => row.lead === 'none')) {
        fail('some rows keep a leading column and some do not');
      }
      const columns = new Set(labelled.map(row => Math.round(row.textX)));
      if (columns.size > 1) fail(`labels start at ${[...columns].join(', ')} px, not one column`);
      if (!drawn.shadow.includes(drawn.edge)) fail(`the panel's edge is not the theme's floating edge ${drawn.edge}: ${drawn.shadow}`);
      if (drawn.faceEdge && !drawn.restShadows.every(shadow => shadow.includes(drawn.faceEdge))) {
        fail(`a button at rest has no edge of ${drawn.faceEdge}: ${drawn.restShadows.join(' | ')}`);
      }
      if (!drawn.faceEdge && drawn.restShadows.some(shadow => shadow !== 'none' && !/rgba\([^)]*, 0\)/.test(shadow))) {
        fail(`a button at rest draws an edge the app does not: ${drawn.restShadows.join(' | ')}`);
      }
      if (!drawn.triggerChevron) fail('its button draws no down chevron, where the app draws one');
      // The chevron is the app's small icon, as wide as it is tall, never stretched by the words beside it.
      if (drawn.triggerChevron && drawn.triggerChevronSize.some(length => Math.abs(length - drawn.iconSize) > 0.01)) {
        fail(`its button's chevron is ${drawn.triggerChevronSize.join('×')} px where the app's is ${drawn.iconSize} px`);
      }
      if (drawn.trigger !== drawn.held) fail(`its button is not held while it is open (${drawn.trigger}, not ${drawn.held})`);
      if (drawn.panel.some((length, axis) => Math.abs(length - panel[axis]) > 0.5)) {
        fail(`the panel is ${drawn.panel.join('×')} px where the app's is ${panel.join('×')} px`);
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
