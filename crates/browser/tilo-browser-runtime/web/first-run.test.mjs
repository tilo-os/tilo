import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import {codeWait, CONNECTED_MS, createFirstRun, FOLLOW_MS, MAKE_MS, manualCommands, MCP_URL, OFFER_MS, PASSKEY_WAIT_MS, RENEW_MS,
  SEEN, setupFailure, setupPrompt, signInPrompt, stepLines, minutesLeft} from './first-run.mjs';

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
  const made = {code, expiresAt, polled: {resolved: false, expired: false, steps: []}, asked: 0, signedIn: []};
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

function fixture({copy, site} = {}) {
  const {view, said, names, last} = fakeView();
  const stored = new Map();
  const copied = [];
  const state = {clock: 0, ticks: null, every: null, timers: new Map(), id: 0, reloads: 0};
  const run = createFirstRun({view, site,
    // The clipboard takes the text at once, as `writeText` does inside the click.
    clipboard: {copy: copy ?? (text => { copied.push(text); return Promise.resolve(); })},
    storage: {setItem: (key, value) => stored.set(key, value)},
    now: () => state.clock, every: (tick, ms) => { state.ticks = tick; state.every = ms; return () => { state.ticks = null; }; },
    later: (run, ms) => { const id = ++state.id; state.timers.set(id, {run, ms}); return id; },
    clear: id => state.timers.delete(id),
    reload: () => { state.reloads += 1; }});
  // One follow round: the tick, and the host's answer.
  const tick = async () => { state.ticks?.(); await settle(); };
  // Run the timers set for `ms`.
  const fire = async ms => {
    for (const [id, timer] of [...state.timers]) if (timer.ms === ms) { state.timers.delete(id); timer.run(); }
    await settle();
  };
  return {run, said, names, last, stored, state, copied, tick, fire};
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
  assert.deepEqual(texts({resolved: true, agent, steps: ['resolved', 'logged-in', 'tools-added', 'restart', 'entered']}).slice(-2),
    ['✓ Claude Code restarted', '✓ Claude Code is in your workspace']);
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
  const leave = [...state.timers.values()].find(timer => timer.ms === CONNECTED_MS);
  leave.run();
  assert.equal(names().at(-1), 'close');
  assert.equal(opened, 1, 'the person’s own workspace opens');
  assert.equal(stored.get(SEEN), '1', 'the first screen is not shown again in this browser');
  assert.ok(run.closed);
  assert.equal([...state.timers.values()].some(timer => timer.ms !== CONNECTED_MS), false, 'no renewal outlives the screen');
});

test('a failure the computer reports is said, and the tab keeps following', async () => {
  const {run, last, state, tick} = fixture();
  const code = fakeCode();
  run.offerSetup(maker(code).make);
  await settle();
  await run.copy();
  Object.assign(code.polled, {resolved: true, steps: ['resolved', 'failed'], detail: 'Claude Code is not on this computer.'});
  await tick();
  assert.deepEqual(last('problem'), ['problem', 'Claude Code is not on this computer.']);
  assert.ok(state.ticks, 'still followed: the agent may try again');
});

test('returning: the agent claims the code, the tab asks to be that person, and opens their workspaces', async () => {
  const {run, last, names, copied, state, tick} = fixture();
  const code = fakeCode();
  run.offerAgentSignIn(maker(code).make);
  await settle();
  await run.signInWithAgent();
  assert.deepEqual(copied, [signInPrompt(CODE)]);
  assert.deepEqual(last('waiting'), ['waiting', 'sign-in', {text: signInPrompt(CODE), copied: true}]);
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
  [...state.timers.values()].find(timer => timer.ms === CONNECTED_MS).run();
  assert.equal(opened, 1);
  assert.equal(names().at(-1), 'close');
});

test('returning: a sign-in the page could not finish says why', async () => {
  const {run, last, tick} = fixture();
  const code = fakeCode();
  code.signIn = async () => { throw Object.assign(new Error('no'), {code: 'not-let-in'}); };
  run.offerAgentSignIn(maker(code).make);
  await settle();
  await run.signInWithAgent();
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
  assert.equal(names().at(-1), 'close');
  assert.equal(stored.get(SEEN), '1');
  const never = fixture();
  const waiting = never.run.passkey();
  await never.fire(PASSKEY_WAIT_MS);
  await waiting;
  assert.deepEqual(never.names().slice(-2), ['signInFailed', 'signingIn'], 'the wait ends, said');
});

/** first-run.inline.js against a page at `href`, with `stored` keys and the
 * scrubber's `secrets`: whether the first screen shows. */
function shows(href, {stored = {}, secrets = {}, storage = true} = {}) {
  const url = new URL(href);
  const attributes = new Map();
  const page = {
    document: {documentElement: {setAttribute: (name, value) => attributes.set(name, value)}, addEventListener() {}},
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
  for (const id of ['first-run', 'first-run-agent', 'first-run-offer', 'first-run-offer-note', 'first-run-offer-retry', 'first-run-sign-in-note', 'first-run-skip', 'first-run-passkey', 'first-run-agent-sign-in', 'first-run-manual-toggle', 'first-run-mcp', 'first-run-commands', 'first-run-steps', 'first-run-prompt', 'first-run-status']) {
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
