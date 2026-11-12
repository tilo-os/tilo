import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import {ASK_MS, codeWait, showPrompt, CONNECTED_MS, QUIET_MS, createFirstRun, FOLLOW_MS, MAKE_MS, manualCommands, MCP_URL, OFFER_MS, PASSKEY_WAIT_MS, RENEW_MS,
  readSetup, SETUP, setupFailure, setupPrompt, signInPrompt, stepLines, minutesLeft} from './first-run.mjs';

const here = name => new URL(name, import.meta.url);
const CODE = 'K7Q-MX2P';
const NEXT = 'R3T-WX9Z';

/** A view that records what it was told, in order. */
function fakeView() {
  const said = [];
  const view = new Proxy({}, {get: (_, name) => (...args) => { said.push([name, ...args]); }});
  return {view, said, names: () => said.map(([name]) => name), last: name => said.findLast(([n]) => n === name)};
}

/** A code the host made, whose `follow()` answers what `polled` holds now. */
function fakeCode({code = CODE, expiresAt = 600_000} = {}) {
  const made = {code, expiresAt, polled: {resolved: false, expired: false, steps: []}, asked: 0, signedIn: [],
    saved: {code, expiresAt, follow: `follow-${code}`}};
  made.follow = async () => { made.asked += 1; return structuredClone(made.polled); };
  made.signIn = async claimed => { made.signedIn.push(claimed); };
  return made;
}

/** The host's codes, one after another: `next()` makes the next of `codes`
 * (fresh ones after), and counts how often it was asked. */
function maker(...codes) {
  const made = [];
  const make = async () => {
    const next = codes[made.length] ?? fakeCode({code: `Z${made.length}Z-AAAA`});
    made.push(next);
    return next;
  };
  return {make, made};
}

function fixture({copy, site, stored = new Map(), online = () => true, clock = 0} = {}) {
  const {view, said, names, last} = fakeView();
  const copied = [];
  const state = {clock, ticks: null, every: null, timers: new Map(), id: 0, reloads: 0};
  const run = createFirstRun({view, site,
    // The clipboard takes the text at once, as `writeText` does inside the click.
    clipboard: {copy: copy ?? (text => { copied.push(text); return Promise.resolve(); })},
    storage: {setItem: (key, value) => stored.set(key, value), getItem: key => stored.get(key) ?? null},
    now: () => state.clock, every: (tick, ms) => { state.ticks = tick; state.every = ms; return () => { state.ticks = null; }; },
    later: (run, ms) => { const id = ++state.id; state.timers.set(id, {run, ms}); return id; },
    clear: id => state.timers.delete(id),
    reload: () => { state.reloads += 1; }, online: () => online()});
  // One follow round: the tick, and the host's answer.
  const tick = async () => { state.ticks?.(); await settle(); };
  // Run the timers set for `ms`.
  const fire = async ms => {
    for (const [id, timer] of [...state.timers]) if (timer.ms === ms) { state.timers.delete(id); timer.run(); }
    await settle();
  };
  const kept = () => readSetup({getItem: key => stored.get(key) ?? null});
  return {run, said, names, last, stored, state, copied, tick, fire, kept};
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

test('time left reads in whole minutes, counted up, never none while the code works', () => {
  assert.equal(minutesLeft(600_000, 0), '10 more minutes');
  assert.equal(minutesLeft(600_000, 18_500), '10 more minutes');
  assert.equal(minutesLeft(600_000, 61_000), '9 more minutes');
  assert.equal(minutesLeft(600_000, 570_000), '1 more minute');
  assert.equal(minutesLeft(600_000, 700_000), '1 more minute');
});

test('each step reads in plain words: done ones checked, the one under way last', () => {
  const texts = polled => stepLines(polled).map(({text, done, act, wait}) => `${done ? '✓' : act ? '!' : wait ? '·' : '…'} ${text}`);
  // Until the host sees the code used, nothing is under way, and nothing says it is.
  assert.deepEqual(texts(null), ['· Waiting for your agent']);
  assert.deepEqual(texts({resolved: false}), ['· Waiting for your agent']);
  const agent = 'Claude Code';
  assert.deepEqual(texts({resolved: true, steps: ['resolved']}), ['✓ Tilo is installed', '… Your agent is logging in as you…']);
  assert.deepEqual(texts({resolved: true, agent, steps: ['resolved', 'logged-in']}),
    ['✓ Tilo is installed', '✓ Claude Code logged in as you', '… Adding Tilo’s tools to Claude Code…']);
  assert.deepEqual(texts({resolved: true, agent, steps: ['resolved', 'logged-in', 'tools-added', 'restart']}).slice(-2),
    ['✓ Tilo’s tools are added to Claude Code', '! Restart Claude Code, then it continues']);
  // A restart the agent was asked for is not one that happened: the screen
  // claims only the steps the CLI reported.
  assert.deepEqual(texts({resolved: true, agent, steps: ['resolved', 'logged-in', 'tools-added', 'restart', 'entered']}).slice(-2),
    ['✓ Tilo’s tools are added to Claude Code', '✓ Claude Code is in your workspace']);
  assert.deepEqual(texts({resolved: true, agent, steps: ['resolved', 'logged-in', 'entered']}).slice(-2),
    ['✓ Claude Code logged in as you', '✓ Claude Code is in your workspace'],
    'tools are never said to be added when the CLI did not confirm them');
  const signIn = polled => stepLines(polled, 'sign-in').map(({text}) => text);
  assert.deepEqual(signIn({resolved: false}), ['Waiting for your agent']);
  assert.deepEqual(stepLines(null, 'sign-in'), [{text: 'Waiting for your agent', wait: true}]);
  assert.deepEqual(signIn({resolved: true, claimed: {user: 'ab', invite: 'cd', by: 'Claude Code on build-box'}}),
    ['Claude Code on build-box vouched for you', 'Signing this tab in…']);
});

test('the prompt is on the screen before any click: the page makes its code ahead', async () => {
  const {run, last, copied} = fixture();
  assert.deepEqual(last('offer'), ['offer', 'setup', {note: codeWait()}], 'until then the screen says what it waits for');
  run.offerSetup(maker(fakeCode()).make);
  await settle();
  assert.deepEqual(last('offer'), ['offer', 'setup', {text: setupPrompt(CODE), code: CODE}]);
  assert.deepEqual(copied, [], 'nothing is copied before the person asks');
});

test('the click copies exactly the prompt shown, inside the click, and only then says it copied', async () => {
  const {run, last, names, copied, state} = fixture();
  const codes = maker(fakeCode(), fakeCode({code: NEXT}));
  run.offerSetup(codes.make);
  await settle();
  const [, , {text: shown}] = last('offer');
  const done = run.copy();
  assert.deepEqual(copied, [shown], 'written at once, in the click, with nothing awaited first');
  assert.equal(names().includes('waiting'), false, 'no claim before the clipboard answered');
  await done;
  assert.deepEqual(last('waiting'), ['waiting', 'setup', {text: shown, copied: true}]);
  assert.deepEqual(last('steps')[1], [{text: 'Waiting for your agent', wait: true}], 'nothing claimed before the agent used the code');
  assert.deepEqual(last('left'), ['left', '10 more minutes']);
  assert.equal(state.every, FOLLOW_MS, 'followed about once a second');
  assert.ok(codes.made[0].asked >= 1, 'the host is asked where the copied code is');
  await settle();
  assert.equal(codes.made.length, 2, 'a fresh code is made for another try');
  assert.deepEqual(last('offer')[2].code, NEXT);
});

test('a click before the page has a code copies nothing and says what it waits for', async () => {
  const {run, last, names, copied} = fixture({site: 'https://tilo.sh'});
  assert.equal(await run.copy(), false);
  assert.deepEqual(copied, []);
  assert.deepEqual(last('notReady'), ['notReady', 'setup', 'Getting your code from tilo.sh…']);
  assert.equal(names().includes('waiting'), false, 'the button stays where it is');
  await run.signInWithAgent();
  await settle();
  assert.deepEqual(last('notReady'), ['notReady', 'signIn', 'Getting your code from tilo.sh…']);
});

test('a code the host never makes ends in a stated reason with a retry, not a hang', async () => {
  const {run, last, fire, copied} = fixture({site: 'https://tilo.sh'});
  let asked = 0;
  run.offerSetup(() => { asked += 1; return new Promise(() => {}); });
  await settle();
  assert.deepEqual(last('offer'), ['offer', 'setup', {note: 'Getting your code from tilo.sh…'}]);
  await fire(MAKE_MS);
  assert.deepEqual(last('offer'), ['offer', 'setup', {problem: 'Couldn’t reach tilo.sh. Check your connection, then try again.'}]);
  assert.equal(await run.copy(), false, 'a click copies nothing');
  assert.deepEqual(copied, []);
  assert.equal(asked, 2, 'and asks the host again');
  await run.retry();
  assert.equal(asked, 2, 'one ask at a time');
  await fire(MAKE_MS);
  void run.retry();
  await settle();
  assert.equal(asked, 3, 'retry asks again');
});

test('a host that refuses, or a page that never offers codes, is said, not waited on', async () => {
  const refused = fixture();
  refused.run.offerSetup(async () => { throw Object.assign(new Error('refused'), {code: 'refused'}); });
  await settle();
  await settle();
  assert.deepEqual(refused.last('offer'), ['offer', 'setup', {problem: setupFailure({code: 'refused'})}]);
  const silent = fixture();
  await silent.fire(OFFER_MS);
  const offered = kind => silent.said.findLast(([name, of]) => name === 'offer' && of === kind);
  assert.deepEqual(offered('setup'), ['offer', 'setup', {problem: setupFailure({code: 'not-loaded'})}]);
  assert.deepEqual(offered('signIn'), ['offer', 'signIn', {problem: setupFailure({code: 'not-loaded'})}]);
  assert.match(setupFailure({code: 'not-loaded'}), /Reload/);
  await silent.run.retry();
  assert.equal(silent.state.reloads, 1, 'trying again loads the page again');
  const gone = fixture();
  gone.run.unavailable({code: 'refused'});
  assert.deepEqual(gone.last('offer'), ['offer', 'signIn', {problem: setupFailure({code: 'refused'})}]);
  await gone.run.signInWithAgent();
  assert.deepEqual(gone.last('notReady'), ['notReady', 'signIn', setupFailure({code: 'refused'})]);
  assert.equal(setupFailure({code: 'not-let-in'}), 'This tab isn’t let in yet. Try again in a moment.');
});

test('a clipboard that refuses claims no copy: the prompt stays, to copy by hand', async () => {
  const {run, last, names} = fixture({copy: async () => { throw new Error('denied'); }});
  run.offerSetup(maker(fakeCode()).make);
  await settle();
  assert.equal(await run.copy(), false);
  assert.deepEqual(last('waiting'), ['waiting', 'setup', {text: setupPrompt(CODE), copied: false}]);
  assert.deepEqual(last('problem'), ['problem', setupFailure({code: 'copy-failed'})]);
  assert.equal(names().includes('copied'), false, 'nothing says it copied');
  const thrown = fixture({copy: () => { throw new Error('no clipboard'); }});
  thrown.run.offerSetup(maker(fakeCode()).make);
  await settle();
  assert.equal(await thrown.run.copy(), false);
  assert.deepEqual(thrown.last('waiting')[2], {text: setupPrompt(CODE), copied: false});
  await thrown.run.again('text');
  assert.deepEqual(thrown.last('copied'), ['copied', false, 'prompt']);
});

test('a prompt copied by hand is followed like one copied with the button', async () => {
  const {run, last, copied} = fixture();
  run.copiedByHand();
  assert.equal(last('waiting'), undefined, 'nothing shown, nothing copied');
  run.offerSetup(maker(fakeCode()).make);
  await settle();
  run.copiedByHand();
  assert.deepEqual(last('waiting'), ['waiting', 'setup', {text: setupPrompt(CODE), copied: true}]);
  assert.deepEqual(copied, []);
});

test('a ready code is renewed shortly before it expires, the old one shown until then', async () => {
  const {run, last, fire, names} = fixture();
  const codes = maker(fakeCode(), fakeCode({code: NEXT, expiresAt: 1_200_000}));
  run.offerSetup(codes.make);
  await settle();
  const offers = names().filter(name => name === 'offer').length;
  await fire(600_000 - RENEW_MS);
  assert.equal(codes.made.length, 2);
  assert.equal(names().filter(name => name === 'offer').length, offers + 1, 'no wait shown in between');
  assert.deepEqual(last('offer'), ['offer', 'setup', {text: setupPrompt(NEXT), code: NEXT}]);
});

test('a copied code that expires says so, and the retry copies the fresh one at once', async () => {
  const {run, names, last, state, tick, copied} = fixture();
  run.offerSetup(maker(fakeCode(), fakeCode({code: NEXT, expiresAt: 1_200_000})).make);
  await settle();
  await run.copy();
  await settle();
  state.clock = 18_500;
  await tick();
  assert.deepEqual(last('left'), ['left', '10 more minutes']);
  state.clock = 61_000;
  await tick();
  assert.deepEqual(last('left'), ['left', '9 more minutes']);
  state.clock = 600_000;
  await tick();
  assert.equal(names().at(-1), 'expired');
  assert.equal(state.ticks, null, 'an expired code is no longer followed');
  await run.retry('setup');
  assert.deepEqual(copied, [setupPrompt(CODE), setupPrompt(NEXT)]);
  assert.deepEqual(last('waiting')[2], {text: setupPrompt(NEXT), copied: true});
});

test('the host saying the code expired shows it expired too', async () => {
  const {run, names, state, tick} = fixture();
  const code = fakeCode();
  run.offerSetup(maker(code).make);
  await settle();
  await run.copy();
  await settle();
  code.polled.expired = true;
  await tick();
  assert.deepEqual(names().filter(name => ['expired', 'steps', 'left', 'connected'].includes(name)).at(-1), 'expired');
  assert.equal(state.ticks, null);
});

test('each step the agent reports shows live, then the screen leaves for the workspace, remembered', async () => {
  const {run, names, last, stored, state, tick} = fixture();
  const code = fakeCode();
  run.offerSetup(maker(code).make);
  await settle();
  let opened = 0;
  run.whenDone(() => { opened += 1; });
  await run.copy();
  await settle();
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
  assert.deepEqual(last('connected'), ['connected', {mode: 'setup', agent: 'Claude Code', heading: 'Claude Code is in your workspace'}]);
  assert.equal(readSetup({getItem: key => stored.get(key)}).step, 'connected', 'kept: a reload says so too');
  assert.equal(state.ticks, null);
  const leave = [...state.timers.values()].find(timer => timer.ms === CONNECTED_MS);
  leave.run();
  assert.deepEqual(last('opening'), ['opening', {heading: 'Claude Code is in your workspace'}], 'it says it opens the workspace');
  await settle();
  assert.equal(names().at(-1), 'close');
  assert.equal(opened, 1, 'the person’s own workspace opens');
  assert.equal(readSetup({getItem: key => stored.get(key)}).step, 'done', 'the first screen is not shown again in this browser');
  assert.ok(run.closed);
  assert.equal([...state.timers.values()].some(timer => timer.ms !== CONNECTED_MS), false, 'no renewal outlives the screen');
});

test('a failure the computer reports is said, and the tab keeps following', async () => {
  const {run, last, state, tick} = fixture();
  const code = fakeCode();
  run.offerSetup(maker(code).make);
  await settle();
  await run.copy();
  await settle();
  Object.assign(code.polled, {resolved: true, steps: ['resolved', 'failed'], detail: 'Claude Code is not on this computer.'});
  await tick();
  assert.deepEqual(last('agentFailed'), ['agentFailed', 'Claude Code is not on this computer.'], 'the agent’s reason');
  assert.deepEqual(last('steps')[1].at(-1), {text: 'Your agent could not log in as you', failed: true}, 'the step it stopped at, not under way');
  assert.ok(state.ticks, 'still followed: the agent may try again');
});

test('returning: the agent claims the code, the tab asks to be that person, and opens their workspaces', async () => {
  const {run, last, names, copied, state, tick} = fixture();
  const code = fakeCode();
  run.offerAgentSignIn(maker(code).make);
  await settle();
  await run.signInWithAgent();
  await settle();
  assert.deepEqual(copied, [signInPrompt(CODE)]);
  assert.deepEqual(last('waiting'), ['waiting', 'sign-in', {text: signInPrompt(CODE), copied: true}]);
  const claimed = {user: 'ab'.repeat(32), invite: 'cd'.repeat(16), by: 'Claude Code on build-box'};
  Object.assign(code.polled, {resolved: true, steps: ['resolved'], claimed});
  await tick();
  await settle();
  assert.deepEqual(code.signedIn, [claimed], 'certified with that person and invite, once');
  assert.deepEqual(last('connected'), ['connected', {mode: 'sign-in', agent: null, heading: 'You’re signed in'}]);
  await tick();
  assert.equal(code.signedIn.length, 1);
  let opened = 0;
  run.whenDone(() => { opened += 1; });
  [...state.timers.values()].find(timer => timer.ms === CONNECTED_MS).run();
  assert.equal(opened, 1);
  assert.equal(names().at(-1), 'close');
});

test('returning: signed in before the page can open their workspace, the tab opens it once it can', async () => {
  const {run, last, names, state, tick} = fixture();
  const code = fakeCode();
  run.offerAgentSignIn(maker(code).make);
  await settle();
  await run.signInWithAgent();
  await settle();
  Object.assign(code.polled, {resolved: true, steps: ['resolved'], claimed: {user: 'ab'.repeat(32), invite: 'cd'.repeat(16), by: 'Claude Code'}});
  await tick();
  await settle();
  assert.deepEqual(last('connected'), ['connected', {mode: 'sign-in', agent: null, heading: 'You’re signed in'}]);
  // The moment it said so is over before the page can open anything.
  [...state.timers.values()].find(timer => timer.ms === CONNECTED_MS).run();
  assert.notEqual(names().at(-1), 'close', 'the screen waits for what it opens, rather than leaving the tab where it stands');
  let opened = 0;
  run.whenDone(() => { opened += 1; });
  assert.equal(opened, 1, 'the person\'s own workspace opens once the page can');
  assert.equal(names().at(-1), 'close');
  run.whenDone(() => { opened += 1; });
  assert.equal(opened, 1, 'and only once');
});

test('returning: a sign-in the page could not finish says why', async () => {
  const {run, last, tick} = fixture();
  const code = fakeCode();
  code.signIn = async () => { throw Object.assign(new Error('no'), {code: 'not-let-in'}); };
  run.offerAgentSignIn(maker(code).make);
  await settle();
  await run.signInWithAgent();
  await settle();
  Object.assign(code.polled, {resolved: true, claimed: {user: 'ab', invite: 'cd', by: 'x'}});
  await tick();
  await settle();
  assert.deepEqual(last('failed'), ['failed', 'This tab isn’t let in yet. Try again in a moment.', 'sign-in']);
});

test('set up manually shows the MCP server and the commands with the code shown, and follows it', async () => {
  const {run, last, names, copied, tick} = fixture();
  const code = fakeCode();
  run.offerSetup(maker(code).make);
  await settle();
  run.manual();
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
  other.run.offerSetup(maker(fakeCode()).make);
  await settle();
  other.run.manual();
  other.run.manual();
  assert.deepEqual(other.last('manual'), ['manual', false]);
  assert.equal(other.state.ticks, null);
  // Without a code yet the panel says what it waits for.
  const early = fixture({site: 'https://tilo.sh'});
  early.run.manual();
  assert.deepEqual(early.last('commands'), ['commands', null, MCP_URL, 'Getting your code from tilo.sh…']);
});

test('back returns to the start, with the next prompt shown, and forgets the code under way', async () => {
  const {run, names, last, state} = fixture();
  run.offerSetup(maker(fakeCode(), fakeCode({code: NEXT})).make);
  await settle();
  await run.copy();
  await settle();
  run.back();
  assert.deepEqual(names().slice(-2), ['start', 'offer']);
  assert.deepEqual(last('offer')[2].code, NEXT);
  assert.equal(state.ticks, null);
});

test('a passkey clicked before the page can ask waits for it, busy, then asks; a page that never can says so', async () => {
  const {run, names, stored} = fixture();
  let asked = 0;
  const pressed = run.passkey();
  assert.deepEqual(names().slice(-1), ['signingIn'], 'busy at once, not dead');
  await settle();
  assert.equal(asked, 0);
  run.offerSignIn(async () => { asked += 1; });
  await pressed;
  assert.equal(asked, 1);
  await run.passkey();
  assert.equal(asked, 2);
  run.skip();
  assert.equal(names().at(-1), 'opening', 'the screen stays until the page can open the workspace');
  run.whenDone(() => {});
  await settle();
  assert.equal(names().at(-1), 'close');
  assert.equal(readSetup({getItem: key => stored.get(key)}).step, 'done');
  const never = fixture();
  const waiting = never.run.passkey();
  await never.fire(PASSKEY_WAIT_MS);
  await waiting;
  assert.deepEqual(never.names().slice(-2), ['signInFailed', 'signingIn'], 'the wait ends, said');
});

/** first-run.inline.js against a page at `href`, with `stored` keys and the
 * scrubber's `secrets`: whether the first screen shows. */
function shows(href, {stored = {}, secrets = {}, storage = true, step = false} = {}) {
  const url = new URL(href);
  const attributes = new Map();
  const page = {
    document: {documentElement: {setAttribute: (name, value) => attributes.set(name, value)}, addEventListener() {}},
    location: {hash: url.hash, search: url.search, pathname: url.pathname, host: url.host},
    localStorage: storage ? {getItem: key => stored[key] ?? null, setItem: (key, value) => { stored[key] = value; }} : undefined,
    Date,
  };
  page.globalThis = page;
  page.__tiloLinkSecrets = secrets;
  if (!storage) Object.defineProperty(page, 'localStorage', {get() { throw new Error('denied'); }});
  vm.runInNewContext(readFileSync(here('first-run.inline.js'), 'utf8'), page);
  if (step) return attributes.get('data-first-run') ?? null;
  return attributes.has('data-first-run');
}
const kept = state => ({[SETUP]: JSON.stringify(state)});

test('only a browser new here, opening the site itself, sees the first screen', () => {
  assert.equal(shows('https://tilo.example/'), true);
  assert.equal(shows('https://tilo.example/#/'), true);
  assert.equal(shows('https://tilo.example/?debug'), true);
  assert.equal(shows('https://tilo.example/', {stored: kept({step: 'done'})}), false, 'set up already');
  assert.equal(shows('https://tilo.example/', {stored: {'tilo-host-memberships': '{}'}}), false, 'a host let it in before');
  assert.equal(shows('https://tilo.example/', {stored: {'tilo-entered-workspaces': '[]'}}), false);
  assert.equal(shows('https://tilo.example/', {secrets: {invite: 'x'}}), false, 'an invitation opens as it did');
  assert.equal(shows('https://tilo.example/#/@rooms.example/w/abc'), false, 'a link to a workspace');
  assert.equal(shows('https://tilo.example/?embed=1'), false, 'inside another site');
  assert.equal(shows('https://tilo.example/?workspace=Lounge'), false);
  assert.equal(shows('https://tilo.example/', {storage: false}), false, 'no storage: as it always opened');
});

test('the kept setup alone decides the step a bare entry opens on, whatever the page did behind the screen', () => {
  const fresh = {};
  assert.equal(shows('https://tilo.example/', {stored: fresh, step: true}), '', 'a browser new here starts');
  assert.equal(JSON.parse(fresh[SETUP]).step, 'start', 'and its setup is kept from the first paint');
  const lounge = {'tilo-host-memberships': '{"x":{}}', 'tilo-entered-workspaces': '[{}]'};
  const code = {code: 'K7Q-MX2P', expiresAt: Date.now() + 300_000, follow: 'f'.repeat(64)};
  assert.equal(shows('https://tilo.example/', {stored: {...lounge, ...kept({step: 'start'})}, step: true}), '',
    'mid-setup after the home workspace let it in: still the first screen, not the home workspace');
  assert.equal(shows('https://tilo.example/', {stored: {...lounge, ...kept({step: 'waiting', code})}, step: true}), 'waiting');
  assert.equal(shows('https://tilo.example/', {stored: kept({step: 'waiting', code: {...code, expiresAt: Date.now() - 1}}), step: true}), 'expired',
    'a code that ran out while the tab was closed has run out');
  assert.equal(shows('https://tilo.example/', {stored: kept({step: 'waiting', code: {...code, expiresAt: Date.now() - 1},
    polled: {resolved: true, steps: ['resolved']}}), step: true}), 'waiting', 'a code the agent used is followed past its time');
  assert.equal(shows('https://tilo.example/', {stored: kept({step: 'connected'}), step: true}), 'connected');
  assert.equal(shows('https://tilo.example/', {stored: kept({step: 'opening'}), step: true}), 'connected', 'opening shows the same screen');
  assert.equal(shows('https://tilo.example/', {stored: {...lounge, ...kept({step: 'done'})}, step: true}), null, 'done: where the page opens');
  assert.equal(shows('https://tilo.example/#/@rooms.example/w/lounge', {stored: {...lounge, ...kept({step: 'waiting', code})}, step: true}), 'waiting',
    'the home workspace’s own address, written once it let the page in behind the screen, still shows the step');
  assert.equal(shows('https://tilo.example/#/@rooms.example/w/abc?invite=x', {stored: kept({step: 'waiting', code}), secrets: {invite: 'x'}}), false,
    'an invitation still opens what it names');
  assert.equal(shows('https://tilo.example/?embed=1', {stored: kept({step: 'waiting', code})}), false, 'inside another site: never');
});

test('the desktop entry page inlines the first screen’s script unchanged, after the scrubber, and carries its screen', () => {
  const html = readFileSync(here('index.html'), 'utf8');
  const script = readFileSync(here('first-run.inline.js'), 'utf8');
  const at = html.indexOf(`<script>\n${script}</script>`);
  assert.ok(at > 0, 'index.html must inline first-run.inline.js unchanged');
  assert.ok(html.indexOf('__tiloLinkSecrets') < at, 'after the scrubber, whose secrets it reads');
  for (const id of ['first-run', 'first-run-agent', 'first-run-offer', 'first-run-offer-note', 'first-run-offer-retry', 'first-run-sign-in-note', 'first-run-skip', 'first-run-passkey', 'first-run-agent-sign-in', 'first-run-manual-toggle', 'first-run-mcp', 'first-run-commands', 'first-run-steps', 'first-run-prompt', 'first-run-status']) {
    assert.ok(html.includes(`id="${id}"`), `index.html has #${id}`);
  }
  assert.ok(html.includes('<script type="module" async src="./first-run.mjs"></script>'));
  assert.ok(html.includes('prefers-color-scheme: light'), 'the screen follows the person’s light or dark');
});

test('skip for now opens what the page names, also when the person skipped before it could', async () => {
  const later = fixture();
  let opened = 0;
  later.run.whenDone(() => { opened += 1; });
  later.run.skip();
  await settle();
  assert.equal(opened, 1, 'the person’s own workspace opens');
  const early = fixture();
  early.run.skip();
  assert.equal(readSetup({getItem: key => early.stored.get(key)}).step, 'opening', 'kept: a reload opens it as well');
  early.run.whenDone(() => { opened += 1; });
  await settle();
  assert.equal(opened, 2, 'a skip before the page opened still opens it');
  const closed = fixture();
  closed.run.close();
  closed.run.whenDone(() => { opened += 1; });
  assert.equal(opened, 2, 'a screen closed otherwise opens nothing else');
});

test('a press before the module could answer is pressed again once it can, the last one only', async () => {
  const {pressEarly} = await import('./first-run.mjs');
  const pressed = [];
  const document = {getElementById: id => ({click: () => pressed.push(id)})};
  const scope = {__tiloFirstRunEarly: ['first-run-manual-toggle', 'first-run-agent']};
  pressEarly(document, scope);
  assert.deepEqual(pressed, ['first-run-agent']);
  assert.equal(scope.__tiloFirstRunReady, true, 'later presses reach the buttons themselves');
  pressEarly(document, scope);
  assert.deepEqual(pressed, ['first-run-agent'], 'a press is replayed once');
  pressEarly(document, {});
  assert.deepEqual(pressed, ['first-run-agent'], 'nothing pressed early, nothing pressed');
});

/** Codes kept by the setup, followed again: `resume(saved)` answers the
 * code of that name among `codes`. */
const resumer = (...codes) => saved => codes.find(code => code.code === saved.code) ?? null;

test('a reload mid-way shows the step that was kept at once, then follows the same code', async () => {
  const stored = new Map();
  const first = fixture({stored});
  const code = fakeCode();
  first.run.offerSetup(maker(code).make, resumer(code));
  await settle();
  await first.run.copy();
  await settle();
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await first.tick();
  assert.equal(first.kept().step, 'waiting');
  assert.deepEqual(first.kept().polled.steps, ['resolved', 'logged-in'], 'what the agent did is kept');
  assert.deepEqual(first.kept().shown.lines.map(({text}) => text).slice(0, 2), ['Tilo is installed', 'Claude Code logged in as you'],
    'and what the screen drew, for the next first paint');
  // Loaded again: the page has no codes to offer yet.
  const again = fixture({stored});
  assert.deepEqual(again.last('waiting'), ['waiting', 'setup', {text: setupPrompt(CODE), copied: true, manual: false}], 'the same step, before the host answers');
  assert.deepEqual(again.last('steps')[1].map(({text}) => text).slice(0, 2), ['Tilo is installed', 'Claude Code logged in as you']);
  assert.deepEqual(again.last('net'), ['net', 'Checking with tilo.sh…'], 'and that it is checking');
  const made = maker(fakeCode({code: NEXT}));
  again.run.offerSetup(made.make, resumer(code));
  await settle();
  assert.ok(code.asked > 1, 'the kept code is followed again');
  assert.deepEqual(again.last('net'), ['net', null], 'confirmed once the host answered');
  assert.equal(again.names().includes('start'), false, 'never back at the start');
  // The agent finished while nobody looked: the screen says so, and leaves.
  code.polled.steps.push('tools-added', 'restart', 'entered');
  await again.tick();
  assert.equal(again.last('connected')[1].heading, 'Claude Code is in your workspace');
});

test('the agent finishing while the tab was closed is said on reopening, and the workspace opens', async () => {
  const stored = new Map();
  const code = fakeCode();
  code.polled = {resolved: true, expired: false, agent: 'Claude Code', steps: ['resolved', 'logged-in', 'tools-added', 'restart', 'entered']};
  stored.set(SETUP, JSON.stringify({step: 'waiting', mode: 'setup', copied: true, code: {...code.saved, kind: 'setup'},
    polled: {resolved: true, steps: ['resolved', 'logged-in'], agent: 'Claude Code', detail: null}}));
  const back = fixture({stored});
  let opened = 0;
  back.run.whenDone(() => { opened += 1; });
  back.run.offerSetup(maker(fakeCode({code: NEXT})).make, resumer(code));
  await settle();
  assert.equal(back.last('connected')[1].heading, 'Claude Code is in your workspace');
  [...back.state.timers.values()].find(timer => timer.ms === CONNECTED_MS).run();
  await settle();
  assert.equal(opened, 1);
  assert.equal(back.kept().step, 'done');
});

test('a code that runs out shows a fresh one with a note, kept so, and copying it follows the fresh one', async () => {
  const stored = new Map();
  const {run, last, state, tick, copied, kept} = fixture({stored});
  const code = fakeCode();
  const next = fakeCode({code: NEXT, expiresAt: 1_300_000});
  run.offerSetup(maker(code, next).make, resumer(code, next));
  await settle();
  await run.copy();
  await settle();
  state.clock = 600_000;
  await tick();
  assert.deepEqual(last('expired'), ['expired', 'setup', {text: setupPrompt(NEXT)}], 'the fresh prompt shows');
  assert.equal(kept().step, 'expired');
  // Reloaded now, the screen still says it ran out, with the fresh prompt.
  const again = fixture({stored, clock: 600_000});
  again.run.offerSetup(maker().make, resumer(code, next));
  await settle();
  assert.deepEqual(again.last('expired'), ['expired', 'setup', {text: setupPrompt(NEXT)}]);
  await run.again('', 'prompt');
  await settle();
  assert.deepEqual(copied.at(-1), setupPrompt(NEXT), 'the Copy beside it copies the fresh prompt');
  assert.equal(kept().step, 'waiting');
  assert.equal(kept().code.code, NEXT, 'and follows it');
});

test('offline, then online: said, kept following, picked up at once', async () => {
  let online = true;
  const {run, last, tick} = fixture({online: () => online});
  const code = fakeCode();
  run.offerSetup(maker(code).make);
  await settle();
  await run.copy();
  await settle();
  const follow = code.follow;
  code.follow = () => Promise.reject(new TypeError('Failed to fetch'));
  online = false;
  await tick();
  assert.deepEqual(last('net'), ['net', 'You’re offline.']);
  online = true;
  code.follow = follow;
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  run.online();
  await settle();
  assert.deepEqual(last('net'), ['net', null]);
  assert.equal(last('steps')[1][1].text, 'Claude Code logged in as you');
});

test('a slow host and a quiet agent are each said, with the reason, while the tab keeps following', async () => {
  const {run, last, state, tick, fire} = fixture();
  const code = fakeCode();
  run.offerSetup(maker(code).make);
  await settle();
  await run.copy();
  await settle();
  const follow = code.follow;
  code.follow = () => new Promise(() => {});
  await tick();
  await fire(ASK_MS);
  assert.deepEqual(last('net'), ['net', 'tilo.sh isn’t answering.']);
  code.follow = follow;
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await tick();
  assert.deepEqual(last('net'), ['net', null]);
  state.clock += QUIET_MS;
  await tick();
  assert.match(last('net')[1], /^Claude Code has reported nothing new for 3 minutes\. Look at its window/);
  assert.ok(state.ticks, 'still following');
});

test('a host that forgot the code: an agent that logged in opens the workspace, else a fresh prompt', async () => {
  const gone = Object.assign(new Error('no such login code'), {status: 404});
  const logged = fixture();
  const code = fakeCode();
  logged.run.offerSetup(maker(code).make);
  await settle();
  await logged.run.copy();
  await settle();
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await logged.tick();
  code.follow = () => Promise.reject(gone);
  await logged.tick();
  assert.equal(logged.names().at(-1), 'opening');
  assert.equal(logged.kept().step, 'opening');
  const unused = fixture();
  const other = fakeCode();
  unused.run.offerSetup(maker(other).make);
  await settle();
  await unused.run.copy();
  await settle();
  other.follow = () => Promise.reject(gone);
  await unused.tick();
  assert.equal(unused.last('expired')[1], 'setup');
});

test('a second tab follows the first: the same prompt on the start, the same code once copied, back together', async () => {
  const stored = new Map();
  const code = fakeCode();
  const a = fixture({stored});
  const made = maker(code, fakeCode({code: NEXT}));
  a.run.offerSetup(made.make, resumer(code));
  await settle();
  const b = fixture({stored});
  const theirs = maker();
  b.run.offerSetup(theirs.make, resumer(code));
  await settle();
  assert.equal(theirs.made.length, 0, 'the second tab makes no competing code');
  assert.equal(b.last('offer')[2].text, setupPrompt(CODE), 'it shows the first tab’s prompt');
  await a.run.copy();
  await settle();
  b.run.sync();
  await settle();
  assert.deepEqual(b.last('waiting')[2].text, setupPrompt(CODE), 'copied in one tab, the other follows the same code');
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await b.tick();
  assert.equal(b.last('steps')[1][1].text, 'Claude Code logged in as you');
  a.run.back();
  b.run.sync();
  assert.equal(b.names().includes('start'), true, 'back in one is back in both');
  stored.set(SETUP, JSON.stringify({step: 'done'}));
  b.run.sync();
  assert.ok(b.run.closed);
  assert.equal(b.state.reloads, 1, 'done in another tab: this one loads where the page opens now');
});

test('reloaded on the start, the kept prompt shows and copies before the page can follow codes, then is followed', async () => {
  const stored = new Map();
  const code = fakeCode();
  stored.set(SETUP, JSON.stringify({step: 'start', offer: code.saved}));
  const {run, last, names, copied, kept, tick} = fixture({stored});
  assert.deepEqual(last('offer')[2], {text: setupPrompt(CODE), code: CODE}, 'the same prompt, at once');
  assert.equal(names().includes('net'), false);
  await run.copy();
  await settle();
  assert.deepEqual(copied, [setupPrompt(CODE)], 'a click before the page loaded copies it');
  assert.equal(kept().code.code, CODE);
  await tick();
  assert.equal(last('net')?.[1] ?? null, null, 'waiting for the page is no failure of the host');
  run.offerSetup(maker(fakeCode({code: NEXT})).make, resumer(code));
  await settle();
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await tick();
  assert.equal(last('steps')[1][1].text, 'Claude Code logged in as you', 'followed once the page can');
});

test('reloaded on the start, the kept prompt stays when the page can follow codes again: no new code is made', async () => {
  const stored = new Map();
  const code = fakeCode();
  stored.set(SETUP, JSON.stringify({step: 'start', offer: code.saved}));
  const {run, last} = fixture({stored});
  const made = maker(fakeCode({code: NEXT}));
  run.offerSetup(made.make, resumer(code));
  await settle();
  assert.equal(made.made.length, 0);
  assert.equal(last('offer')[2].code, CODE);
});

test('the prompt breaks only between its words: a flag and a URL each stay whole', () => {
  const node = (kind, text = '') => ({kind, className: '', textContent: text});
  const document = {createTextNode: text => node('text', text), createElement: () => node('span')};
  const element = {ownerDocument: document, children: [], replaceChildren(...children) { this.children = children; },
    get textContent() { return this.children.map(child => child.textContent).join(''); }};
  const prompt = 'Set up Tilo for me: run curl -fsSL https://tilo.example | sh && tilo login D9B-X4GK --site https://tilo.example, then do what it tells you.';
  showPrompt(element, prompt);
  assert.equal(element.textContent, prompt, 'what is copied from the box is the prompt itself');
  const words = element.children.filter(child => child.className === 'fr-word').map(child => child.textContent);
  for (const whole of ['-fsSL', 'https://tilo.example', '--site', 'D9B-X4GK']) assert.ok(words.includes(whole), `${whole} stays one unbreakable word`);
  assert.ok(element.children.filter(child => child.kind === 'text').every(child => /^ +$/.test(child.textContent)), 'it breaks only at spaces');
});
