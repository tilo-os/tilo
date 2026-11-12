// The browsers the HTML renderer's tools drive: Chromium, Firefox and WebKit,
// behind one small tab interface, so a scenario is written once and worked the
// same way in each.
//
// Chromium is Playwright's Chrome for Testing, driven over the DevTools
// protocol by `chrome.mjs` — no package to install, which is why the other
// tools keep working on a bare machine. Firefox and WebKit are Playwright's
// own builds and need `playwright-core`, resolved from `$PLAYWRIGHT` (the path
// of its `index.mjs`), else from a `playwright-core` or `playwright` this
// script can import. A missing one says so, with what to install.
//
// What a tab offers, and what each engine maps it to:
//
//   tab.navigate(url)                  goto, and wait for the load
//   tab.evaluate(expr)                 the expression's value
//   tab.waitFor(expr, {timeout})       polls `expr` until it is true
//   tab.viewport({width, height, scale, mobile})
//   tab.colourScheme('light'|'dark')
//   tab.mouse.move(x, y)               a move with no button held
//   tab.mouse.down({x, y, button})     press at a point
//   tab.mouse.up({x, y, button})       release there
//   tab.mouse.wheel({x, y, dx, dy})    one wheel step
//   tab.key.down({key, text})          a key pressed, with what it commits
//   tab.key.up({key})
//   tab.screenshot()                   a PNG of the viewport
//   tab.log                            console errors and page exceptions
//   tab.close()
//
// A pointer press is never a click: an app that opens a menu where the pointer
// is reads the move before it, and one that drags must see the press stay
// down, so the tools move, press and release themselves.
import {launch as launchChromium, sleep} from '../chrome.mjs';

const engines = new Set(['chromium', 'firefox', 'webkit']);

/** The Playwright build of `engine`, or a sentence saying what to install. */
async function playwright() {
  const candidates = [
    process.env.PLAYWRIGHT,
    'playwright-core',
    'playwright',
  ].filter(Boolean);
  for (const candidate of candidates) {
    try {
      const module = await import(candidate);
      // `playwright` exports the engines beside `default`; `playwright-core`
      // the same.
      if (module.firefox && module.webkit) return module;
    } catch {
      // Not this one: the next.
    }
  }
  throw new Error(
    'Firefox and WebKit are Playwright\'s builds, and no Playwright is installed: set PLAYWRIGHT '
    + 'to the path of its index.mjs (PLAYWRIGHT=/path/playwright-core/index.mjs), or install '
    + 'playwright-core@1.64.1 somewhere node can resolve it (`node tools/web/html-renderer/engines.sh install`).',
  );
}

/** The buttons a page sees, by the name the tools use. */
const BUTTONS = {left: 0, middle: 1, right: 2};

/** A tab of a Playwright browser: the same interface `chrome.mjs`'s tab has,
 * for the parts these tools use, with an input surface of its own. */
function playwrightTab(page, context, browser) {
  const log = [];
  page.on('console', message => {
    if (message.type() === 'error') log.push(`ERROR ${message.text()}`);
  });
  page.on('pageerror', error => log.push(`EXC ${error.message}`));
  const state = {scale: 1};
  return {
    log,
    async close() {
      await context.close().catch(() => {});
      await browser.close().catch(() => {});
    },
    async navigate(url) {
      await page.goto(url, {waitUntil: 'load', timeout: 120_000});
    },
    async evaluate(expression) {
      // The tools hand over expressions and small function bodies; an object
      // comes back as an object, never as a handle.
      try {
        return await page.evaluate(expression);
      } catch (error) {
        // A page that threw (an aborted navigation, a promise the page
        // rejected) answers like the DevTools protocol's rejected evaluation.
        throw new Error(String(error?.message ?? error));
      }
    },
    async waitFor(expression, {timeout = 30_000} = {}) {
      const started = Date.now();
      for (;;) {
        if (await page.evaluate(expression).catch(() => false)) return true;
        if (Date.now() - started > timeout) throw new Error(`waited ${timeout} ms for ${expression}`);
        await sleep(50);
      }
    },
    async viewport({width, height, scale = 1, mobile = false}) {
      state.scale = scale;
      await page.setViewportSize({width, height});
      // Playwright's own scale lives on the context, which is fixed for the
      // page's life: the tools open a page per render anyway, and this is the
      // one call a page may make before it has drawn anything.
    },
    async colourScheme(scheme) {
      await page.emulateMedia({colorScheme: scheme});
    },
    mouse: {
      async move(x, y) { await page.mouse.move(x, y); },
      async down({x, y, button = 'left'}) {
        await page.mouse.move(x, y);
        await page.mouse.down({button});
      },
      async up({x, y, button = 'left'}) {
        await page.mouse.move(x, y);
        await page.mouse.up({button});
      },
      async wheel({x, y, dx = 0, dy}) {
        await page.mouse.move(x, y);
        await page.mouse.wheel(dx, dy);
      },
    },
    key: {
      async down({key}) {
        // Playwright's `down` of a printable key commits it too, as the
        // DevTools protocol's `text` does, so a key and the text it commits
        // are one event on both paths.
        await page.keyboard.down(key);
      },
      async up({key}) { await page.keyboard.up(key); },
      async press({key, modifiers = []}) {
        const held = modifiers.map(name => `${name}+`).join('');
        await page.keyboard.press(`${held}${key}`);
      },
    },
    async screenshot() { return page.screenshot({type: 'png'}); },
  };
}

/** A Chromium tab over the DevTools protocol: `chrome.mjs`'s own tab, with
 * every input the protocol spells differently named once here. */
function chromiumTab(tab) {
  return {
    log: tab.log,
    close: () => tab.close(),
    navigate: url => tab.navigate(url),
    evaluate: expression => tab.evaluate(expression),
    waitFor: (expression, options) => tab.waitFor(expression, options ?? {}),
    async viewport({width, height, scale = 1, mobile = false}) {
      await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: scale, mobile});
      if (mobile) await tab.send('Emulation.setTouchEmulationEnabled', {enabled: true, maxTouchPoints: 5});
    },
    async colourScheme(scheme) {
      await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: scheme}]});
    },
    mouse: {
      async move(x, y) {
        await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y, button: 'none'});
      },
      async down({x, y, button = 'left'}) {
        // Where the pointer is, before it presses: a menu opens at the last
        // move the app saw.
        await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y, button: 'none'});
        await tab.send('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button, clickCount: 1});
      },
      async up({x, y, button = 'left'}) {
        await tab.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button, clickCount: 1});
      },
      async wheel({x, y, dx = 0, dy}) {
        await tab.send('Input.dispatchMouseEvent', {type: 'mouseWheel', x, y, deltaX: dx, deltaY: dy});
      },
    },
    key: {
      async down({key, text}) {
        await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key, text, unmodifiedText: text});
      },
      async up({key}) { await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key}); },
      async press({key, modifiers = []}) {
        const MODIFIERS = {Alt: 1, Control: 2, Meta: 4, Shift: 8};
        const bits = modifiers.reduce((all, held) => all | (MODIFIERS[held] ?? 0), 0);
        for (const type of ['keyDown', 'keyUp']) {
          await tab.send('Input.dispatchKeyEvent', {type, key, modifiers: bits, windowsVirtualKeyCode: key === 'Escape' ? 27 : undefined});
        }
      },
    },
    screenshot: () => tab.screenshot(),
    // What only the DevTools protocol can do, for the tools that ask.
    send: (method, params) => tab.send(method, params),
    cdp: tab,
  };
}

/**
 * A browser and one tab on it, as one of `chromium`, `firefox`, `webkit`.
 *
 * WebGPU is Chromium's here: the canvas renders in neither Firefox nor
 * WebKit's headless build, so those engines show the HTML renderer and are
 * compared with each other and with Chromium's HTML, not with a canvas.
 */
export async function open({engine = 'chromium', width = 1280, height = 800, scale = 1, mobile = false, args = []} = {}) {
  if (!engines.has(engine)) throw new Error(`no engine ${engine}: one of ${[...engines].join(', ')}`);
  if (engine === 'chromium') {
    const browser = await launchChromium({args: [`--window-size=${width},${height}`, ...args]});
    const tab = chromiumTab(await browser.tab());
    await tab.viewport({width, height, scale, mobile});
    // `protocol` is the DevTools protocol itself, for what only it can do.
    return {name: 'chromium', tab, cdp: true, protocol: browser,
      close: async () => { await tab.close().catch(() => {}); await browser.close().catch(() => {}); }};
  }
  const {firefox, webkit} = await playwright();
  const type = engine === 'firefox' ? firefox : webkit;
  const browser = await type.launch();
  const context = await browser.newContext({
    viewport: {width, height},
    deviceScaleFactor: scale,
    isMobile: mobile,
    hasTouch: mobile,
    colorScheme: 'light',
  });
  const page = await context.newPage();
  return {name: engine, tab: playwrightTab(page, context, browser), cdp: false,
    close: async () => { await context.close().catch(() => {}); await browser.close().catch(() => {}); }};
}

export {sleep};
