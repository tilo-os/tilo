// The person's hands and eyes on the web desktop, shared by the page drivers:
// keys and clicks as a person gives them, the nodes the desktop publishes for a
// screen reader (what a driver reads and presses by name, since what is drawn
// is a picture), the shell's search, invitation links, what a page said while
// it loaded, and the mails a stack's entry sends.
import {execFile} from 'node:child_process';
import {mkdirSync, readdirSync, readFileSync} from 'node:fs';
import {dirname, isAbsolute, join} from 'node:path';
import {sleep} from '../../../../tools/web/chrome.mjs';
import {SHELL, stepLimit} from './waits.mjs';

/** Each simulated computer keeps its own identity and private daemon sockets. The
 * computers share one network namespace, so the kernel chooses each UDP listener. */
export const computerEnv = home => {
  const runtime = join(home, 'run');
  mkdirSync(runtime, {recursive: true, mode: 0o700});
  return {
    PATH: `${join(home, 'bin')}:${process.env.PATH}`,
    HOME: home,
    XDG_STATE_HOME: join(home, 'state'),
    XDG_CONFIG_HOME: join(home, 'config'),
    XDG_RUNTIME_DIR: runtime,
    TILO_DEVICE_PORT: '0',
  };
};

// The command printed for the person to give their agent. A ticket ends before
// the comma introducing the next instruction; older hosts offer a short code.
export function agentCommand(prompt) {
  const match = prompt?.match(/\b(?:tilo|sh -s) (login|device add) (tilo1[0-9a-f]+|[2-9A-HJKMNP-TV-Z]{3}-[2-9A-HJKMNP-TV-Z]{4})(?=[,\s]|$)/);
  if (!match) return null;
  const [, kind, text] = match;
  const ticket = text.startsWith('tilo1') ? text : null;
  if (kind === 'device add' && !ticket) return null;
  return {kind, text, ticket, code: ticket ? null : text, site: prompt.match(/--site ([^,\s]+)/)?.[1] ?? null};
}

/** The CLI the copied setup command installed, which the test agent uses for later calls. */
export const hostedTiloPath = (home, version) => join(home, '.local/share/tilo/cli', version, 'tilo');

/** Run the copied installer against the disposable stack, in the agent's own test home. */
export async function runSetupPrompt(prompt, {site, version, env, agent}) {
  const url = new URL(site);
  const local = url.hostname === 'localhost' || url.hostname.endsWith('.localhost')
    || /^127\.\d+\.\d+\.\d+$/.test(url.hostname) || url.hostname === '[::1]';
  if (!local || !['http:', 'https:'].includes(url.protocol) || url.origin !== site) {
    throw new Error('The installer needs the local stack origin.');
  }
  if (!/^v\d+\.\d+\.\d+-[0-9A-Za-z.]+$/.test(version) || !env?.HOME || !isAbsolute(env.HOME)) {
    throw new Error('The installer needs a pinned prerelease and a test home.');
  }
  const setup = agentCommand(prompt);
  if (!setup?.ticket) throw new Error('The setup prompt holds no device ticket.');
  const login = setup.kind === 'login';
  const instruction = login ? 'Set up Tilo for me' : 'Sign me in to Tilo';
  const installer = `${site}/install/hosted-v1.sh`;
  const source = url.hostname === '[::1]' ? `'${installer}'` : installer;
  const selection = `TILO_VERSION=${version} TILO_HOSTED_SETUP=1 TILO_INSTALL_DIR="$HOME/.local/share/tilo/cli/${version}"`;
  const command = `curl -fsSL ${source} | ${selection} sh -s ${setup.kind} ${setup.ticket}`;
  if (prompt !== `${instruction}: run ${command}, then do what it tells you.`) {
    throw new Error('The copied command does not select the expected local installer and CLI release.');
  }
  const args = login ? ['login', setup.ticket] : ['device', 'add', setup.ticket];
  if (agent !== undefined) {
    if (!login || typeof agent !== 'string' || !agent.trim()) throw new Error('An agent name is only supplied at login.');
    args.push('--agent', agent);
  }
  // Only the fixed pipeline is shell code. The site, command and ticket are arguments.
  // Fail on a refused download even if sh receives an empty script and exits successfully.
  const pipeline = 'curl -fsSL --max-redirs 0 "$1" | sh -s "${@:2}"';
  return new Promise(resolve => {
    execFile('bash', ['-o', 'pipefail', '-c', pipeline, 'setup', installer, ...args], {
      env: {...env, TILO_VERSION: version, TILO_HOSTED_SETUP: '1', TILO_INSTALL_DIR: dirname(hostedTiloPath(env.HOME, version))},
      timeout: 180_000, maxBuffer: 1 << 20,
    }, (error, stdout, stderr) => resolve({code: error ? (error.code || 1) : 0, stdout, stderr}));
  });
}

// Keys as the desktop's search reads them (its own scripts drive it this
// way): the chord, the query, Enter.
//
// The events of one gesture go out together, not one acknowledged frame at a
// time. Chromium answers a dispatched key or press when the page has taken it,
// and this page's main thread is inside a frame for seconds under the check: a
// driver that waited for each key in turn typed a five-letter query in eight
// frames, where a person types it in a tenth of a second and the shell reads
// the whole query on the frame it draws next.
export const event = (tab, params) => tab.send('Input.dispatchKeyEvent', params);
export const key = async (tab, name, modifiers = 0) => {
  const named = {Enter: 13, Escape: 27, ArrowDown: 40, ArrowUp: 38};
  const code = name.length === 1 ? `Key${name.toUpperCase()}` : name;
  const pressed = {key: name, code, windowsVirtualKeyCode: named[name] ?? name.toUpperCase().charCodeAt(0), modifiers};
  await Promise.all([
    event(tab, {type: 'rawKeyDown', ...pressed, ...(name === 'Enter' ? {text: '\r'} : {})}),
    event(tab, {type: 'keyUp', ...pressed}),
  ]);
};
export const type = async (tab, text) => {
  await Promise.all([...text].flatMap(ch => [
    event(tab, {type: 'keyDown', key: ch, text: ch}),
    event(tab, {type: 'keyUp', key: ch}),
  ]));
  // The shell reads the query on its next frame; the wait that follows is on
  // its own answer to it.
  await sleep(600);
};
export const took = from => ((Date.now() - from) / 1000).toFixed(1);
export const click = async (tab, x, y) => {
  // One gesture's three events, sent together: a press acknowledged one frame
  // at a time is three of this page's frames for a click a person makes in
  // milliseconds.
  await Promise.all([
    tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y}),
    ...['mousePressed', 'mouseReleased'].map(type =>
      tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1})),
  ]);
  // The press is the page's to take on its next frame; what it opened is read
  // after this, and the waits themselves end on the page's changes.
  await sleep(600);
};
// The centre of the first semantic node named `name`, in page pixels.
// The `nth` control drawn under `name` (a negative index counts from the
// last): a shell's floating card and the app's own panel can offer the same
// control, and a person's press is the one they can reach.
// [`find`] as the page-side expression a wait reads: the node's JSON, or ''.
export const found = (name, nth = 0) => `(() => {
  const found = [];
  // A control is the element that carries the name a person reaches it by, and
  // its box is what the browser laid out.
  const page = document.getElementById('tilo-page');
  for (const el of page?.querySelectorAll('[aria-label], button, a, input, textarea, [contenteditable]') ?? []) {
    const box = el.getBoundingClientRect();
    if (!box.width || !box.height) continue;
    found.push({name: el.getAttribute('aria-label') ?? (el.innerText || '').trim().split('\n')[0],
      bounds: {x0: box.x, y0: box.y, x1: box.right, y1: box.bottom}});
  }
  const matches = found.filter(n => n.name === ${JSON.stringify(name)} && n.bounds);
  const nth = ${nth};
  const match = matches[nth < 0 ? matches.length + nth : nth];
  // '' while it is not there, so a wait on this reads it as not yet.
  return match ? JSON.stringify(match) : '';
})()`;
export const find = async (tab, name, nth = 0) => (await tab.evaluate(found(name, nth))) || 'null';
export const until = async (tab, expression, what, timeout = stepLimit(3)) => {
  const value = await tab.waitFor(expression, {timeout}).catch(() => false);
  if (value) return value;
  const shown = await tab.evaluate(`JSON.stringify({
    arrival: document.documentElement.dataset.arrival ?? null,
    refusal: document.documentElement.dataset.refusal ?? null,
    entry: document.documentElement.dataset.entry ?? null,
    status: document.getElementById('status')?.textContent?.trim().slice(0, 200) ?? null,
    at: location.pathname,
  })`).catch(() => 'unreadable');
  throw new Error(`${what} did not happen within ${(timeout / 1000).toFixed(1)} s (the page: ${shown})`);
};
export const returning = tab => tab.send('Page.addScriptToEvaluateOnNewDocument',
  {source: "try { localStorage.setItem('tilo-setup', JSON.stringify({step: 'done'})); } catch {}"});
// What a page's loading said beside its console's errors: requests that
// failed and the browser's own log (a resource that did not load is said
// there, not in the console), and the page's console in full, so a page that
// stops while it loads leaves why.
export const recordLoading = async (launched, tab) => {
  const asked = new Map();
  launched.on(({sessionId, method, params}) => {
    if (sessionId !== tab.sessionId) return;
    if (method === 'Network.requestWillBeSent') asked.set(params.requestId, params.request.url);
    else if (method === 'Network.loadingFailed') {
      tab.log.push(`NET ${asked.get(params.requestId) ?? params.requestId}: ${params.errorText}${params.canceled ? ' (canceled)' : ''}`);
    } else if (method === 'Network.responseReceived' && params.response.status >= 400) {
      tab.log.push(`HTTP ${params.response.status} ${params.response.url}`);
    } else if (method === 'Log.entryAdded') {
      tab.log.push(`LOG ${params.entry.level} ${params.entry.text}${params.entry.url ? ` (${params.entry.url})` : ''}`.slice(0, 600));
    } else if (method === 'Runtime.consoleAPICalled' && !['error', 'warning'].includes(params.type)) {
      tab.log.push(`${params.type.toUpperCase()} ${params.args.map(a => a.value ?? a.description ?? '').join(' ')}`.slice(0, 600));
    }
  });
  await tab.send('Network.enable', {}).catch(() => {});
  await tab.send('Log.enable', {}).catch(() => {});
};
export const pageText = tab => tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim()`).catch(() => '');
/** A person's search, as the page answers it: the workspace's own search, at
 * the address that names it, typed into, and its row named `row` pressed.
 *
 * `command` was the desktop's search chord, which the page does not need, and
 * `opens` the desktop's record of what the press opened; both are kept in the
 * signature so a caller reads as it did. */
export const searcher = ({until, failed, dump}) => {
  const search = async (tab, _command, query, row) => {
    const workspace = await until(tab,
      `location.pathname.startsWith('/w/') ? location.pathname.split('/')[2] : ''`,
      'the page naming its workspace in its address', stepLimit(4));
    await tab.evaluate(`location.assign('/w/${workspace}/search')`);
    await until(tab, `!!document.querySelector('#tilo-page .tilo-search-field')`, 'the search page opening', stepLimit(6));
    const field = `document.querySelector('#tilo-page .tilo-search-field')`;
    await until(tab, `document.activeElement === ${field}`, 'the search field taking the caret', stepLimit(3));
    await type(tab, query);
    const wanted = `[...document.querySelectorAll('#tilo-page .tilo-search-row')]
      .find(candidate => (candidate.innerText || '').includes(${JSON.stringify(row)}) ?? false)`;
    const rows = JSON.parse(await until(tab, `(() => {
      const rows = [...document.querySelectorAll('#tilo-page .tilo-search-row')]
        .map(candidate => (candidate.innerText || '').replace(/\\s+/g, ' ').trim());
      return rows.some(text => text.includes(${JSON.stringify(row)})) ? JSON.stringify(rows) : '';
    })()`, `the search offering "${row}"`, stepLimit(4)).catch(async error => {
      await dump(tab, `search-${query}`);
      failed(`the page's search offers no "${row}" for "${query}": ${error?.message ?? error}`);
    }));
    const box = JSON.parse(await until(tab, `(() => {
      const found = ${wanted};
      if (!found) return '';
      found.scrollIntoView({block: 'nearest'});
      const rect = found.getBoundingClientRect();
      return rect.width && rect.height
        ? JSON.stringify({x: rect.x + Math.min(rect.width / 2, 40), y: rect.y + rect.height / 2}) : '';
    })()`, `the row "${row}" drawn`, stepLimit(3)));
    await click(tab, box.x, box.y);
    return rows;
  };
  return {search};
};

/** The mails the entry sent, oldest first: what the stack's mail sink kept of
 * each (`stack.sh mail` lists the same, with their links). */
export const mailsOf = dir => readdirSync(dir).filter(name => name.endsWith('.json')).sort()
  .map(name => JSON.parse(readFileSync(join(dir, name), 'utf8')));
export const waitForMail = async (dir, what, count, timeout = stepLimit(6)) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const kept = mailsOf(dir);
    if (kept.length >= count) return kept[count - 1];
    if (Date.now() > deadline) {
      throw new Error(`${what} never reached the mail sink (${kept.length} mails: ${JSON.stringify(kept.map(mail => mail.subject))})`);
    }
    await sleep(250);
  }
};
/** The link a mail carries whose path ends in `marker`'s variable, e.g. the
 * confirmation's `wait-list-confirm=<secret>` or an invitation's `invite=<key>`. */
export const mailLink = (mail, marker) => {
  const text = `${mail.text ?? ''}\n${mail.html ?? ''}`;
  return text.match(new RegExp(`https?://[^\\s"'<>]*${marker}[=:][0-9a-zA-Z_-]+`))?.[0] ?? null;
};
/** A person leaves `address` on the entry's wait list, as the page's own form
 * takes it. The driver waits on what the page says next. */
export const leaveWaitListAddress = (tab, address) => tab.evaluate(`(() => {
  const field = document.getElementById('arrival-wait-email');
  if (!field) return false;
  field.value = ${JSON.stringify(address)};
  field.dispatchEvent(new Event('input', {bubbles: true}));
  const button = document.querySelector('#arrival-wait button');
  if (button) button.click();
  return !!button;
})()`);
/** A person presses the button of the page's own form whose text is `text`, in
 * the card `within` (a page's card, not the shell's canvas). */
export const pressText = (tab, text, within = 'body') => tab.evaluate(`(() => {
  const button = [...document.querySelectorAll(${JSON.stringify(within)} + ' button')]
    .find(candidate => candidate.textContent.trim() === ${JSON.stringify(text)} && !candidate.disabled);
  if (!button) return false;
  button.click();
  return true;
})()`);
/** Wait until the page answers `expression` something other than an empty
 * string, or say '' when it never did: for reading what a card says next, or
 * what went wrong, without the wait failing the run. */
export const poll = (tab, expression, timeout = stepLimit(4)) =>
  tab.waitFor(expression, {timeout}).catch(() => '');
/** The Lounge's waiting card, as a person reads it: the addresses waiting to be
 * let in and what the card offers, once it has answered with rows, with
 * nobody on it, or with what went wrong; null while it has said none of them. */
export const waitingCard = async (tab, timeout = stepLimit(6)) => {
  const shown = await poll(tab, `(() => {
    const card = document.querySelector('.tilo-waiting');
    if (!card) return '';
    const error = card.querySelector('.tilo-entry-error');
    const rows = [...card.querySelectorAll('.tilo-waiting-contact')].map(node => node.textContent.trim());
    const empty = card.querySelector('.tilo-waiting-none');
    if (!rows.length && !empty && !(error && !error.hidden)) return '';
    return JSON.stringify({contacts: rows,
      said: error && !error.hidden ? error.textContent.trim() : null,
      buttons: [...card.querySelectorAll('li button')].map(node => node.textContent.trim())});
  })()`, timeout);
  return shown ? JSON.parse(shown) : null;
};
