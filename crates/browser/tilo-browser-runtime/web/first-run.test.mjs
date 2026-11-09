import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import {CONNECTED_MS, createFirstRun, FOLLOW_MS, manualCommands, MCP_URL, SEEN, setupFailure, setupPrompt, signInPrompt,
  stepLines, timeLeft} from './first-run.mjs';

const here = name => new URL(name, import.meta.url);
const CODE = 'K7Q-MX2P';

/** A view that records what it was told, in order. */
function fakeView() {
  const said = [];
  const view = new Proxy({}, {get: (_, name) => (...args) => { said.push([name, ...args]); }});
  return {view, said, names: () => said.map(([name]) => name), last: name => said.findLast(([n]) => n === name)};
}

/** A code the host made, whose `follow()` answers what `polled` holds now. */
function fakeCode({expiresAt = 600_000} = {}) {
  const code = {code: CODE, expiresAt, polled: {resolved: false, expired: false, steps: []}, asked: 0, signedIn: []};
  code.follow = async () => { code.asked += 1; return structuredClone(code.polled); };
  code.signIn = async claimed => { code.signedIn.push(claimed); };
  return code;
}

function fixture({copy, site} = {}) {
  const {view, said, names, last} = fakeView();
  const stored = new Map();
  const copied = [];
  const state = {clock: 0, ticks: null, every: null, laters: []};
  const run = createFirstRun({view, site, clipboard: {copy: copy ?? (async text => { copied.push(await text); })},
    storage: {setItem: (key, value) => stored.set(key, value)},
    now: () => state.clock, every: (tick, ms) => { state.ticks = tick; state.every = ms; return () => { state.ticks = null; }; },
    later: (run, ms) => state.laters.push([run, ms])});
  // One follow round: the tick, and the host's answer.
  const tick = async () => { state.ticks?.(); await settle(); };
  return {run, said, names, last, stored, state, copied, tick};
}
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

test('the setup prompt is one line: install, log in with the code, follow what tilo says', () => {
  assert.equal(setupPrompt(CODE),
    'Set up Tilo for me: run curl -fsSL tilo.sh | sh && tilo login K7Q-MX2P, then do what it tells you.');
  assert.equal(setupPrompt(CODE, {site: 'https://tilo.sh'}), setupPrompt(CODE), 'tilo.sh is the default site');
  assert.equal(setupPrompt(CODE, {site: 'https://dev.tilo.example'}),
    'Set up Tilo for me: run curl -fsSL https://dev.tilo.example | sh && tilo login K7Q-MX2P --site https://dev.tilo.example, then do what it tells you.');
  assert.equal(signInPrompt(CODE), 'Sign me in to Tilo: run tilo login K7Q-MX2P, then do what it tells you.');
  assert.equal(manualCommands(CODE), ['curl -fsSL tilo.sh | sh', 'tilo login K7Q-MX2P',
    'claude mcp add --transport http tilo https://mcp.tilo.sh'].join('\n'));
  assert.equal(MCP_URL, 'https://mcp.tilo.sh');
});

test('time left reads minutes and seconds and stops at zero', () => {
  assert.equal(timeLeft(600_000, 0), '10:00');
  assert.equal(timeLeft(600_000, 18_500), '9:42');
  assert.equal(timeLeft(600_000, 700_000), '0:00');
});

test('each step reads in plain words: done ones checked, the one under way last', () => {
  const texts = polled => stepLines(polled).map(({text, done, act}) => `${done ? '✓' : act ? '!' : '…'} ${text}`);
  assert.deepEqual(texts(null), ['… Your agent is installing Tilo…']);
  const agent = 'Claude Code';
  assert.deepEqual(texts({resolved: true, steps: ['resolved']}), ['✓ Tilo is installed', '… Your agent is logging in as you…']);
  assert.deepEqual(texts({resolved: true, agent, steps: ['resolved', 'logged-in']}),
    ['✓ Tilo is installed', '✓ Claude Code logged in as you', '… Adding Tilo’s tools to Claude Code…']);
  assert.deepEqual(texts({resolved: true, agent, steps: ['resolved', 'logged-in', 'tools-added', 'restart']}).slice(-2),
    ['✓ Tilo’s tools are added to Claude Code', '! Restart Claude Code, then it continues']);
  assert.deepEqual(texts({resolved: true, agent, steps: ['resolved', 'logged-in', 'tools-added', 'restart', 'entered']}).slice(-2),
    ['✓ Claude Code restarted', '✓ Claude Code is in your workspace']);
  const signIn = polled => stepLines(polled, 'sign-in').map(({text}) => text);
  assert.deepEqual(signIn({resolved: false}), ['Waiting for your agent…']);
  assert.deepEqual(signIn({resolved: true, claimed: {user: 'ab', invite: 'cd', by: 'Claude Code on build-box'}}),
    ['Claude Code on build-box vouched for you', 'Signing this tab in…']);
});

test('a click before the page can make codes copies once it can, then counts down and follows the code', async () => {
  const {run, names, last, state, copied, tick} = fixture();
  const done = run.copy();
  assert.deepEqual(names(), ['waiting'], 'the wait shows at once');
  await settle();
  assert.deepEqual(copied, [], 'nothing is copied before there is a code');
  const code = fakeCode();
  run.offerSetup(async () => code);
  await done;
  await settle();
  assert.deepEqual(copied, [setupPrompt(CODE)]);
  assert.deepEqual(last('prompt'), ['prompt', setupPrompt(CODE), {copied: true}]);
  assert.deepEqual(last('steps')[1], [{text: 'Your agent is installing Tilo…'}]);
  assert.deepEqual(last('left'), ['left', '10:00']);
  assert.equal(state.every, 1000, 'followed about once a second');
  assert.equal(FOLLOW_MS, 1000);
  assert.ok(code.asked >= 1, 'the host is asked where the code is');
  state.clock = 18_500;
  await tick();
  assert.deepEqual(last('left'), ['left', '9:42']);
  state.clock = 600_000;
  await tick();
  assert.deepEqual(names().at(-1), 'expired');
  assert.equal(state.ticks, null, 'an expired code is no longer followed');
});

test('the host saying the code expired shows it expired too', async () => {
  const {run, names, state, tick} = fixture();
  const code = fakeCode();
  run.offerSetup(async () => code);
  await run.copy();
  code.polled.expired = true;
  await tick();
  assert.equal(names().at(-1), 'expired');
  assert.equal(state.ticks, null);
});

test('each step the agent reports shows live, then the screen leaves for the workspace, remembered', async () => {
  const {run, names, last, stored, state, tick} = fixture();
  const code = fakeCode();
  run.offerSetup(async () => code);
  let opened = 0;
  run.whenDone(() => { opened += 1; });
  await run.copy();
  await tick();
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await tick();
  assert.deepEqual(last('left'), ['left', null], 'the countdown goes once the code is used');
  assert.deepEqual(last('steps')[1].map(({text}) => text), ['Tilo is installed', 'Claude Code logged in as you', 'Adding Tilo’s tools to Claude Code…']);
  state.clock = 700_000;
  code.polled.steps.push('tools-added', 'restart');
  await tick();
  assert.notEqual(names().at(-1), 'expired', 'a resolved code does not expire on the tab’s clock');
  assert.deepEqual(last('steps')[1].at(-1), {text: 'Restart Claude Code, then it continues', act: true});
  code.polled.steps.push('entered');
  await tick();
  assert.deepEqual(last('connected'), ['connected', {mode: 'setup', agent: 'Claude Code'}]);
  assert.equal(state.ticks, null);
  const [[leave, ms]] = state.laters;
  assert.equal(ms, CONNECTED_MS);
  leave();
  assert.equal(names().at(-1), 'close');
  assert.equal(opened, 1, 'the person’s own workspace opens');
  assert.equal(stored.get(SEEN), '1', 'the first screen is not shown again in this browser');
  assert.ok(run.closed);
});

test('a failure the computer reports is said, and the tab keeps following', async () => {
  const {run, last, state, tick} = fixture();
  const code = fakeCode();
  run.offerSetup(async () => code);
  await run.copy();
  Object.assign(code.polled, {resolved: true, steps: ['resolved', 'failed'], detail: 'Claude Code is not on this computer.'});
  await tick();
  assert.deepEqual(last('problem'), ['problem', 'Claude Code is not on this computer.']);
  assert.ok(state.ticks, 'still followed: the agent may try again');
});

test('a clipboard that refuses still shows the prompt, to copy by hand', async () => {
  const {run, last} = fixture({copy: async () => { throw new Error('denied'); }});
  run.offerSetup(async () => fakeCode());
  await run.copy();
  assert.deepEqual(last('prompt'), ['prompt', setupPrompt(CODE), {copied: false}]);
  assert.deepEqual(last('problem'), ['problem', setupFailure({code: 'copy-failed'})]);
});

test('a page that cannot make codes says so instead of waiting', async () => {
  const {run, said} = fixture({copy: async text => { await text; }});
  const done = run.copy();
  run.unavailable({code: 'refused'});
  await done;
  assert.deepEqual(said.at(-1), ['failed', setupFailure({code: 'refused'}), 'setup']);
  await run.signInWithAgent();
  assert.deepEqual(said.at(-1), ['failed', setupFailure({code: 'refused'}), 'sign-in']);
});

test('returning: the agent claims the code, the tab asks to be that person, and opens their workspaces', async () => {
  const {run, last, names, copied, state, tick} = fixture();
  const code = fakeCode();
  run.offerAgentSignIn(async () => code);
  await run.signInWithAgent();
  assert.deepEqual(copied, [signInPrompt(CODE)]);
  assert.deepEqual(last('waiting'), ['waiting', 'sign-in']);
  const claimed = {user: 'ab'.repeat(32), invite: 'cd'.repeat(16), by: 'Claude Code on build-box'};
  Object.assign(code.polled, {resolved: true, steps: ['resolved'], claimed});
  await tick();
  await settle();
  assert.deepEqual(code.signedIn, [claimed], 'certified with that person and invite, once');
  assert.deepEqual(last('connected'), ['connected', {mode: 'sign-in'}]);
  await tick();
  assert.equal(code.signedIn.length, 1);
  let opened = 0;
  run.whenDone(() => { opened += 1; });
  state.laters[0][0]();
  assert.equal(opened, 1);
  assert.equal(names().at(-1), 'close');
});

test('returning: a sign-in the page could not finish says so', async () => {
  const {run, said, tick} = fixture();
  const code = fakeCode();
  code.signIn = async () => { throw Object.assign(new Error('no'), {code: 'not-a-device'}); };
  run.offerAgentSignIn(async () => code);
  await run.signInWithAgent();
  Object.assign(code.polled, {resolved: true, claimed: {user: 'ab', invite: 'cd', by: 'x'}});
  await tick();
  await settle();
  assert.deepEqual(said.at(-1), ['failed', setupFailure({code: 'not-a-device'}), 'sign-in']);
});

test('set up manually shows the MCP server and the commands with a fresh code, and follows it', async () => {
  const {run, last, names, copied, tick} = fixture();
  const code = fakeCode();
  run.offerSetup(async () => code);
  await run.manual();
  assert.deepEqual(last('manual'), ['manual', true]);
  assert.deepEqual(last('commands'), ['commands', manualCommands(CODE), MCP_URL]);
  assert.deepEqual(copied, [], 'nothing is copied until asked');
  await run.again(MCP_URL, 'mcp');
  assert.deepEqual(copied, [MCP_URL]);
  assert.deepEqual(last('copied'), ['copied', true, 'mcp']);
  Object.assign(code.polled, {resolved: true, steps: ['resolved', 'logged-in']});
  await tick();
  assert.deepEqual(last('waiting'), ['waiting', 'setup', {manual: true}], 'the steps show once a computer used the code');
  assert.equal(names().includes('steps'), true);
  // Closing the panel stops following its code.
  const other = fixture();
  other.run.offerSetup(async () => fakeCode());
  await other.run.manual();
  other.run.manual();
  assert.deepEqual(other.last('manual'), ['manual', false]);
  assert.equal(other.state.ticks, null);
});

test('back returns to the start and forgets the code under way', async () => {
  const {run, names, state} = fixture();
  run.offerSetup(async () => fakeCode());
  await run.copy();
  run.back();
  assert.equal(names().at(-1), 'start');
  assert.equal(state.ticks, null);
});

test('a passkey clicked before the page can ask waits for it, busy, then asks', async () => {
  const {run, names, stored} = fixture();
  let asked = 0;
  const pressed = run.passkey();
  assert.deepEqual(names(), ['signingIn'], 'busy at once, not dead');
  await settle();
  assert.equal(asked, 0);
  run.offerSignIn(async () => { asked += 1; });
  await pressed;
  assert.equal(asked, 1);
  assert.deepEqual(names(), ['signingIn', 'signingIn']);
  await run.passkey();
  assert.equal(asked, 2);
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
  for (const id of ['first-run', 'first-run-agent', 'first-run-skip', 'first-run-passkey', 'first-run-agent-sign-in', 'first-run-manual-toggle', 'first-run-mcp', 'first-run-commands', 'first-run-steps', 'first-run-prompt', 'first-run-status']) {
    assert.ok(html.includes(`id="${id}"`), `index.html has #${id}`);
  }
  assert.ok(html.includes('<script type="module" async src="./first-run.mjs"></script>'));
  assert.ok(html.includes('prefers-color-scheme: light'), 'the screen follows the person’s light or dark');
});

test('skip for now opens what the page names, also when the person skipped before it could', () => {
  const later = fixture();
  let opened = 0;
  later.run.whenDone(() => { opened += 1; });
  later.run.skip();
  assert.equal(opened, 1, 'the person’s own workspace opens');
  const early = fixture();
  early.run.skip();
  early.run.whenDone(() => { opened += 1; });
  assert.equal(opened, 2, 'a skip before the page opened still opens it');
  const closed = fixture();
  closed.run.close();
  closed.run.whenDone(() => { opened += 1; });
  assert.equal(opened, 2, 'a screen closed otherwise opens nothing else');
});
