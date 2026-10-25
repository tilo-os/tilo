// A page in Safari, through WebDriver: the page steps the CDP client in
// tools/web/chrome.mjs takes (navigate, evaluate, action, click and type,
// window size), for the qualification runs Safari can take and Chrome
// cannot.
//
// Safari runs only on macOS, under a logged-in user, and its driver is off
// until someone turns it on once:
//
//   safaridriver --enable      # asks for an administrator's password, once
//
// `launch()` starts `safaridriver --port 0` (SAFARIDRIVER names another
// binary) and reads the port from its `Server started on port N` line. It
// drives the classic protocol — `POST /session`, `POST /session/<id>/url`,
// `POST /session/<id>/execute/sync` and `/execute/async`, `POST
// /session/<id>/actions`, `GET|POST /session/<id>/window/rect` — which is all
// a page step needs: it types, clicks and reads its own numbers back from
// `evaluate`. A pointer step carries its own pause, so a case's timing is the
// page's, not the driver's round trip.
//
// Every setup failure (not macOS, no Safari, remote automation off, no
// logged-in session) is an `Unavailable`, never a page result: a run that
// cannot observe its subject is invalid, not a failure.
import {spawn} from 'node:child_process';

export const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

/** The host cannot run Safari at all, and a step that needs one cannot start (an invalid run). */
export class Unavailable extends Error {}

const BINARY = () => process.env.SAFARIDRIVER || '/usr/bin/safaridriver';

/**
 * What the driver's exit says to do about it: everything a person has to
 * arrange before Safari can be driven, in one place (the report quotes it).
 */
export function safaridriverUnavailable(binary, code, said) {
  return `safaridriver (${binary}) exited (${code}): ${said.slice(-400) || 'no output'}. It needs macOS with a logged-in `
    + `desktop session, and remote automation turned on once: \`${binary} --enable\` (an administrator's password)`;
}

/**
 * The keys a harness types, as WebDriver's key values (W3C WebDriver, §17.4;
 * a character is its own value) and the text a press inserts (`text`), which
 * is what makes a key typed rather than a shortcut.
 */
export const KEYS = Object.freeze({
  Control: {value: '\uE009'},
  Shift: {value: '\uE008'},
  Enter: {value: '\uE007'},
  v: {value: 'v', text: 'v'},
});

/** WebDriver's pointer buttons. */
export const BUTTONS = Object.freeze({left: 0, middle: 1, right: 2});

/**
 * The W3C actions `steps` describe: `{pointer: {type: 'pointerMove'|'pointerDown'|'pointerUp', x, y, button, pause}}`
 * and `{key: {value, up, pause}}`, in one list, in order. A pointer step's
 * `pause` is a pause after it (`pointerMove`, `pointerDown`, `pointerUp`);
 * a `pause: N` of its own is a pause alone.
 */
export function wxActions(steps) {
  const sources = [];
  let pointer, keyboard;
  const pointerSource = () => (pointer ??= {type: 'pointer', id: 'mouse', parameters: {pointerType: 'mouse'}, actions: []});
  const keySource = () => (keyboard ??= {type: 'key', id: 'keyboard', actions: []});
  for (const step of steps) {
    if (step.pointer) {
      const {pause = 0, ...move} = step.pointer;
      pointerSource().actions.push(move);
      if (pause) pointerSource().actions.push({type: 'pause', duration: pause});
    } else if (step.key) {
      const {value, up = true, pause = 0, down = true} = step.key;
      if (down) keySource().actions.push({type: 'keyDown', value});
      if (up) keySource().actions.push({type: 'keyUp', value});
      if (pause) keySource().actions.push({type: 'pause', duration: pause});
    } else if (step.pause !== undefined) {
      pointerSource().actions.push({type: 'pause', duration: step.pause});
    } else {
      throw Error(`an action is a pointer step, a key step or a pause: ${JSON.stringify(step)}`);
    }
  }
  // The pointer's source leads: its pauses are the run's clock.
  for (const source of [pointer, keyboard]) if (source) sources.push(source);
  return sources;
}

/**
 * A Safari window on `platform` (a macOS host by default), driven through
 * `safaridriver`. Resolves to `{product, version, evaluate, navigate, actions,
 * click, type, setWindowRect, close}`, or throws `Unavailable`.
 */
export async function launch({platform = process.platform, startTimeout = 120_000, args = []} = {}) {
  if (platform !== 'darwin') {
    throw new Unavailable(`Safari is macOS's browser alone; this host is ${platform}, whose WebKit builds (WebKitGTK, WPE) `
      + 'are not Safari and have no WebTransport');
  }
  const child = spawn(BINARY(), ['--port', '0', ...args], {stdio: ['ignore', 'ignore', 'pipe']});
  let said = '';
  let failed;
  const port = await new Promise((resolve, reject) => {
    failed = reject;
    const timer = setTimeout(() => {
      child.kill('SIGTERM');
      reject(new Unavailable(safaridriverUnavailable(BINARY(), 'no port', said)));
    }, startTimeout);
    child.stderr.on('data', chunk => {
      said = (said + chunk).slice(-8000);
      const match = /Server started on port (\d+)/.exec(said);
      if (match) { clearTimeout(timer); resolve(Number(match[1])); }
    });
    child.on('exit', code => { clearTimeout(timer); reject(new Unavailable(safaridriverUnavailable(BINARY(), code, said))); });
    child.on('error', error => {
      clearTimeout(timer);
      reject(new Unavailable(`safaridriver did not start: ${error}. Safari ships with macOS; SAFARIDRIVER names another binary`));
    });
  });
  // A driver that dies after it answered (no logged-in window, lost
  // automation) must stop the run's next call, not leave it waiting.
  child.on('exit', code => failed?.(new Unavailable(safaridriverUnavailable(BINARY(), code, said))));
  const base = `http://127.0.0.1:${port}`;
  const call = async (method, path, body) => {
    const reply = await fetch(`${base}${path}`, {
      method, headers: body === undefined ? {} : {'content-type': 'application/json'},
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await reply.text();
    const parsed = text ? JSON.parse(text) : {};
    if (!reply.ok || parsed.value?.error) {
      const error = parsed.value ?? {error: String(reply.status), message: text.slice(0, 400)};
      throw Error(`${method} ${path}: ${error.error}: ${error.message}${error.data ? ` (${error.data})` : ''}`);
    }
    return parsed.value ?? null;
  };
  let session;
  try {
    session = (await call('POST', '/session', {capabilities: {alwaysMatch: {browserName: 'safari'}}})).sessionId;
  } catch (error) {
    child.kill('SIGTERM');
    throw new Unavailable(`Safari opened no session: ${error.message}. Remote automation is off until \`${BINARY()} --enable\` `
      + '(once, as an administrator), and Safari needs a logged-in desktop session');
  }
  const at = path => `/session/${session}${path}`;
  let closed = false;
  const capabilities = await call('GET', at('/capabilities')).catch(() => ({}));
  const finish = async () => {
    closed = true;
    const exited = child.exitCode !== null ? Promise.resolve() : new Promise(resolve => child.once('exit', resolve));
    await call('DELETE', at('')).catch(() => {});
    child.kill('SIGTERM');
    await exited;
  };
  return {
    product: `Safari ${capabilities.browserVersion ?? 'unknown'}`,
    version: capabilities,
    session,
    /** `expression`'s value; it may be a promise, which the driver waits for. */
    evaluate: expression => call('POST', at('/execute/async'), {script: `return (async () => { ${expression}\n })();`, args: []}),
    navigate: url => call('POST', at('/url'), {url}),
    /** Key and pointer steps (`wxActions`) within `ms` (an Invalid-style timeout, never a hang). */
    actions: async (steps, {timeout = 120_000} = {}) => {
      let timer;
      const deadline = new Promise((_, reject) => {
        timer = setTimeout(() => reject(Error(`a page's input did not finish within ${Math.round(timeout / 1000)} s`)), timeout);
      });
      return Promise.race([call('POST', at('/actions'), {actions: wxActions(steps)}), deadline]).finally(() => clearTimeout(timer));
    },
    /** A click at page coordinates: where the pointer goes, presses and lifts, as a person's does. */
    click: async (x, y, {button = BUTTONS.left, pause = 150} = {}) => call('POST', at('/actions'), {actions: wxActions([
      {pointer: {type: 'pointerMove', x, y, origin: 'viewport', duration: 0, pause}},
      {pointer: {type: 'pointerDown', button, pause: 0}},
      {pointer: {type: 'pointerUp', button, pause: 100}},
    ])}),
    /**
     * Text typed one character at a time, each as a key press. WebDriver has
     * no input method and no clipboard, so a case that needs either sets it
     * in the page (and the run's report says so).
     */
    type: async (text, {pause = 30} = {}) => {
      for (const character of text) {
        await call('POST', at('/actions'), {actions: wxActions([{key: {value: character, pause}}])});
      }
    },
    /** The window's size in CSS pixels, set when asked: Safari's viewport is its window's content box. */
    windowRect: () => call('GET', at('/window/rect')),
    setWindowRect: async ({width, height, x = 0, y = 0}) => {
      await call('POST', at('/window/rect'), {width, height, x, y});
      return call('GET', at('/window/rect'));
    },
    close: async () => { if (!closed) await finish(); },
  };
}
