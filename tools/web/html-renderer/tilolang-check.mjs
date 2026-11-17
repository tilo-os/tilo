// A tilolang package on the web, worked in headless Chromium: build-tilolang-web.sh's
// output served, the package installed and mounted on the WebGPU canvas and as HTML, a
// screenshot of each, then a field typed into and a control pressed in each, with a
// screenshot after. What the page reported (and every console error) lands in check.json.
//
// usage: node tools/web/html-renderer/tilolang-check.mjs BUNDLE_DIR OUT_DIR [--package tl/<id>/]
//                                                       [--scenario all|NAME,...] [--open ROW]
//                                                       [--width N] [--height N]
//                                                       [--pages ROW,ROW,...]
// With --pages, each named row is opened in a page of its own, in both themes and both
// renderers, and shot as `<render>-<theme>-<row>.png`, before the interaction checks.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {serve} from '../chrome.mjs';
import {open, sleep} from './engines.mjs';

const bundle = resolve(process.argv[2]);
const out = resolve(process.argv[3]);
const option = name => { const at = process.argv.indexOf(name); return at < 0 ? null : process.argv[at + 1]; };
const pkg = option('--package') ?? 'tl/so.tilo.lang.gallery/';
const scenario = option('--scenario') ?? 'all';
// The row opened before typing: the gallery's controls.
const opens = option('--open') ?? 'Controls';
const width = Number(option('--width')) || 1280;
const height = Number(option('--height')) || 800;
const pages = (option('--pages') ?? '').split(',').filter(Boolean);
const TEXT = 'Hello web';
mkdirSync(out, {recursive: true});

// The first element of the mount that a person types into, and the first that toggles, as the
// page shows them: HTML's own elements, or the canvas's accessibility mirror over it.
const FIND = `((what) => {
  const root = document.getElementById('app');
  const selector = what === 'field'
    ? 'input:not([type=checkbox]):not([type=radio]), textarea, [contenteditable=true], [role=textbox]'
    : 'input[type=checkbox], [role=switch], [role=checkbox]';
  // As HTML, the page's own elements; the accessibility mirror over them is not what a press reaches.
  const dom = Boolean(root.querySelector('[data-path]'));
  for (const element of root.querySelectorAll(selector)) {
    if (dom && !element.closest('[data-path]')) continue;
    const box = element.getBoundingClientRect();
    if (box.width < 4 || box.height < 4) continue;
    if (box.bottom < 0 || box.top > innerHeight) continue;
    return JSON.stringify({x: box.left + box.width / 2, y: box.top + box.height / 2,
      tag: element.tagName, role: element.getAttribute('role'), label: element.getAttribute('aria-label'),
      checked: element.checked ?? element.getAttribute('aria-checked')});
  }
  return 'null';
})`;

// The smallest element of the mount that reads `name`, as the page shows it.
const NAMED = `((name) => {
  let best = null;
  const root = document.getElementById('app');
  const dom = Boolean(root.querySelector('[data-path]'));
  for (const element of root.querySelectorAll('*')) {
    if (dom && !element.closest('[data-path]')) continue;
    const said = (element.getAttribute('aria-label') ?? element.textContent ?? '').trim();
    if (said !== name) continue;
    const box = element.getBoundingClientRect();
    if (box.width < 4 || box.height < 4) continue;
    if (!best || box.width * box.height < best.area) best = {x: box.left + box.width / 2, y: box.top + box.height / 2, area: box.width * box.height};
  }
  return JSON.stringify(best);
})`;

async function settle(tab) {
  for (let i = 0, last = -1; i < 40; i++) {
    await sleep(250);
    const n = await tab.evaluate('window.renderMarks.presented.length');
    if (n === last) break;
    last = n;
  }
}

async function press(tab, at) {
  await tab.mouse.move(at.x, at.y);
  await tab.mouse.down({x: at.x, y: at.y});
  await tab.mouse.up({x: at.x, y: at.y});
  await sleep(400);
}

const server = await serve(bundle);
const report = {package: pkg, renders: {}, pages: {}};
try {
  for (const page of pages) {
    for (const theme of ['light', 'dark']) {
      for (const render of ['gpu', 'dom']) {
        const opened = await open({engine: 'chromium', width, height});
        const tab = opened.tab;
        const result = report.pages[`${render}-${theme}-${page}`] = {};
        try {
          await tab.colourScheme(theme);
          await tab.navigate(`${server.url}/app.html?package=${encodeURIComponent(pkg)}&scenario=${scenario}&render=${render}&theme=${theme}`);
          await tab.waitFor('window.renderMarks.presented.length > 0', {timeout: 120000});
          await settle(tab);
          // The row is pressed until its page shows: a press that lands
          // while the window is still settling can go unanswered.
          for (let tries = 0; tries < 3; tries++) {
            const at = JSON.parse(await tab.evaluate(`${NAMED}(${JSON.stringify(page)})`));
            if (!at || (result.opened && tries && Math.abs(at.y - result.opened.y) > 1)) break;
            result.opened = at;
            result.presses = tries + 1;
            await sleep(500);
            await press(tab, at);
            await settle(tab);
          }
          writeFileSync(join(out, `${render}-${theme}-${page.toLowerCase()}.png`), await tab.screenshot());
          if (render === 'dom' && theme === 'light') writeFileSync(join(out, `dom-${page.toLowerCase()}.html`), await tab.evaluate(`document.getElementById('app').outerHTML`));
        } catch (error) {
          result.error = String(error?.stack ?? error);
        } finally {
          result.log = tab.log.filter(line => /^(EXC|ERROR|WARN)|panic|failed/i.test(line)).slice(-12);
          await opened.close();
        }
      }
    }
  }
  for (const theme of ['light', 'dark']) {
    for (const render of ['gpu', 'dom']) {
      const opened = await open({engine: 'chromium', width, height});
      const tab = opened.tab;
      const result = {};
      report.renders[`${render}-${theme}`] = result;
      try {
        await tab.colourScheme(theme);
        await tab.navigate(`${server.url}/app.html?package=${encodeURIComponent(pkg)}&scenario=${scenario}&render=${render}&theme=${theme}`);
        await tab.waitFor('window.renderMarks.presented.length > 0', {timeout: 120000});
        await settle(tab);
        writeFileSync(join(out, `${render}-${theme}.png`), await tab.screenshot());
        if (theme !== 'light') continue;
        if (render === 'dom') writeFileSync(join(out, 'dom.html'), await tab.evaluate(`document.getElementById('app').outerHTML`));
        result.opened = JSON.parse(await tab.evaluate(`${NAMED}(${JSON.stringify(opens)})`));
        if (result.opened) {
          await sleep(500);
          await press(tab, result.opened);
          await settle(tab);
          writeFileSync(join(out, `${render}-opened.png`), await tab.screenshot());
        }
        result.field = JSON.parse(await tab.evaluate(`${FIND}('field')`));
        if (result.field) {
          await press(tab, result.field);
          for (const ch of TEXT) {
            await tab.key.down({key: ch, text: ch});
            await tab.key.up({key: ch});
            await sleep(60);
          }
          await settle(tab);
          result.typed = await tab.evaluate(`document.getElementById('app').innerText.includes(${JSON.stringify(TEXT)}) || [...document.querySelectorAll('#app input, #app textarea')].some(e => e.value.includes(${JSON.stringify(TEXT)}))`);
          writeFileSync(join(out, `${render}-typed.png`), await tab.screenshot());
        }
        result.toggle = JSON.parse(await tab.evaluate(`${FIND}('toggle')`));
        if (result.toggle) {
          await press(tab, result.toggle);
          await settle(tab);
          result.toggled = JSON.parse(await tab.evaluate(`${FIND}('toggle')`));
          writeFileSync(join(out, `${render}-toggled.png`), await tab.screenshot());
        }
      } catch (error) {
        result.error = String(error?.stack ?? error);
      } finally {
        result.log = tab.log.filter(line => /^(EXC|ERROR)/.test(line)).slice(0, 12);
        await opened.close();
      }
    }
  }
} finally {
  writeFileSync(join(out, 'check.json'), JSON.stringify(report, null, 2));
  await server.close();
}
console.log(JSON.stringify(report, null, 2));
