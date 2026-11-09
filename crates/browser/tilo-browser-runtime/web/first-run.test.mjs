import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import {agentPrompt, CONNECTED_MS, createFirstRun, INSTALL, SEEN, setupFailure, timeLeft} from './first-run.mjs';

const here = name => new URL(name, import.meta.url);
const LINK = `https://tilo.example/#/@rooms.example/w/0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0?pair=${'ab'.repeat(32)}${'0a'.repeat(16)}`;

/** A view that records what it was told, in order. */
function fakeView() {
  const said = [];
  const view = new Proxy({}, {get: (_, name) => (...args) => { said.push([name, ...args]); }});
  return {view, said, names: () => said.map(([name]) => name)};
}

function fixture({copy = async text => { fixture.copied.push(await text); }} = {}) {
  const {view, said, names} = fakeView();
  const stored = new Map();
  const state = {clock: 0, ticks: null, laters: []};
  const run = createFirstRun({view, clipboard: {copy: text => copy(text)},
    storage: {setItem: (key, value) => stored.set(key, value)},
    now: () => state.clock, every: tick => { state.ticks = tick; return () => { state.ticks = null; }; },
    later: (run, ms) => state.laters.push([run, ms])});
  return {run, said, names, stored, state};
}
fixture.copied = [];
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

test('the prompt is complete on its own: install, sign in with the link, add the MCP server as tilo, check', () => {
  const prompt = agentPrompt(LINK);
  assert.ok(prompt.includes(INSTALL));
  assert.ok(prompt.includes(`tilo login '${LINK}' --agent`));
  assert.ok(prompt.includes('tilo agent connect claude'));
  assert.ok(prompt.includes('tilo agent connect codex'));
  assert.ok(prompt.includes('`tilo mcp`'));
  assert.ok(prompt.includes('tilo_status'));
  assert.ok(prompt.includes('works once, for 10 minutes'));
  assert.ok(prompt.length < 1000, `short enough to read before pasting (${prompt.length})`);
  assert.equal(INSTALL, 'curl -fsSL https://tilo.sh | sh');
});

test('time left reads minutes and seconds and stops at zero', () => {
  assert.equal(timeLeft(600_000, 0), '10:00');
  assert.equal(timeLeft(600_000, 18_500), '9:42');
  assert.equal(timeLeft(600_000, 700_000), '0:00');
});

test('a click before the page can make links copies once it can, then counts down', async () => {
  fixture.copied = [];
  const {run, names, said, state} = fixture();
  const done = run.copy();
  assert.deepEqual(names(), ['waiting'], 'the wait shows at once');
  await settle();
  assert.deepEqual(fixture.copied, [], 'nothing is copied before there is a link');
  run.offerSetup(async () => ({link: LINK, expiresAt: 600_000}));
  await done;
  assert.deepEqual(fixture.copied, [agentPrompt(LINK)]);
  assert.deepEqual(said.find(([name]) => name === 'prompt'), ['prompt', agentPrompt(LINK), {copied: true}]);
  assert.deepEqual(said.at(-1), ['left', '10:00']);
  state.clock = 18_500;
  state.ticks();
  assert.deepEqual(said.at(-1), ['left', '9:42']);
  state.clock = 600_000;
  state.ticks();
  assert.deepEqual(said.at(-1), ['expired']);
  assert.equal(state.ticks, null, 'an expired link is no longer counted down');
});

test('a clipboard that refuses still shows the prompt, to copy by hand', async () => {
  const {run, said} = fixture({copy: async () => { throw new Error('denied'); }});
  run.offerSetup(async () => ({link: LINK, expiresAt: 600_000}));
  await run.copy();
  assert.deepEqual(said.find(([name]) => name === 'prompt'), ['prompt', agentPrompt(LINK), {copied: false}]);
  assert.deepEqual(said.find(([name]) => name === 'problem'), ['problem', setupFailure({code: 'copy-failed'})]);
});

test('a page that cannot make links says so instead of waiting', async () => {
  const {run, said} = fixture({copy: async text => { await text; }});
  const done = run.copy();
  run.unavailable({code: 'refused'});
  await done;
  assert.deepEqual(said.at(-1), ['failed', setupFailure({code: 'refused'})]);
  // A later click fails at once too.
  await run.copy();
  assert.deepEqual(said.at(-1), ['failed', setupFailure({code: 'refused'})]);
});

test('the agent signing in shows it connected, then leaves the page to the workspace, remembered', async () => {
  const {run, names, stored, state} = fixture();
  run.offerSetup(async () => ({link: LINK, expiresAt: 600_000}));
  await run.copy();
  run.connected('Claude Code on laptop');
  assert.equal(names().at(-1), 'connected');
  assert.equal(state.ticks, null);
  const [[leave, ms]] = state.laters;
  assert.equal(ms, CONNECTED_MS);
  leave();
  assert.equal(names().at(-1), 'close');
  assert.equal(stored.get(SEEN), '1', 'the first screen is not shown again in this browser');
  assert.ok(run.closed);
});

test('skip for now leaves at once, remembered; passkey sign-in waits until the page offers it', async () => {
  const {run, names, stored} = fixture();
  await run.passkey();
  assert.deepEqual(names(), [], 'a passkey before the page can ask does nothing');
  let asked = 0;
  run.offerSignIn(async () => { asked += 1; });
  await run.passkey();
  assert.equal(asked, 1);
  assert.deepEqual(names(), ['signInReady', 'signingIn', 'signingIn']);
  run.skip();
  assert.equal(names().at(-1), 'close');
  assert.equal(stored.get(SEEN), '1');
});

/** first-run.inline.js against a page at `href`, with `stored` keys and the
 * scrubber's `secrets`: whether the first screen shows. */
function shows(href, {stored = {}, secrets = {}, storage = true} = {}) {
  const url = new URL(href);
  const attributes = new Map();
  const page = {
    document: {documentElement: {setAttribute: (name, value) => attributes.set(name, value)}},
    location: {hash: url.hash, search: url.search, pathname: url.pathname},
    localStorage: storage ? {getItem: key => stored[key] ?? null} : undefined,
  };
  page.globalThis = {...page, __tiloLinkSecrets: secrets};
  if (!storage) Object.defineProperty(page, 'localStorage', {get() { throw new Error('denied'); }});
  vm.runInNewContext(readFileSync(here('first-run.inline.js'), 'utf8'), page);
  return attributes.has('data-first-run');
}

test('only a browser new here, opening the site itself, sees the first screen', () => {
  assert.equal(shows('https://tilo.example/'), true);
  assert.equal(shows('https://tilo.example/#/'), true);
  assert.equal(shows('https://tilo.example/?debug'), true);
  assert.equal(shows('https://tilo.example/', {stored: {[SEEN]: '1'}}), false, 'seen already');
  assert.equal(shows('https://tilo.example/', {stored: {'tilo-host-memberships': '{}'}}), false, 'a host let it in before');
  assert.equal(shows('https://tilo.example/', {stored: {'tilo-entered-workspaces': '[]'}}), false);
  assert.equal(shows('https://tilo.example/', {secrets: {invite: 'x'}}), false, 'an invitation opens as it did');
  assert.equal(shows('https://tilo.example/#/@rooms.example/w/abc'), false, 'a link to a workspace');
  assert.equal(shows('https://tilo.example/?embed=1'), false, 'inside another site');
  assert.equal(shows('https://tilo.example/?workspace=Lounge'), false);
  assert.equal(shows('https://tilo.example/', {storage: false}), false, 'no storage: as it always opened');
});

test('the desktop entry page inlines the first screen’s script unchanged, after the scrubber, and carries its screen', () => {
  const html = readFileSync(here('index.html'), 'utf8');
  const script = readFileSync(here('first-run.inline.js'), 'utf8');
  const at = html.indexOf(`<script>\n${script}</script>`);
  assert.ok(at > 0, 'index.html must inline first-run.inline.js unchanged');
  assert.ok(html.indexOf('__tiloLinkSecrets') < at, 'after the scrubber, whose secrets it reads');
  for (const id of ['first-run', 'first-run-agent', 'first-run-skip', 'first-run-passkey', 'first-run-prompt', 'first-run-status']) {
    assert.ok(html.includes(`id="${id}"`), `index.html has #${id}`);
  }
  assert.ok(html.includes('<script type="module" async src="./first-run.mjs"></script>'));
  assert.ok(html.includes('prefers-color-scheme: light'), 'the screen follows the person’s light or dark');
});

test('skip for now opens what the page names, also when the person skipped before it could', () => {
  const later = fixture();
  let opened = 0;
  later.run.whenSkipped(() => { opened += 1; });
  later.run.skip();
  assert.equal(opened, 1, 'the person’s own workspace opens');
  const early = fixture();
  early.run.skip();
  early.run.whenSkipped(() => { opened += 1; });
  assert.equal(opened, 2, 'a skip before the page opened still opens it');
  const connected = fixture();
  connected.run.connected('Claude Code');
  connected.state.laters[0][0]();
  connected.run.whenSkipped(() => { opened += 1; });
  assert.equal(opened, 2, 'a screen the agent closed opened nothing else');
});
