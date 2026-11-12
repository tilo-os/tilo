import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {parseMessages} from './arrival.mjs';
import {domFailureView, failureCause, failureDetails, failureScreen, localHost, namedOrigin, retryHelps, SCREENS, unanswered} from './failure-screen.mjs';

const here = name => new URL(name, import.meta.url);
const locale = name => parseMessages(readFileSync(here(`locales/${name}.ftl`), 'utf8'));
const named = (name, code, message = code) => Object.assign(new Error(message), {name, code});

test('each failure is told apart by what the page knows of it', () => {
  const cases = [
    [named('HostAdmissionError', 'removed'), {}, 'removed'],
    [named('AdmissionError', 'removed'), {}, 'removed'],
    [named('HostAdmissionError', 'cooling-down'), {}, 'cooling-down'],
    [named('HostAdmissionError', 'invitation-only'), {}, 'invitation-only'],
    [named('AdmissionError', 'no-access'), {}, 'no-access'],
    [named('AdmissionError', 'declined', 'You did not join'), {}, 'refused'],
    [named('CustodyError', 'custody-busy'), {}, 'custody-busy'],
    [new Error('anything'), {settled: 'gone'}, 'gone'],
    [new Error('anything'), {settled: 'deleted'}, 'deleted'],
    [new TypeError('Failed to fetch'), {online: false}, 'offline'],
    [new TypeError('Failed to fetch'), {}, 'unreachable'],
    [named('HostAdmissionError', 'unavailable', 'join: Failed to fetch'), {}, 'unreachable'],
    [new TypeError('Failed to fetch'), {siteAnswers: false}, 'no-connection'],
    // A file of this site's own that did not come, while the site answers.
    [new TypeError('Failed to fetch dynamically imported module: https://tilo.sh/v/7/app.js'), {hostIsSite: true, admitted: true}, 'desktop'],
    [new TypeError('Failed to fetch dynamically imported module: https://tilo.sh/v/7/app.js'), {hostIsSite: true}, 'loading'],
    [new Error('WebSocket connection closed'), {admitted: true}, 'unreachable'],
    [Object.assign(new Error('join: HTTP 502'), {status: 502}), {}, 'host-error'],
    [new Error('admit: HTTP 500'), {}, 'host-error'],
    [new Error('the peer has no replicated room'), {admitted: true}, 'desktop'],
    [new Error('device lost'), {admitted: true, drawn: true}, 'stopped'],
    [new Error('RuntimeError: unreachable executed'), {}, 'loading'],
  ];
  for (const [error, context, cause] of cases) assert.equal(failureCause(error, context), cause, `${error.message} ${JSON.stringify(context)}`);
});

test('every screen says its cause and offers Try again only where trying again can help', () => {
  const never = ['removed', 'cooling-down', 'invitation-only', 'gone', 'deleted', 'no-access', 'refused'];
  for (const cause of Object.keys(SCREENS).filter(key => key !== 'unreachable-local')) {
    const screen = failureScreen(cause, {workspace: 'Lounge', host: 'https://eu1.tilo.sh', step: 'open',
      error: new Error('You did not join')});
    assert.equal(screen.retry, !never.includes(cause), cause);
    assert.equal(retryHelps(cause), screen.retry, cause);
    assert.ok(screen.heading && screen.body, cause);
    // Plain words: no raw error, no placeholder left, no bare "Try again".
    for (const said of [screen.heading, screen.body]) {
      assert.doesNotMatch(said, /\{|\$|HTTP|TypeError|undefined|null/, `${cause}: ${said}`);
    }
    assert.notEqual(screen.body.trim(), 'Try again.', cause);
  }
});

test('a host that never answers is named, and only a machine’s own address is suspected of its certificate', () => {
  const local = failureScreen('unreachable', {workspace: 'Lounge', host: 'https://127.0.0.1:24100'});
  assert.equal(local.heading, 'Can’t connect to 127.0.0.1:24100');
  assert.match(local.body, /This site loads, so your connection works/);
  assert.match(local.body, /doesn’t trust its certificate. Open 127\.0\.0\.1:24100 to see which/);
  assert.equal(local.openHost, 'https://127.0.0.1:24100');
  assert.equal(local.retry, true);
  const hosted = failureScreen('unreachable', {workspace: 'Lounge', host: 'https://eu1.tilo.sh'});
  assert.equal(hosted.heading, 'Can’t connect to eu1.tilo.sh');
  assert.doesNotMatch(hosted.body, /certificate/);
  assert.equal(hosted.openHost, null);
  assert.match(failureScreen('no-connection', {host: 'https://eu1.tilo.sh'}).body, /Check your connection/);
  for (const origin of ['https://127.0.0.1:24100', 'https://[::1]:443', 'https://tilo.localhost:24120', 'https://nas.local', 'https://localhost']) {
    assert.ok(localHost(origin), origin);
  }
  for (const origin of ['https://eu1.tilo.sh', 'https://tilo.sh', 'not a url']) assert.ok(!localHost(origin), origin);
});

test('a desktop that did not start says where it stopped and why trying again is safe', () => {
  const drawing = failureScreen('desktop', {workspace: 'Auth', step: 'draw'});
  assert.equal(drawing.heading, 'Auth didn’t start');
  assert.equal(drawing.body, 'It stopped while drawing its first screen. Your windows and documents are kept in this browser, '
    + 'so trying again picks up where you left off.');
  assert.match(failureScreen('desktop', {step: 'open'}).body, /^It stopped while setting up its desktop\./);
  assert.match(failureScreen('loading', {step: 'join'}).body, /^It stopped while joining\. Try again\. If it stops again, copy the technical details/);
  assert.equal(failureScreen('refused', {error: new Error('You did not join')}).body, 'You did not join');
});

test('the details name the cause, host, step and error, never a secret', () => {
  const text = failureDetails({cause: 'unreachable', host: 'https://127.0.0.1:24100/peer/v1/host?token=abc', step: 'join',
    error: new TypeError(`Failed to fetch https://h/?invite=${'a'.repeat(40)}`), build: '7'});
  assert.match(text, /^Cause: unreachable$/m);
  assert.match(text, /^Host: 127\.0\.0\.1:24100$/m);
  assert.match(text, /^Stopped at: join$/m);
  assert.match(text, /TypeError: Failed to fetch/);
  assert.doesNotMatch(text, /a{40}|token=abc/);
});

test('the host a failure names is the one in its words, where it names one', () => {
  assert.equal(namedOrigin(new TypeError('Failed to fetch dynamically imported module: https://tilo.sh/v/7/app.js')), 'https://tilo.sh');
  assert.equal(namedOrigin(new Error("admit: request to 'https://127.0.0.1:24100/peer/v1/host' failed")), 'https://127.0.0.1:24100');
  assert.equal(namedOrigin(new TypeError('Failed to fetch')), null);
});

test('a network failure is one the browser never answered, not anything that mentions a network', () => {
  for (const message of ['Failed to fetch', 'NetworkError when attempting to fetch resource.', 'Load failed', 'join: Failed to fetch', 'WebSocket connection closed']) {
    assert.ok(unanswered(new TypeError(message)), message);
  }
  assert.ok(!unanswered(new Error('the network interface named no address')));
});

test('both locales word every screen, with the workspace’s name and without', () => {
  const english = locale('en-US');
  for (const name of ['en-US', 'de-DE']) {
    const messages = locale(name);
    for (const key of Object.keys(SCREENS)) {
      for (const id of [`entry-failure-${key}`, `entry-failure-${key}-unnamed`, `entry-failure-${key}-body`]) {
        assert.ok(messages.get(id), `${name}: ${id}`);
      }
    }
    for (const id of ['entry-failure-step-load', 'entry-failure-step-join', 'entry-failure-step-open', 'entry-failure-step-draw',
      'entry-failure-open-host', 'entry-failure-others', 'entry-failure-the-host', 'entry-failure-refused-fallback']) {
      assert.ok(messages.get(id), `${name}: ${id}`);
    }
  }
  // The English file says what the module says without one.
  for (const cause of Object.keys(SCREENS)) {
    for (const options of [{workspace: 'Lounge', host: 'https://eu1.tilo.sh', step: 'draw'}, {host: 'https://127.0.0.1:1'}]) {
      assert.deepEqual(failureScreen(cause, {...options, messages: english}), failureScreen(cause, options), cause);
    }
  }
  assert.equal(failureScreen('desktop', {workspace: 'Kaffeeküche', step: 'draw', messages: locale('de-DE')}).heading,
    'Kaffeeküche ist nicht gestartet');
});

/** A document holding the failure screen's parts, by id. */
function screenDocument() {
  const made = {};
  const element = id => (made[id] ??= {id, hidden: false, textContent: '', className: '', children: [], listeners: {},
    addEventListener(type, listener) { this.listeners[type] = listener; }, focus() { made.focused = id; },
    replaceChildren(...nodes) { this.children = nodes; }, append(...nodes) { this.children.push(...nodes); }});
  const document = {documentElement: {dataset: {}}, getElementById: element,
    createElement: tag => ({tag, children: [], listeners: {}, append(...nodes) { this.children.push(...nodes); },
      addEventListener(type, listener) { this.listeners[type] = listener; }})};
  return {document, made};
}

test('the screen shows one action first and the other workspaces under it, in the same screen', () => {
  const {document, made} = screenDocument();
  let retried = 0;
  const opened = [];
  const view = domFailureView({document, onRetry: () => { retried += 1; }, open: url => opened.push(url)});
  const local = failureScreen('unreachable', {workspace: 'Lounge', host: 'https://127.0.0.1:24100'});
  view.show({screen: local, details: 'Cause: unreachable', others: [{label: 'Lounge', note: 'Didn’t open last time', open: () => opened.push('lounge')}]});
  assert.equal(document.documentElement.dataset.failure, 'unreachable');
  assert.equal(made['arrival-failure-heading'].textContent, 'Can’t connect to 127.0.0.1:24100');
  // Opening the host leads where only the browser can say what is wrong.
  assert.equal(made['arrival-failure-host'].className, 'failure-primary');
  assert.equal(made['arrival-failure-retry'].className, 'failure-secondary');
  assert.equal(made.focused, 'arrival-failure-host');
  made['arrival-failure-host'].listeners.click();
  made['arrival-failure-retry'].listeners.click();
  assert.deepEqual(opened, ['https://127.0.0.1:24100']);
  assert.equal(retried, 1);
  assert.equal(made['arrival-failure-elsewhere'].hidden, false);
  assert.equal(made['arrival-failure-others-heading'].textContent, 'Or open another of your workspaces');
  const button = made['arrival-failure-others'].children[0].children[0];
  assert.deepEqual(button.children.map(part => part.textContent), ['Lounge', 'Didn’t open last time']);
  button.listeners.click();
  assert.deepEqual(opened.at(-1), 'lounge');
  // The words reach screen readers through the status line.
  assert.match(made.status.textContent, /^Can’t connect to 127\.0\.0\.1:24100\. This site loads/);

  // A refusal offers nothing to try again, and no list when there is nowhere else.
  view.show({screen: failureScreen('removed', {workspace: 'Lounge'})});
  assert.equal(document.documentElement.dataset.failure, 'removed');
  assert.equal(made['arrival-failure-retry'].hidden, true);
  assert.equal(made['arrival-failure-host'].hidden, true);
  assert.equal(made['arrival-failure-elsewhere'].hidden, true);
  assert.equal(made['arrival-failure-details'].hidden, true);
  view.hide();
  assert.equal(document.documentElement.dataset.failure, undefined);
});

test('while the failure screen shows, nothing else of the page shows beside or over it', () => {
  const html = readFileSync(here('index.html'), 'utf8');
  const hidden = html.match(/\[data-failure\] :is\(([^)]*)\) \{ display: none; \}/)?.[1] ?? '';
  for (const part of ['#arrival', '#arrival-note', '#arrival-retry', '#arrival-own', '#arrival-reconnect', '.tilo-entry-card']) {
    assert.ok(hidden.split(',').map(item => item.trim()).includes(part), part);
  }
  assert.match(html, /\[data-failure\] #status \{ position: fixed; width: 1px; height: 1px; overflow: hidden; clip-path: inset\(50%\)/);
  for (const id of ['arrival-failure-heading', 'arrival-failure-body', 'arrival-failure-retry', 'arrival-failure-host',
    'arrival-failure-elsewhere', 'arrival-failure-others', 'arrival-failure-details', 'arrival-failure-text']) {
    assert.match(html, new RegExp(`id="${id}"`), id);
  }
  // On a phone the actions stack at full width rather than overlap.
  assert.match(html, /@media \(max-width: 480px\) \{[^}]*\[data-failure\] #arrival-failure[^@]*\.failure-actions \{ flex-direction: column/);
  // A stop before the page's modules ran grows its words up from Try again, never under it.
  assert.match(html, /\[data-arrival=stopped\] #status \{ top: auto; bottom: calc\(50% - 64px\); \}/);
});
