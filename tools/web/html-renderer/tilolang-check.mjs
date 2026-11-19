// A tilolang package on the web, worked in headless Chromium: build-tilolang-web.sh's
// output served, the package installed and mounted on the WebGPU canvas and as HTML, a
// screenshot of each, then a field typed into and a control pressed in each, with a
// screenshot after. What the page reported (and every console error) lands in check.json.
//
// usage: node tools/web/html-renderer/tilolang-check.mjs BUNDLE_DIR OUT_DIR [--package tl/<id>/]
//                                                       [--scenario all|NAME,...] [--open ROW]
//                                                       [--width N] [--height N]
//                                                       [--pages ROW,ROW,...] [--zone ZONE]
//                                                       [--press NAME,...]
//                                                       [--hover NAME@FX:FY;...]
// With --pages, each named place in the window's sidebar is chosen, and its page shot in both
// themes and both renderers as `<render>-<theme>-<place>.png`, before the interaction checks.
// The gallery's pages are its sidebar's places, so by default every one of them is walked.
// The page reads its clock in ZONE (an IANA name, UTC by default, as a device's capture does), and
// a package staged with a scenario shows the capture's instant, so its days and times are a
// capture's.
// With --press, on each page the control or row reading each name (a table row, a tree row, a
// card) is pointed at and shot as `<render>-<theme>-<place>-hovered-<name>.png`, then pressed
// and shot as `...-pressed-<name>.png`: the states a pointer and a choice give it.
// With --hover, on each page the pointer rests on the element reading each NAME (a chart by its
// title) at FX, FY of its box (fractions, 0 to 1), and the page is shot as
// `<render>-<theme>-<place>-hover-<name>-<n>.png`: what pointing there reads out.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {serve} from '../chrome.mjs';
import {open, sleep} from './engines.mjs';

const bundle = resolve(process.argv[2]);
const out = resolve(process.argv[3]);
const option = name => { const at = process.argv.indexOf(name); return at < 0 ? null : process.argv[at + 1]; };
const pkg = option('--package') ?? 'tl/so.tilo.lang.gallery/';
// The gallery is one window whose sidebar names every group: its places are its pages.
const GALLERY = 'tl/so.tilo.lang.gallery/';
const GALLERY_PLACES = ['Basics', 'Controls', 'Feedback', 'Backlog', 'Release board', 'Data',
  'Media', 'Places', 'Shell parts', 'Charts and views'];
const scenario = option('--scenario') ?? 'all';
// The row opened before typing: the gallery's controls.
const opens = option('--open') ?? 'Controls';
const width = Number(option('--width')) || 1280;
const height = Number(option('--height')) || 800;
const pages = (option('--pages') ?? (pkg === GALLERY ? GALLERY_PLACES.join(',') : ''))
  .split(',').filter(Boolean);
const zone = option('--zone') ?? 'UTC';
const presses = (option('--press') ?? '').split(',').filter(Boolean);
const hovers = (option('--hover') ?? '').split(';').filter(Boolean).map(spec => {
  const [name, at = '0.5:0.5'] = spec.split('@');
  const [fx, fy] = at.split(':').map(Number);
  return {name, fx, fy};
});
const TEXT = 'Hello web';
mkdirSync(out, {recursive: true});

// The first element of the mount that a person types into, and the first that toggles, as the
// page shows them: HTML's own elements, or the canvas's accessibility mirror over it.
const FIND = `((what) => {
  const root = document.getElementById('app');
  const selector = what === 'field'
    ? 'input:not([type=checkbox]):not([type=radio]), textarea, [contenteditable=true], [role=textbox]'
    : what === 'words'
      ? 'input:not([type]):not([role=spinbutton]), input[type=text]:not([role=spinbutton]), input[type=search], textarea, [role=textbox]'
      : 'input[type=checkbox], [role=switch], [role=checkbox]';
  // As HTML, the page's own elements; the accessibility mirror over them is not what a press reaches.
  const dom = Boolean(root.querySelector('[data-path]'));
  for (const element of root.querySelectorAll(selector)) {
    if (dom && !element.closest('[data-path]')) continue;
    // Text shown to be read is no field to type into.
    if (what !== 'toggle' && (element.readOnly || element.getAttribute('aria-readonly') === 'true')) continue;
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

// The box of the largest element of the mount that reads `name`: a chart, by its title.
const BOX = `((name) => {
  let best = null;
  const root = document.getElementById('app');
  const dom = Boolean(root.querySelector('[data-path]'));
  for (const element of root.querySelectorAll('*')) {
    if (dom && !element.closest('[data-path]')) continue;
    if ((element.getAttribute('aria-label') ?? '').trim() !== name) continue;
    const box = element.getBoundingClientRect();
    if (!best || box.width * box.height > best.width * best.height) best = {x: box.left, y: box.top, width: box.width, height: box.height};
  }
  return JSON.stringify(best);
})`;

// Whether an element of the mount other than the pressed row, `y` its middle, reads `name`: the
// page's title once the press showed the page.
const SHOWN = `((name, y) => {
  const root = document.getElementById('app');
  const dom = Boolean(root.querySelector('[data-path]'));
  for (const element of root.querySelectorAll('*')) {
    if (dom && !element.closest('[data-path]')) continue;
    const said = (element.getAttribute('aria-label') ?? element.textContent ?? '').trim();
    if (said !== name) continue;
    const box = element.getBoundingClientRect();
    if (box.width < 4 || box.height < 4) continue;
    if (Math.abs(box.top + box.height / 2 - y) > 4) return 'true';
  }
  return 'false';
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
          await tab.send('Emulation.setTimezoneOverride', {timezoneId: zone});
          await tab.navigate(`${server.url}/app.html?package=${encodeURIComponent(pkg)}&scenario=${scenario}&render=${render}&theme=${theme}`);
          await tab.waitFor('window.renderMarks.presented.length > 0', {timeout: 120000});
          await settle(tab);
          // One press on the place shows its page: the window's title reads the place's name
          // somewhere other than the row that was pressed.
          const at = JSON.parse(await tab.evaluate(`${NAMED}(${JSON.stringify(page)})`));
          if (at) {
            result.opened = at;
            await press(tab, at);
            await settle(tab);
            result.shown = JSON.parse(await tab.evaluate(`${SHOWN}(${JSON.stringify(page)}, ${at.y})`));
          }
          // Choosing a place in the gallery never reports a page it could not open.
          result.error_taught = await tab.evaluate(
            `document.body.innerText.includes('This app could not be opened')`,
          );
          writeFileSync(join(out, `${render}-${theme}-${page.toLowerCase()}.png`), await tab.screenshot());
          if (render === 'dom' && theme === 'light') writeFileSync(join(out, `dom-${page.toLowerCase()}.html`), await tab.evaluate(`document.getElementById('app').outerHTML`));
          for (const name of presses) {
            const named = JSON.parse(await tab.evaluate(`${NAMED}(${JSON.stringify(name)})`));
            const shot = state => join(out, `${render}-${theme}-${page.toLowerCase()}-${state}-${name.toLowerCase().replace(/[^a-z0-9]+/g, '-')}.png`);
            (result.presses ??= {})[name] = named;
            if (!named) continue;
            await tab.mouse.move(named.x, named.y);
            await settle(tab);
            writeFileSync(shot('hovered'), await tab.screenshot());
            await press(tab, named);
            await settle(tab);
            writeFileSync(shot('pressed'), await tab.screenshot());
          }
          for (const [index, hover] of hovers.entries()) {
            let box = JSON.parse(await tab.evaluate(`${BOX}(${JSON.stringify(hover.name)})`));
            (result.hovers ??= []).push({...hover, box});
            if (!box) continue;
            // The chart is scrolled into the window first, then pointed at where it now stands.
            if (box.y + box.height > height || box.y < 0) {
              await tab.mouse.move(width / 2, height / 2);
              await tab.mouse.wheel({x: width / 2, y: height / 2, dx: 0, dy: box.y + box.height / 2 - height / 2});
              await settle(tab);
              box = JSON.parse(await tab.evaluate(`${BOX}(${JSON.stringify(hover.name)})`));
              if (!box) continue;
            }
            const point = {x: box.x + box.width * hover.fx, y: box.y + box.height * hover.fy};
            await tab.mouse.move(point.x, point.y);
            await settle(tab);
            if (render === 'dom' && theme === 'light') {
              result.hovers[result.hovers.length - 1].readout = await tab.evaluate(
                `[...document.querySelectorAll('#app [role=tooltip]')].map(e => e.getAttribute('aria-label')).join(' | ')`);
            }
            const slug = hover.name.toLowerCase().replace(/[^a-z0-9]+/g, '-');
            writeFileSync(join(out, `${render}-${theme}-${page.toLowerCase().replace(/[^a-z0-9]+/g, '-')}-hover-${slug}-${index}.png`), await tab.screenshot());
          }
          // Words typed into the page's first field for them reach it, on both renders.
          const field = theme === 'light' ? JSON.parse(await tab.evaluate(`${FIND}('words')`)) : null;
          if (field) {
            result.field = field;
            await press(tab, field);
            for (const ch of TEXT) {
              await tab.key.down({key: ch, text: ch});
              await tab.key.up({key: ch});
              await sleep(60);
            }
            await settle(tab);
            result.typed = await tab.evaluate(`document.getElementById('app').innerText.includes(${JSON.stringify(TEXT)}) || [...document.querySelectorAll('#app input, #app textarea')].some(e => e.value.includes(${JSON.stringify(TEXT)}))`);
            writeFileSync(join(out, `${render}-typed-${page.toLowerCase()}.png`), await tab.screenshot());
          }
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
        await tab.send('Emulation.setTimezoneOverride', {timezoneId: zone});
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
