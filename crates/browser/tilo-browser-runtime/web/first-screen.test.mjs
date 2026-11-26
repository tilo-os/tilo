import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import {ASK_MS, domView, setupLater, showPrompt, CONNECTED_MS, KEPT_LOST_MS, QUIET_MS, createFirstScreen, FOLLOW_MS, MAKE_MS,
  manualCommands, MCP_URL, OFFER_MS, openFirstScreen, openSignIn, RENEW_MS, readSetup, readShown, SETUP, setupFailure, setupPrompt, SHOWN_FOLLOW_MS,
  signInPrompt, startFirstScreen, stepLines, minutesLeft} from './first-screen.mjs';

const here = name => new URL(name, import.meta.url);
const CODE = 'K7Q-MX2P';
const NEXT = 'R3T-WX9Z';
/** A ticket as a page's host serves one: `tilo1` and postcard bytes in hex. */
const TICKET = `tilo1${'ab'.repeat(80)}`;

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

function fixture({copy, site, stored = new Map(), online = () => true, clock = 0, held = false, signInOnly = false, onBack} = {}) {
  const {view, said, names, last} = fakeView();
  const copied = [];
  const state = {clock, ticks: null, every: null, timers: new Map(), id: 0, reloads: 0};
  const screen = createFirstScreen({view, site, held, signInOnly, onBack,
    // The clipboard takes the text at once, as `writeText` does inside the click.
    clipboard: {copy: copy ?? (text => { copied.push(text); return Promise.resolve(); })},
    storage: signInOnly ? null : {setItem: (key, value) => stored.set(key, value), getItem: key => stored.get(key) ?? null},
    now: () => state.clock, every: (tick, ms) => { state.ticks = tick; state.every = ms; return () => { state.ticks = null; }; },
    later: (callback, ms) => { const id = ++state.id; state.timers.set(id, {run: callback, ms}); return id; },
    clear: id => state.timers.delete(id),
    reload: () => { state.reloads += 1; }, online: () => online()});
  // One follow round: the tick, and the host's answer.
  const tick = async () => { state.ticks?.(); await settle(); };
  // Fire the timers set for `ms`.
  const fire = async ms => {
    for (const [id, timer] of [...state.timers]) if (timer.ms === ms) { state.timers.delete(id); timer.run(); }
    await settle();
  };
  const kept = () => readSetup({getItem: key => stored.get(key) ?? null});
  return {screen, said, names, last, stored, state, copied, tick, fire, kept};
}
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

test('the setup prompt is one line: install, log in with the ticket by the installed path, follow what tilo says', () => {
  // The installer starts `login` with the `tilo` it just put in place, by its full path: an agent's
  // shell on a fresh machine has no ~/.local/bin on its PATH (dist/install/tilo.sh).
  assert.equal(setupPrompt(TICKET),
    `Set up Tilo for me: run curl -fsSL tilo.sh | sh -s login ${TICKET}, then do what it tells you.`);
  assert.equal(setupPrompt(TICKET, {site: 'https://tilo.sh'}), setupPrompt(TICKET), 'tilo.sh is the default site');
  // A ticket names the host it is served from, so no site is said, even on a site of another name.
  assert.equal(setupPrompt(TICKET, {site: 'https://dev.tilo.example'}),
    `Set up Tilo for me: run curl -fsSL https://dev.tilo.example | sh -s login ${TICKET}, then do what it tells you.`);
  assert.equal(signInPrompt(TICKET), `Sign me in to Tilo: run tilo device add ${TICKET}, then do what it tells you.`);
  assert.equal(manualCommands(TICKET), `curl -fsSL tilo.sh | sh -s login ${TICKET}`);
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

test('a first visit makes no setup code, and so no workspace, until the person asks: the press says it is getting one', async () => {
  const {screen, last, copied, names} = fixture();
  const codes = maker(fakeCode());
  assert.deepEqual(last('offer'), ['offer', 'setup', {asking: false}], 'nothing is being got before the page can make one');
  screen.offerSetup(codes.make, null, {ahead: false});
  await settle();
  assert.deepEqual(codes.made, [], 'nothing is made while the person is still deciding');
  assert.deepEqual(last('offer'), ['offer', 'setup', {asking: false}], 'the button offers to make one, and the screen says nothing it is not doing');
  await screen.copy();
  await settle();
  assert.equal(codes.made.length, 1, 'the press makes the code');
  assert.deepEqual(last('offer'), ['offer', 'setup', {text: setupPrompt(CODE), code: CODE}], 'and the prompt is on the screen');
  assert.deepEqual(copied, [], 'the press that made it copies nothing: the next one does');
  assert.deepEqual(names().includes('waiting'), false);
  await screen.copy();
  assert.deepEqual(copied, [setupPrompt(CODE)]);
});

test('the prompt is on the screen before any click: a screen reopened from the card makes its code ahead', async () => {
  const {screen, last, copied} = fixture();
  assert.deepEqual(last('offer'), ['offer', 'setup', {asking: false}], 'until then nothing is being got');
  screen.offerSetup(maker(fakeCode()).make);
  await settle();
  assert.deepEqual(last('offer'), ['offer', 'setup', {text: setupPrompt(CODE), code: CODE}]);
  assert.deepEqual(copied, [], 'nothing is copied before the person asks');
});

test('a person who asks before the page can make a code gets it once the page can', async () => {
  const {screen, last, copied} = fixture();
  await screen.copy();
  await settle();
  assert.deepEqual(copied, [], 'nothing to copy yet');
  const codes = maker(fakeCode());
  screen.offerSetup(codes.make, null, {ahead: false});
  await settle();
  assert.equal(codes.made.length, 1, 'the press asked for it, so it is made as soon as the page can');
  assert.deepEqual(last('offer'), ['offer', 'setup', {text: setupPrompt(CODE), code: CODE}]);
});

test('the click copies exactly the prompt shown, inside the click, and only then says it copied', async () => {
  const {screen, last, names, copied, state} = fixture();
  const codes = maker(fakeCode(), fakeCode({code: NEXT}));
  screen.offerSetup(codes.make);
  await settle();
  const [, , {text: shown}] = last('offer');
  const done = screen.copy();
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

test('the code is followed before the clipboard holds it: the host knows it when an agent asks', async () => {
  // The host keeps a code only from its tab's first follow (tilo-server's login
  // codes), and the prompt is on the clipboard the moment the click returns: an
  // agent handed it at once asks for a code the host must already have. The
  // shown code is followed as soon as it is on the screen; the click follows it
  // again, for a look that had not landed yet.
  const askedAtCopy = [];
  let codes = null;
  const {screen, copied} = fixture({copy: text => { askedAtCopy.push(codes.made[0].asked); copied.push(text); return Promise.resolve(); }});
  codes = maker(fakeCode());
  screen.offerSetup(codes.make);
  await settle();
  assert.ok(codes.made[0].asked >= 1, 'the host was asked where the shown code is, before any click');
  const before = codes.made[0].asked;
  await screen.copy();
  assert.deepEqual(askedAtCopy.length, 1);
  assert.ok(askedAtCopy[0] > before, 'the click asked again before the clipboard had the prompt');
  assert.deepEqual(copied, [setupPrompt(CODE)], 'the prompt went to the clipboard');
});

test('finishing a clipboard write after Skip leaves the setup closed to that ticket', async () => {
  const receipt = Promise.withResolvers();
  const {screen, names, kept} = fixture({copy: () => receipt.promise});
  screen.offerSetup(maker(fakeCode()).make);
  await settle();
  const copying = screen.copy();
  screen.skip();
  receipt.resolve();
  await copying;
  assert.ok(!names().includes('waiting'));
  assert.equal(kept().step, 'opening');
});

test('a code shown and never copied is kept at once, and the steps say what the agent is doing without a copy', async () => {
  // A person who selects the prompt and copies it by hand, or types the code
  // their agent asks about, hands it over without the button: the host knows
  // the code because the screen followed it, and the screen moves on to the
  // steps. Nothing was copied, and the agent already has the code, so the
  // screen says what it is doing instead of asking for a paste.
  const {screen, last, names, fire, copied} = fixture();
  const code = fakeCode();
  screen.offerSetup(maker(code).make);
  await settle();
  assert.ok(code.asked >= 1, 'kept as soon as it is on the screen');
  assert.deepEqual(copied, [], 'nothing is copied before the person asks');
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await fire(SHOWN_FOLLOW_MS);
  assert.deepEqual(last('waiting'), ['waiting', 'setup', {text: setupPrompt(CODE), copied: false, handed: true, agent: 'Claude Code'}],
    'the agent got the code without a copy here, and the screen says which agent');
  assert.equal(names().includes('problem'), false, 'nothing says a copy failed');
  assert.deepEqual(last('steps')[1].map(({text}) => text).slice(0, 2), ['Tilo is installed', 'Claude Code logged in as you']);
});

test('a code handed over without a copy is kept that way after a reload', async () => {
  const stored = new Map();
  const code = fakeCode({code: NEXT});
  stored.set(SETUP, JSON.stringify({step: 'waiting', mode: 'setup', copied: false, handed: true,
    code: {...code.saved, kind: 'setup'},
    polled: {resolved: true, steps: ['resolved', 'logged-in'], agent: 'Claude Code', detail: null}}));
  const again = fixture({stored});
  assert.deepEqual(again.last('waiting'), ['waiting', 'setup', {text: setupPrompt(NEXT), copied: false, manual: false,
    handed: true, agent: 'Claude Code'}], 'the reloaded screen says what the agent is doing, never that a copy was made');
});

test('the sign-in prompt is kept while it waits on the screen, so a claim signs the tab in without a click', async () => {
  const {screen, last, fire, copied} = fixture();
  const code = fakeCode();
  screen.offerAgentSignIn(maker(code).make);
  await settle();
  assert.ok(code.asked >= 1, 'kept while it waits on the screen');
  assert.deepEqual(copied, []);
  Object.assign(code.polled, {resolved: true, steps: ['resolved'], agent: 'Claude Code',
    claimed: {user: 'ab'.repeat(32), invite: 'cd'.repeat(16), by: 'Claude Code'}});
  await fire(SHOWN_FOLLOW_MS);
  await settle();
  assert.deepEqual(last('waiting'), ['waiting', 'sign-in', {text: signInPrompt(CODE), copied: false, handed: true, agent: 'Claude Code'}]);
  assert.equal(code.signedIn.length, 1, 'certified with that person and invite, as a copied prompt would be');
  assert.deepEqual(last('connected')[1].heading, 'You’re signed in');
});

test('a prompt kept from before a reload that this page cannot follow is said after a while, with a reload', async () => {
  // The setup keeps what it showed, and a reload shows that prompt before the
  // page can follow codes again. When it never can (its runtime did not load,
  // or the code is not this tab's to follow any more), looking again every
  // couple of seconds says nothing for ever: the prompt's box says so instead,
  // and its button reloads the page, which makes a fresh code.
  const keep = fakeCode({code: NEXT});
  const stored = new Map([[SETUP, JSON.stringify({step: 'start', offer: keep.saved, at: 0})]]);
  const {screen, last, state, fire} = fixture({stored});
  await settle();
  assert.deepEqual(last('offer'), ['offer', 'setup', {text: setupPrompt(NEXT), code: NEXT}], 'the kept prompt is shown');
  // A few rounds of looking, without a word.
  for (let round = 0; round < 2; round += 1) { state.clock += SHOWN_FOLLOW_MS; await fire(SHOWN_FOLLOW_MS); }
  assert.deepEqual(last('offer'), ['offer', 'setup', {text: setupPrompt(NEXT), code: NEXT}],
    'looked at again without a word while it may still be followed');
  for (let round = 0; round <= KEPT_LOST_MS / SHOWN_FOLLOW_MS; round += 1) { state.clock += SHOWN_FOLLOW_MS; await fire(SHOWN_FOLLOW_MS); }
  assert.deepEqual(last('offer'), ['offer', 'setup', {problem: 'Tilo didn’t finish loading in this tab.', reload: true}],
    'said at the prompt, with the reload that makes a fresh code');
  await screen.retry('setup');
  assert.equal(state.reloads, 1, 'the button reloads, which makes a fresh code');
});

test('a click before the page has a code copies nothing and says what it waits for', async () => {
  const {screen, last, names, copied} = fixture({site: 'https://tilo.sh'});
  assert.equal(await screen.copy(), false);
  assert.deepEqual(copied, []);
  assert.deepEqual(last('notReady'), ['notReady', 'setup', 'Getting your code from tilo.sh…']);
  assert.equal(names().includes('waiting'), false, 'the button stays where it is');
  await screen.signInWithAgent();
  await settle();
  assert.deepEqual(last('notReady'), ['notReady', 'signIn', 'Getting your code from tilo.sh…']);
});

test('a code the host never makes ends in a stated reason with a retry, not a hang', async () => {
  const {screen, last, fire, copied} = fixture({site: 'https://tilo.sh'});
  let asked = 0;
  screen.offerSetup(() => { asked += 1; return new Promise(() => {}); });
  await settle();
  assert.deepEqual(last('offer'), ['offer', 'setup', {note: 'Getting your code from tilo.sh…'}]);
  await fire(MAKE_MS);
  assert.deepEqual(last('offer'), ['offer', 'setup', {problem: 'Couldn’t reach tilo.sh. Check your connection, then try again.'}]);
  assert.equal(await screen.copy(), false, 'a click copies nothing');
  assert.deepEqual(copied, []);
  assert.equal(asked, 2, 'and asks the host again');
  await screen.retry();
  assert.equal(asked, 2, 'one ask at a time');
  await fire(MAKE_MS);
  void screen.retry();
  await settle();
  assert.equal(asked, 3, 'retry asks again');
});

test('a host that refuses, or a page that never offers codes, is said, not waited on', async () => {
  const refused = fixture();
  refused.screen.offerSetup(async () => { throw Object.assign(new Error('refused'), {code: 'refused'}); });
  await settle();
  await settle();
  assert.deepEqual(refused.last('offer'), ['offer', 'setup', {problem: setupFailure({code: 'refused'})}]);
  const silent = fixture();
  await silent.fire(OFFER_MS);
  const offered = kind => silent.said.findLast(([name, of]) => name === 'offer' && of === kind);
  // Said once, in the setup's box, whose button reloads the page; the
  // sign-in beside it says nothing more about the same load.
  assert.deepEqual(offered('setup'), ['offer', 'setup', {problem: setupFailure({code: 'not-loaded'}), reload: true}]);
  assert.deepEqual(offered('signIn'), ['offer', 'signIn', {}]);
  await silent.screen.retry();
  assert.equal(silent.state.reloads, 1, 'trying again loads the page again');
  const gone = fixture();
  gone.screen.unavailable({code: 'refused'});
  assert.deepEqual(gone.last('offer'), ['offer', 'signIn', {problem: setupFailure({code: 'refused'})}]);
  await gone.screen.signInWithAgent();
  assert.deepEqual(gone.last('notReady'), ['notReady', 'signIn', setupFailure({code: 'refused'})]);
  assert.equal(setupFailure({code: 'not-let-in'}), 'This tab isn’t let in yet. Try again in a moment.');
});

test('a load that finishes late is not said to have failed: the offered codes take the sentence back', async () => {
  // The wait says a page did not load only when its codes are missing: a
  // screen still held for the entry's answer is not shown yet, so the wait
  // has nothing to say of it, and a screen whose page does offer its codes
  // (after a slow load) stops saying it did not.
  const held = fixture({held: true});
  await held.fire(OFFER_MS);
  assert.deepEqual(held.last('offer'), ['offer', 'setup', {asking: false}],
    'a screen still held for the entry is not said to have failed to load');
  held.screen.reveal();
  await settle();
  assert.equal(held.last('offer')[2].problem, undefined, 'shown, it says nothing of a failure');
  held.screen.offerSetup(maker(fakeCode()).make, null, {ahead: false});
  await settle();
  assert.deepEqual(held.last('offer'), ['offer', 'setup', {asking: false}],
    'the loaded page offers its setup, never a load failure');

  const late = fixture();
  await late.fire(OFFER_MS);
  const offered = kind => late.said.findLast(([name, of]) => name === 'offer' && of === kind);
  assert.deepEqual(offered('setup'), ['offer', 'setup', {problem: setupFailure({code: 'not-loaded'}), reload: true}],
    'a page that really offered nothing says so');
  late.screen.offerSetup(maker(fakeCode()).make);
  await settle();
  assert.deepEqual(offered('setup'), ['offer', 'setup', {text: setupPrompt(CODE), code: CODE}],
    'the code that arrives takes the failure back');
  assert.equal(late.screen.held, false);
});

test('the screen offers one Skip for now, whichever step it is at', () => {
  const html = readFileSync(here('index.html'), 'utf8');
  assert.equal(html.match(/>Skip for now</g)?.length, 1, 'one button named Skip for now');
  assert.equal(html.match(/id="first-screen-skip"/g)?.length, 1);
});

test('a server that refuses the code says the code could not be made, never that the workspace could not be opened', async () => {
  // What a page that came by its server's second name got from it: the workspace is there, the code is refused.
  const {screen, last} = fixture({site: 'https://tilo.wifi.example:24520'});
  const refusal = Object.assign(new Error('the code route answered 400'),
    {name: 'TicketError', code: 'refused', status: 400, origin: 'https://tilo.wifi.example:24500'});
  screen.offerSetup(async () => { throw refusal; });
  await settle();
  await settle();
  const [, , {problem}] = last('offer');
  const rest = ' Try again, or skip for now and use Tilo without an agent.';
  assert.equal(problem, `The code for your agent could not be made here: tilo.wifi.example:24520 refused it.${rest}`);
  // The site is named as its address bar names it, not by the address its codes come from, and never
  // by an address by number.
  const numbered = Object.assign(new Error('the code route answered 400'), {name: 'TicketError', code: 'refused', status: 400, origin: 'http://127.0.0.1:24500'});
  assert.equal(setupFailure(numbered, {site: 'http://tilo.localhost:24520'}),
    `The code for your agent could not be made here: tilo.localhost:24520 refused it.${rest}`);
  assert.equal(setupFailure(numbered, {site: 'http://127.0.0.1:24520'}), `The code for your agent could not be made here: this site refused it.${rest}`);
  assert.equal(setupFailure(numbered), `The code for your agent could not be made here: tilo.sh refused it.${rest}`);
  // Skip for now still opens the person's own workspace, as the sentence says it does.
  const opened = [];
  screen.whenDone(how => opened.push(how));
  screen.skip();
  await settle();
  assert.deepEqual(opened, [{signedIn: false}], 'the person’s own workspace opens, as the tab’s own');
  // Only the page's own stop says a workspace could not be opened and promises a why (the page shows it
  // beneath this screen): no other failure does.
  for (const code of ['refused', 'no-workspace', 'no-codes', 'not-let-in', 'not-loaded', 'copy-failed', 'not-a-device', 'unknown',
    'too-many', 'timeout', 'unreachable', undefined]) {
    assert.doesNotMatch(setupFailure({code}), /to see why|could not open a workspace/, `the failure ${code}`);
  }
  assert.equal(setupFailure({code: 'stopped'}), 'Tilo could not open a workspace for you here yet. Skip for now to see why.');
  assert.equal(setupFailure({code: 'refused'}), `The code for your agent could not be made here.${rest}`, 'refused by something other than the site’s server');
  assert.equal(setupFailure({code: 'no-workspace'}), 'There is no workspace here yet to set your agent up in. Try again, or skip for now.');
  assert.equal(setupFailure({code: 'no-codes'}), 'This tab can’t make the code an agent needs. Try again, or skip for now.');
});

test('a page that stopped promises a why, and Skip for now leaves the screen for it at once', async () => {
  const {screen, said, names, last, kept} = fixture();
  screen.unavailable({code: 'stopped'});
  assert.deepEqual(said.findLast(([name, of]) => name === 'offer' && of === 'setup'),
    ['offer', 'setup', {problem: 'Tilo could not open a workspace for you here yet. Skip for now to see why.'}]);
  let opened = 0;
  screen.whenDone(() => { opened += 1; });
  screen.skip();
  await settle();
  assert.ok(names().includes('close'), 'the screen leaves: the page says why beneath it');
  assert.equal(last('opening'), undefined, 'and never says a workspace is opening');
  assert.equal(opened, 0, 'nothing is opened for a page that cannot go on');
  assert.notEqual(kept()?.step, 'opening', 'nor kept as opening for the next load');
});

test('a clipboard that refuses claims no copy: the prompt stays, to copy by hand', async () => {
  const {screen, last, names} = fixture({copy: async () => { throw new Error('denied'); }});
  screen.offerSetup(maker(fakeCode()).make);
  await settle();
  assert.equal(await screen.copy(), false);
  assert.deepEqual(last('waiting'), ['waiting', 'setup', {text: setupPrompt(CODE), copied: false}]);
  assert.deepEqual(last('problem'), ['problem', setupFailure({code: 'copy-failed'})]);
  assert.equal(names().includes('copied'), false, 'nothing says it copied');
  const thrown = fixture({copy: () => { throw new Error('no clipboard'); }});
  thrown.screen.offerSetup(maker(fakeCode()).make);
  await settle();
  assert.equal(await thrown.screen.copy(), false);
  assert.deepEqual(thrown.last('waiting')[2], {text: setupPrompt(CODE), copied: false});
  await thrown.screen.again('text');
  assert.deepEqual(thrown.last('copied'), ['copied', false, 'prompt']);
});

test('a prompt copied by hand is followed like one copied with the button', async () => {
  const {screen, last, copied} = fixture();
  screen.copiedByHand();
  assert.equal(last('waiting'), undefined, 'nothing shown, nothing copied');
  screen.offerSetup(maker(fakeCode()).make);
  await settle();
  screen.copiedByHand();
  assert.deepEqual(last('waiting'), ['waiting', 'setup', {text: setupPrompt(CODE), copied: true}]);
  assert.deepEqual(copied, []);
});

test('a ready code is renewed shortly before it expires, the old one shown until then', async () => {
  const {screen, last, fire, names} = fixture();
  const codes = maker(fakeCode(), fakeCode({code: NEXT, expiresAt: 1_200_000}));
  screen.offerSetup(codes.make);
  await settle();
  const offers = names().filter(name => name === 'offer').length;
  await fire(600_000 - RENEW_MS);
  assert.equal(codes.made.length, 2);
  assert.equal(names().filter(name => name === 'offer').length, offers + 1, 'no wait shown in between');
  assert.deepEqual(last('offer'), ['offer', 'setup', {text: setupPrompt(NEXT), code: NEXT}]);
  assert.ok(codes.made[1].asked >= 1, 'the renewed prompt is kept as soon as it shows');
});

test('a copied code that expires says so, and the retry copies the fresh one at once', async () => {
  const {screen, names, last, state, tick, copied} = fixture();
  const codes = maker(fakeCode(), fakeCode({code: NEXT, expiresAt: 1_200_000}));
  screen.offerSetup(codes.make);
  await settle();
  await screen.copy();
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
  assert.ok(codes.made[1].asked >= 1, 'the fresh prompt is kept as soon as it shows');
  await screen.retry('setup');
  assert.deepEqual(copied, [setupPrompt(CODE), setupPrompt(NEXT)]);
  assert.deepEqual(last('waiting')[2], {text: setupPrompt(NEXT), copied: true});
});

test('the host saying the code expired shows it expired too', async () => {
  const {screen, names, state, tick} = fixture();
  const code = fakeCode();
  screen.offerSetup(maker(code).make);
  await settle();
  await screen.copy();
  await settle();
  code.polled.expired = true;
  await tick();
  assert.deepEqual(names().filter(name => ['expired', 'steps', 'left', 'connected'].includes(name)).at(-1), 'expired');
  assert.equal(state.ticks, null);
});

test('each step the agent reports shows live, then the screen leaves for the workspace, remembered', async () => {
  const {screen, names, last, stored, state, tick} = fixture();
  const code = fakeCode();
  screen.offerSetup(maker(code).make);
  await settle();
  let opened = 0;
  screen.whenDone(() => { opened += 1; });
  await screen.copy();
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
  assert.ok(screen.closed);
  assert.equal([...state.timers.values()].some(timer => timer.ms !== CONNECTED_MS), false, 'no renewal outlives the screen');
});

test('a failure the computer reports is said, and the tab keeps following', async () => {
  const {screen, last, state, tick} = fixture();
  const code = fakeCode();
  screen.offerSetup(maker(code).make);
  await settle();
  await screen.copy();
  await settle();
  Object.assign(code.polled, {resolved: true, steps: ['resolved', 'failed'], detail: 'Claude Code is not on this computer.'});
  await tick();
  assert.deepEqual(last('agentFailed'), ['agentFailed', 'Claude Code is not on this computer.'], 'the agent’s reason');
  assert.deepEqual(last('steps')[1].at(-1), {text: 'Your agent could not log in as you', failed: true}, 'the step it stopped at, not under way');
  assert.ok(state.ticks, 'still followed: the agent may try again');
});

test('returning: the agent claims the code, the tab asks to be that person, and opens their workspaces', async () => {
  const {screen, last, names, copied, state, tick} = fixture();
  const code = fakeCode();
  screen.offerAgentSignIn(maker(code).make);
  await settle();
  await screen.signInWithAgent();
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
  const opened = [];
  screen.whenDone(how => opened.push(how));
  [...state.timers.values()].find(timer => timer.ms === CONNECTED_MS).run();
  assert.deepEqual(opened, [{signedIn: true}], 'the page is told a sign-in did it: another person’s workspace opens, never this tab’s own');
  assert.equal(names().at(-1), 'close');
});

test('returning: signed in before the page can open their workspace, the tab opens it once it can', async () => {
  const {screen, last, names, state, tick} = fixture();
  const code = fakeCode();
  screen.offerAgentSignIn(maker(code).make);
  await settle();
  await screen.signInWithAgent();
  await settle();
  Object.assign(code.polled, {resolved: true, steps: ['resolved'], claimed: {user: 'ab'.repeat(32), invite: 'cd'.repeat(16), by: 'Claude Code'}});
  await tick();
  await settle();
  assert.deepEqual(last('connected'), ['connected', {mode: 'sign-in', agent: null, heading: 'You’re signed in'}]);
  // The moment it said so is over before the page can open anything.
  [...state.timers.values()].find(timer => timer.ms === CONNECTED_MS).run();
  assert.notEqual(names().at(-1), 'close', 'the screen waits for what it opens, rather than leaving the tab where it stands');
  const opened = [];
  screen.whenDone(how => opened.push(how));
  assert.deepEqual(opened, [{signedIn: true}], 'the page is told a sign-in opened it, once the page can');
  assert.equal(names().at(-1), 'close');
  screen.whenDone(how => opened.push(how));
  assert.equal(opened.length, 1, 'and only once');
});

test('returning: a sign-in the page could not finish says why', async () => {
  const {screen, last, tick} = fixture();
  const code = fakeCode();
  code.signIn = async () => { throw Object.assign(new Error('no'), {code: 'not-let-in'}); };
  screen.offerAgentSignIn(maker(code).make);
  await settle();
  await screen.signInWithAgent();
  await settle();
  Object.assign(code.polled, {resolved: true, claimed: {user: 'ab', invite: 'cd', by: 'x'}});
  await tick();
  await settle();
  assert.deepEqual(last('failed'), ['failed', 'This tab isn’t let in yet. Try again in a moment.', 'sign-in']);
});

test('returning: a sign-in the agent gave up on says its reason alone, and stops following', async () => {
  const {screen, last, names, tick} = fixture();
  const code = fakeCode();
  screen.offerAgentSignIn(maker(code).make);
  await settle();
  await screen.signInWithAgent();
  await settle();
  const reason = 'This computer can’t sign the tab in: it doesn’t hold your Tilo identity.';
  Object.assign(code.polled, {resolved: true, steps: ['resolved', 'failed'], agent: 'Claude Code', detail: reason});
  await tick();
  await settle();
  assert.deepEqual(last('failed'), ['failed', reason, 'sign-in']);
  assert.deepEqual(stepLines(code.polled, 'sign-in'), [{text: 'Claude Code could not sign this tab in', failed: true}],
    'nothing says it is still finding you');
  const said = names().length;
  await tick();
  await settle();
  assert.equal(names().length, said, 'the screen follows the code no longer');
});

test('signing in from the wait list: the prompt shows at once, a claim signs the tab in, nothing is kept', async () => {
  let back = 0;
  const {screen, last, names, stored, tick} = fixture({signInOnly: true, onBack: () => { back += 1; }});
  const code = fakeCode();
  screen.offerAgentSignIn(maker(code).make);
  await settle();
  await settle();
  // No click copied it: the screen shows the prompt to copy or select, and
  // says nothing failed.
  assert.deepEqual(last('waiting'), ['waiting', 'sign-in', {text: signInPrompt(CODE), copied: false}]);
  assert.ok(!names().includes('problem') || last('problem')[1] === '', 'no copy was asked for, so none failed');
  assert.ok(code.asked > 0, 'followed from the moment it shows');
  const claimed = {user: 'ab'.repeat(32), invite: 'cd'.repeat(16), by: 'Claude Code on build-box'};
  Object.assign(code.polled, {resolved: true, steps: ['resolved'], claimed});
  await tick();
  await settle();
  assert.deepEqual(code.signedIn, [claimed], 'the tab asks to be that person, with the invite the claim named');
  assert.equal(stored.size, 0, 'a browser on the wait list keeps no setup');
  assert.equal(back, 0);
});

test('signing in from the wait list: back closes the screen onto the wait list', async () => {
  let back = 0;
  const {screen, names} = fixture({signInOnly: true, onBack: () => { back += 1; }});
  screen.offerAgentSignIn(maker(fakeCode()).make);
  await settle();
  await settle();
  screen.back();
  assert.equal(names().at(-1), 'close');
  assert.equal(back, 1);
});

test('set up manually shows the MCP server and the commands with the code shown, and follows it', async () => {
  const {screen, last, names, copied, tick} = fixture();
  const code = fakeCode();
  screen.offerSetup(maker(code).make);
  await settle();
  screen.manual();
  assert.deepEqual(last('manual'), ['manual', true]);
  assert.deepEqual(last('commands'), ['commands', manualCommands(CODE), MCP_URL]);
  assert.deepEqual(copied, [], 'nothing is copied until asked');
  await screen.again(MCP_URL, 'mcp');
  assert.deepEqual(copied, [MCP_URL]);
  assert.deepEqual(last('copied'), ['copied', true, 'mcp']);
  Object.assign(code.polled, {resolved: true, steps: ['resolved', 'logged-in']});
  await tick();
  assert.deepEqual(last('waiting'), ['waiting', 'setup', {manual: true}], 'the steps show once a computer used the code');
  assert.equal(names().includes('steps'), true);
  // Closing the panel stops following its code.
  const other = fixture();
  other.screen.offerSetup(maker(fakeCode()).make);
  await settle();
  other.screen.manual();
  other.screen.manual();
  assert.deepEqual(other.last('manual'), ['manual', false]);
  assert.equal(other.state.ticks, null);
  // Without a code yet the panel says what it waits for.
  const early = fixture({site: 'https://tilo.sh'});
  early.screen.manual();
  assert.deepEqual(early.last('commands'), ['commands', null, MCP_URL, 'Getting your code from tilo.sh…']);
});

test('back returns to the start, with the next prompt shown, and forgets the code under way', async () => {
  const {screen, names, last, state} = fixture();
  screen.offerSetup(maker(fakeCode(), fakeCode({code: NEXT})).make);
  await settle();
  await screen.copy();
  await settle();
  screen.back();
  assert.deepEqual(names().slice(-2), ['start', 'offer']);
  assert.deepEqual(last('offer')[2].code, NEXT);
  assert.equal(state.ticks, null);
});

/** first-screen.inline.js against a page at `href`, with `stored` keys and the
 * scrubber's `secrets`: whether the first screen shows, and whether it was
 * held for the site's entry to answer (`data-first-screen-hold`). */
function shows(href, {stored = {}, secrets = {}, storage = true, step = false, held = false} = {}) {
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
  vm.runInNewContext(readFileSync(here('first-screen.inline.js'), 'utf8'), page);
  if (held) return attributes.has('data-first-screen-hold');
  if (step) return attributes.get('data-first-screen') ?? null;
  return attributes.has('data-first-screen');
}
const kept = state => ({[SETUP]: JSON.stringify(state)});

test('only a browser new here, opening the site itself, is offered the first screen', () => {
  // No screen and no setup kept: whether the setup is even the person's to
  // take up is the site's entry's to say, and a site taking people by
  // invitation only sends them to its wait list. The page shows it once it
  // is in, and the visitor the entry turned away never sees it.
  const fresh = {};
  assert.equal(shows('https://tilo.example/', {stored: fresh}), false);
  assert.equal(shows('https://tilo.example/#/'), false);
  assert.equal(shows('https://tilo.example/?debug'), false);
  assert.equal(shows('https://tilo.example/', {stored: fresh, held: true}), true, 'held for the entry’s answer');
  assert.equal(fresh[SETUP], undefined, 'and keeps no setup it has not started');
  assert.equal(shows('https://tilo.example/', {stored: kept({step: 'done'})}), false, 'set up already');
  assert.equal(shows('https://tilo.example/', {stored: kept({step: 'done'}), held: true}), false, 'nothing to hold');
  assert.equal(shows('https://tilo.example/', {stored: {'tilo-host-memberships': '{}'}}), false, 'a host let it in before');
  assert.equal(shows('https://tilo.example/', {stored: {'tilo-entered-workspaces': '[]'}}), false);
  assert.equal(shows('https://tilo.example/', {secrets: {invite: 'x'}}), false, 'an invitation opens as it did');
  assert.equal(shows('https://tilo.example/', {secrets: {invite: 'x'}, held: true}), false, 'and is no visitor to hold');
  assert.equal(shows('https://tilo.example/#/@workspaces.example/w/abc'), false, 'a link to a workspace');
  assert.equal(shows('https://tilo.example/?embed=1'), false, 'inside another site');
  assert.equal(shows('https://tilo.example/?workspaceName=Lounge'), false);
  assert.equal(shows('https://tilo.example/', {storage: false}), false, 'no storage: as it always opened');
});

test('a first visit the site may still turn away is held: the setup shows once the page is in', async () => {
  const first = fixture({held: true});
  assert.equal(first.screen.held, true, 'the page has not said whether the setup is theirs');
  assert.deepEqual(first.names(), ['offer'], 'nothing on the screen: the wait list may be what this visit is');
  first.screen.reveal();
  assert.equal(first.screen.held, false);
  assert.deepEqual(first.names().slice(-2), ['start', 'offer'], 'shown, on its start');
  assert.equal(first.kept()?.step, 'start', 'and kept from now on');
  // The same visit, shown at the first paint: nothing is redrawn under the person.
  const shown = fixture();
  shown.screen.offerSetup(maker(fakeCode()).make);
  await settle();
  const drawn = shown.names().length;
  assert.equal(shown.screen.held, false);
  shown.screen.reveal();
  assert.equal(shown.names().length, drawn, 'left where it was');
  // A page that turned the person away closes it, and it stays closed.
  const away = fixture({held: true});
  away.screen.close();
  away.screen.reveal();
  assert.equal(away.screen.held, false);
  assert.deepEqual(away.names(), ['offer', 'close'], 'nothing comes back over the wait list');
});

test('a newcomer by invitation joins first, and is set up later: on the site itself, never over a workspace', () => {
  const stored = {};
  assert.equal(shows('https://tilo.example/#/@workspaces.example/w/team', {stored, secrets: {invite: 'x'}}), false,
    'the invitation opens the workspace it names, nothing in the way');
  assert.equal(JSON.parse(stored[SETUP]).step, 'later', 'their own setup is kept for later');
  // Joining let the page in: it keeps the host and the workspace now.
  Object.assign(stored, {'tilo-host-memberships': '{"x":{}}', 'tilo-entered-workspaces': '[{}]'});
  assert.equal(shows('https://tilo.example/#/@workspaces.example/w/team', {stored}), false,
    'back at the workspace they were invited to: the workspace, not the setup');
  assert.equal(JSON.parse(stored[SETUP]).step, 'later');
  assert.equal(shows('https://tilo.example/', {stored, step: true}), null,
    'opening the site itself: the workspace they joined, never between them and their work');
  assert.equal(JSON.parse(stored[SETUP]).step, 'later', 'and is still for later, until they take it up');
  assert.equal(shows('https://tilo.example/#/@workspaces.example/w/team', {stored}), false,
    'so the workspace’s address still opens the workspace');
  // A browser that joined nothing (they put the invitation off, say) is
  // offered the setup on the site itself, as before.
  const unjoined = {[SETUP]: JSON.stringify({step: 'later', at: Date.now()})};
  assert.equal(shows('https://tilo.example/', {stored: unjoined, step: true}), '',
    'no workspace joined: the site itself offers the setup');
  assert.equal(setupLater({getItem: () => JSON.stringify({step: 'later'})}), true);
  assert.equal(setupLater({getItem: () => JSON.stringify({step: 'done'})}), false);
  // Someone already here who opens an invitation keeps what they had.
  const member = {'tilo-host-memberships': '{"x":{}}', ...kept({step: 'done'})};
  shows('https://tilo.example/#/@workspaces.example/w/team', {stored: member, secrets: {invite: 'x'}});
  assert.equal(JSON.parse(member[SETUP]).step, 'done');
  const known = {'tilo-host-memberships': '{"x":{}}'};
  shows('https://tilo.example/#/@workspaces.example/w/team', {stored: known, secrets: {invite: 'x'}});
  assert.equal(known[SETUP], undefined, 'a browser a host let in before is no newcomer');
  const paired = {};
  shows('https://tilo.example/#/@workspaces.example/w/team', {stored: paired, secrets: {pair: 'x'}});
  assert.equal(paired[SETUP], undefined, 'a device paired to a person has its person set up already');
});

test('the kept setup alone decides the step a bare entry opens on, whatever the page did behind the screen', () => {
  const fresh = {};
  assert.equal(shows('https://tilo.example/', {stored: fresh, step: true}), null,
    'a browser new here is shown nothing until its site’s entry says');
  assert.equal(shows('https://tilo.example/', {stored: fresh, held: true}), true, 'its screen waits for that answer');
  assert.equal(fresh[SETUP], undefined);
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
  assert.equal(shows('https://tilo.example/#/@workspaces.example/w/lounge', {stored: {...lounge, ...kept({step: 'waiting', code})}, step: true}), 'waiting',
    'the home workspace’s own address, written once it let the page in behind the screen, still shows the step');
  assert.equal(shows('https://tilo.example/#/@workspaces.example/w/abc?invite=x', {stored: kept({step: 'waiting', code}), secrets: {invite: 'x'}}), false,
    'an invitation still opens what it names');
  assert.equal(shows('https://tilo.example/?embed=1', {stored: kept({step: 'waiting', code})}), false, 'inside another site: never');
});

test('the desktop entry page inlines the first screen’s script unchanged, after the scrubber, and carries its screen', () => {
  const html = readFileSync(here('index.html'), 'utf8');
  const script = readFileSync(here('first-screen.inline.js'), 'utf8');
  const at = html.indexOf(`<script>\n${script}</script>`);
  assert.ok(at > 0, 'index.html must inline first-screen.inline.js unchanged');
  assert.ok(html.indexOf('__tiloLinkSecrets') < at, 'after the scrubber, whose secrets it reads');
  for (const id of ['first-screen', 'first-screen-agent', 'first-screen-offer', 'first-screen-offer-note', 'first-screen-offer-retry', 'first-screen-sign-in-note', 'first-screen-skip', 'first-screen-agent-sign-in', 'first-screen-manual-toggle', 'first-screen-mcp', 'first-screen-commands', 'first-screen-steps', 'first-screen-prompt', 'first-screen-status', 'first-screen-heading-wait', 'first-screen-heading-hand', 'first-screen-heading-hand-sign-in']) {
    assert.ok(html.includes(`id="${id}"`), `index.html has #${id}`);
  }
  assert.ok(html.includes('<script type="module" async src="./first-screen.mjs"></script>'));
  assert.ok(html.includes('prefers-color-scheme: light'), 'the screen follows the person’s light or dark');
});

test('skip for now opens what the page names, also when the person skipped before it could', async () => {
  const later = fixture();
  let opened = 0;
  later.screen.whenDone(() => { opened += 1; });
  later.screen.skip();
  await settle();
  assert.equal(opened, 1, 'the person’s own workspace opens');
  const early = fixture();
  early.screen.skip();
  assert.equal(readSetup({getItem: key => early.stored.get(key)}).step, 'opening', 'kept: a reload opens it as well');
  early.screen.whenDone(() => { opened += 1; });
  await settle();
  assert.equal(opened, 2, 'a skip before the page opened still opens it');
  const closed = fixture();
  closed.screen.close();
  closed.screen.whenDone(() => { opened += 1; });
  assert.equal(opened, 2, 'a screen closed otherwise opens nothing else');
});

test('a press before the module could answer is pressed again once it can, the last one only', async () => {
  const {pressEarly} = await import('./first-screen.mjs');
  const pressed = [];
  const document = {getElementById: id => ({click: () => pressed.push(id)})};
  const scope = {__tiloFirstScreenEarly: ['first-screen-manual-toggle', 'first-screen-agent']};
  pressEarly(document, scope);
  assert.deepEqual(pressed, ['first-screen-agent']);
  assert.equal(scope.__tiloFirstScreenReady, true, 'later presses reach the buttons themselves');
  pressEarly(document, scope);
  assert.deepEqual(pressed, ['first-screen-agent'], 'a press is replayed once');
  pressEarly(document, {});
  assert.deepEqual(pressed, ['first-screen-agent'], 'nothing pressed early, nothing pressed');
});

/** Codes kept by the setup, followed again: `resume(saved)` answers the
 * code of that name among `codes`. */
const resumer = (...codes) => saved => codes.find(code => code.code === saved.code) ?? null;

test('a reload mid-way shows the step that was kept at once, then follows the same code', async () => {
  const stored = new Map();
  const first = fixture({stored});
  const code = fakeCode();
  first.screen.offerSetup(maker(code).make, resumer(code));
  await settle();
  await first.screen.copy();
  await settle();
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await first.tick();
  assert.equal(first.kept().step, 'waiting');
  assert.deepEqual(first.kept().polled.steps, ['resolved', 'logged-in'], 'what the agent did is kept');
  assert.deepEqual(readShown({getItem: key => stored.get(key)}).lines.map(({text}) => text).slice(0, 2), ['Tilo is installed', 'Claude Code logged in as you'],
    'and what the screen drew, for the next first paint');
  // Loaded again: the page has no codes to offer yet.
  const again = fixture({stored});
  assert.deepEqual(again.last('waiting'), ['waiting', 'setup', {text: setupPrompt(CODE), copied: true, manual: false}], 'the same step, before the host answers');
  assert.deepEqual(again.last('steps')[1].map(({text}) => text).slice(0, 2), ['Tilo is installed', 'Claude Code logged in as you']);
  assert.deepEqual(again.last('net'), ['net', 'Checking with tilo.sh…'], 'and that it is checking');
  const made = maker(fakeCode({code: NEXT}));
  again.screen.offerSetup(made.make, resumer(code));
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
  back.screen.whenDone(() => { opened += 1; });
  back.screen.offerSetup(maker(fakeCode({code: NEXT})).make, resumer(code));
  await settle();
  assert.equal(back.last('connected')[1].heading, 'Claude Code is in your workspace');
  [...back.state.timers.values()].find(timer => timer.ms === CONNECTED_MS).run();
  await settle();
  assert.equal(opened, 1);
  assert.equal(back.kept().step, 'done');
});

test('a code that runs out shows a fresh one with a note, kept so, and copying it follows the fresh one', async () => {
  const stored = new Map();
  const {screen, last, state, tick, copied, kept} = fixture({stored});
  const code = fakeCode();
  const next = fakeCode({code: NEXT, expiresAt: 1_300_000});
  screen.offerSetup(maker(code, next).make, resumer(code, next));
  await settle();
  await screen.copy();
  await settle();
  state.clock = 600_000;
  await tick();
  assert.deepEqual(last('expired'), ['expired', 'setup', {text: setupPrompt(NEXT)}], 'the fresh prompt shows');
  assert.equal(kept().step, 'expired');
  // Reloaded now, the screen still says it ran out, with the fresh prompt.
  const again = fixture({stored, clock: 600_000});
  again.screen.offerSetup(maker().make, resumer(code, next));
  await settle();
  assert.deepEqual(again.last('expired'), ['expired', 'setup', {text: setupPrompt(NEXT)}]);
  await screen.again('', 'prompt');
  await settle();
  assert.deepEqual(copied.at(-1), setupPrompt(NEXT), 'the Copy beside it copies the fresh prompt');
  assert.equal(kept().step, 'waiting');
  assert.equal(kept().code.code, NEXT, 'and follows it');
});

test('offline, then online: said, kept following, picked up at once', async () => {
  let online = true;
  const {screen, last, tick} = fixture({online: () => online});
  const code = fakeCode();
  screen.offerSetup(maker(code).make);
  await settle();
  await screen.copy();
  await settle();
  const follow = code.follow;
  code.follow = () => Promise.reject(new TypeError('Failed to fetch'));
  online = false;
  await tick();
  assert.deepEqual(last('net'), ['net', 'You’re offline.']);
  online = true;
  code.follow = follow;
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  screen.online();
  await settle();
  assert.deepEqual(last('net'), ['net', null]);
  assert.equal(last('steps')[1][1].text, 'Claude Code logged in as you');
});

test('a slow host and a quiet agent are each said, with the reason, while the tab keeps following', async () => {
  const {screen, last, state, tick, fire} = fixture();
  const code = fakeCode();
  screen.offerSetup(maker(code).make);
  await settle();
  await screen.copy();
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
  logged.screen.offerSetup(maker(code).make);
  await settle();
  await logged.screen.copy();
  await settle();
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await logged.tick();
  code.follow = () => Promise.reject(gone);
  await logged.tick();
  assert.equal(logged.names().at(-1), 'opening');
  assert.equal(logged.kept().step, 'opening');
  const unused = fixture();
  const other = fakeCode();
  unused.screen.offerSetup(maker(other).make);
  await settle();
  await unused.screen.copy();
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
  a.screen.offerSetup(made.make, resumer(code));
  await settle();
  const b = fixture({stored});
  const theirs = maker();
  b.screen.offerSetup(theirs.make, resumer(code));
  await settle();
  assert.equal(theirs.made.length, 0, 'the second tab makes no competing code');
  assert.equal(b.last('offer')[2].text, setupPrompt(CODE), 'it shows the first tab’s prompt');
  await a.screen.copy();
  await settle();
  b.screen.sync();
  await settle();
  assert.deepEqual(b.last('waiting')[2].text, setupPrompt(CODE), 'copied in one tab, the other follows the same code');
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await b.tick();
  assert.equal(b.last('steps')[1][1].text, 'Claude Code logged in as you');
  a.screen.back();
  b.screen.sync();
  assert.equal(b.names().includes('start'), true, 'back in one is back in both');
  stored.set(SETUP, JSON.stringify({step: 'done'}));
  b.screen.sync();
  assert.ok(b.screen.closed);
  assert.equal(b.state.reloads, 1, 'done in another tab: this one loads where the page opens now');
});

test('Back in a second tab reaches the first, though the first redraws on its own copy of storage before it hears it', async () => {
  // Each tab reads and writes a copy of storage of its own, and hears another
  // tab's write later, as a browser's tabs do: `deliver` hands each tab's
  // writes to the other, with the storage event that tells it. A tab whose
  // own write of a key is not through yet keeps its value over one that
  // arrives meanwhile, though it is told of that one all the same.
  const tabs = {};
  const sent = [];
  const copyOf = name => Object.assign(new Map(), {
    pending: new Map(),
    set(key, value) {
      Map.prototype.set.call(this, key, value);
      this.pending.set(key, (this.pending.get(key) ?? 0) + 1);
      sent.push({name, key, value});
      return this;
    },
  });
  const deliver = async () => {
    while (sent.length) {
      const {name, key, value} = sent.shift();
      const from = tabs[name].stored;
      from.pending.set(key, from.pending.get(key) - 1);
      for (const [other, tab] of Object.entries(tabs)) {
        if (other === name) continue;
        if (!tab.stored.pending.get(key)) Map.prototype.set.call(tab.stored, key, value);
        if (key === SETUP) tab.screen.sync();
      }
      await settle();
    }
  };
  const code = fakeCode();
  tabs.a = fixture({stored: copyOf('a')});
  tabs.a.screen.offerSetup(maker(code, fakeCode({code: NEXT})).make, resumer(code));
  await settle();
  await tabs.a.screen.copy();
  await settle();
  await deliver();
  // The second tab opens on the same code, and draws its own screen.
  const second = copyOf('b');
  for (const [key, value] of tabs.a.stored) Map.prototype.set.call(second, key, value);
  tabs.b = fixture({stored: second});
  tabs.b.screen.offerSetup(maker().make, resumer(code));
  await settle();
  assert.equal(tabs.b.last('waiting')?.[2]?.text, setupPrompt(CODE), 'the second tab waits on the same code');
  await deliver();
  // Back in the second tab; the first redraws (its next round of asking the
  // host) before that reaches it.
  tabs.b.screen.back();
  await tabs.a.tick();
  await deliver();
  assert.equal(tabs.b.kept().step, 'start', 'the second tab stays at the start');
  assert.equal(tabs.a.kept().step, 'start', 'the first tab keeps the start the second saved');
  assert.ok(tabs.a.names().lastIndexOf('start') > tabs.a.names().lastIndexOf('waiting'), 'and is drawn back at the start');
});

test('reloaded on the start, the kept prompt shows and copies before the page can follow codes, then is followed', async () => {
  const stored = new Map();
  const code = fakeCode();
  stored.set(SETUP, JSON.stringify({step: 'start', offer: code.saved}));
  const {screen, last, names, copied, kept, tick} = fixture({stored});
  assert.deepEqual(last('offer')[2], {text: setupPrompt(CODE), code: CODE}, 'the same prompt, at once');
  assert.equal(names().includes('net'), false);
  await screen.copy();
  await settle();
  assert.deepEqual(copied, [setupPrompt(CODE)], 'a click before the page loaded copies it');
  assert.equal(kept().code.code, CODE);
  await tick();
  assert.equal(last('net')?.[1] ?? null, null, 'waiting for the page is no failure of the host');
  screen.offerSetup(maker(fakeCode({code: NEXT})).make, resumer(code));
  await settle();
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await tick();
  assert.equal(last('steps')[1][1].text, 'Claude Code logged in as you', 'followed once the page can');
});

test('reloaded on the start, the kept prompt is kept again, and works before anyone copies it', async () => {
  const stored = new Map();
  const code = fakeCode();
  stored.set(SETUP, JSON.stringify({step: 'start', offer: code.saved}));
  const {screen, last, fire} = fixture({stored});
  screen.offerSetup(maker(fakeCode({code: NEXT})).make, resumer(code));
  await settle();
  assert.ok(code.asked >= 1, 'followed again as soon as the page can');
  Object.assign(code.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await fire(SHOWN_FOLLOW_MS);
  assert.deepEqual(last('waiting'), ['waiting', 'setup', {text: setupPrompt(CODE), copied: false, handed: true, agent: 'Claude Code'}],
    'the screen says what the agent is doing, and makes no claim of a copy');
  assert.equal(last('steps')[1][1].text, 'Claude Code logged in as you');
});

test('reloaded on the start, the kept prompt stays when the page can follow codes again: no new code is made', async () => {
  // The kept offer is reused rather than a code made and kept each time: a
  // few reloads stay inside the host's create budget for one address.
  const stored = new Map();
  const code = fakeCode();
  stored.set(SETUP, JSON.stringify({step: 'start', offer: code.saved}));
  for (let reload = 1; reload <= 5; reload += 1) {
    const {screen, last} = fixture({stored});
    const made = maker(fakeCode({code: NEXT}));
    screen.offerSetup(made.make, resumer(code));
    await settle();
    assert.equal(made.made.length, 0, `reload ${reload}: no new code is made`);
    assert.equal(last('offer')[2].code, CODE);
  }
  assert.ok(code.asked >= 5, 'each reload follows the kept code, which the host already has');
});

test('the prompt breaks only between its words: a flag and a URL each stay whole', () => {
  const node = (kind, text = '') => ({kind, className: '', textContent: text});
  const document = {createTextNode: text => node('text', text), createElement: () => node('span')};
  const element = {ownerDocument: document, children: [], replaceChildren(...children) { this.children = children; },
    get textContent() { return this.children.map(child => child.textContent).join(''); }};
  const prompt = 'Set up Tilo for me: run curl -fsSL https://tilo.example | sh -s login D9B-X4GK --site https://tilo.example, then do what it tells you.';
  showPrompt(element, prompt);
  assert.equal(element.textContent, prompt, 'what is copied from the box is the prompt itself');
  const words = element.children.filter(child => child.className === 'fr-word').map(child => child.textContent);
  for (const whole of ['-fsSL', 'https://tilo.example', '--site', 'D9B-X4GK']) assert.ok(words.includes(whole), `${whole} stays one unbreakable word`);
  assert.ok(element.children.filter(child => child.kind === 'text').every(child => /^ +$/.test(child.textContent)), 'it breaks only at spaces');
});

/** Enough of a document for the first screen's own markup: every element it
 * asks for, answering to nothing. `removed` says whether the screen was taken
 * out of the page, which is what showing it again depends on. */
function stubDocument() {
  const made = [];
  // Every element is of the one document, however late it is made.
  const owner = {document: null};
  const element = id => {
    const el = {
      id, dataset: {}, textContent: '', value: '', hidden: false, removed: false, style: {}, className: '',
      get ownerDocument() { return owner.document; },
      attrs: new Map(), children: [], kind: 'element',
      setAttribute(name, value) { this.attrs.set(name, String(value)); },
      getAttribute(name) { return this.attrs.get(name) ?? null; },
      removeAttribute(name) { this.attrs.delete(name); },
      addEventListener() {}, removeEventListener() {}, focus() {}, select() {}, append() {}, appendChild(child) { this.children.push(child); return child; },
      replaceChildren(...children) { this.children = children; }, closest() { return this; }, remove() { this.removed = true; },
      querySelector() { return null; }, querySelectorAll() { return []; },
    };
    made.push(el);
    return el;
  };
  const byId = new Map();
  const getElementById = id => {
    if (!byId.has(id)) byId.set(id, element(id));
    return byId.get(id);
  };
  const root = element('html');
  const document = {documentElement: root, getElementById, getSelection: () => null, addEventListener() {},
    createElement: name => element(name), createTextNode: text => ({kind: 'text', textContent: text}), body: element('body')};
  owner.document = document;
  return {document, root, screen: () => byId.get('first-screen'), made};
}

test('a code an agent took without a copy: the screen says what it is doing, and claims no copy', () => {
  const {document, screen} = stubDocument();
  const view = domView(document);
  view.waiting('setup', {copied: false, handed: true, agent: 'Claude Code'});
  assert.equal(screen().dataset.handed, 'true', 'the screen swaps its heading: what the agent is doing, not a paste');
  assert.equal(screen().dataset.copied, 'false', 'and says nothing was copied');
  assert.equal(document.getElementById('first-screen-status').textContent, 'Claude Code is setting up Tilo…');
  view.waiting('setup', {copied: true});
  assert.equal(screen().dataset.handed, 'false', 'a copy with the button keeps the paste instructions');
  assert.equal(screen().dataset.copied, 'true');
});

test('the agent setup can be shown again from the person\'s card, in the same place', () => {
  const {document, root, screen} = stubDocument();
  const storage = {getItem: () => JSON.stringify({step: 'done'}), setItem() {}};
  const controls = openFirstScreen(document, {storage});
  assert.ok(controls, 'the setup, again');
  assert.equal(typeof controls.offerSetup, 'function', 'it takes a fresh code, as the first screen did');
  assert.equal(root.attrs.get('data-first-screen'), '', 'the screen is shown');
  assert.ok(screen(), 'in the page it was first shown in');
  // The person is set up: the setup is what shows, never the sign-in column a
  // past visit left the screen in.
  assert.equal(screen().dataset.reopened, 'true', 'the Returning? column has nobody left to sign in');
  assert.equal(screen().dataset.mode, 'setup', 'the setup is what the card asked for');
  screen().dataset.mode = 'sign-in';
  openFirstScreen(document, {storage});
  assert.equal(screen().dataset.mode, 'setup', 'opening it again starts the setup, whatever it showed before');
  // Leaving it takes it off the page without taking it out of the document.
  document.getElementById('first-screen').dataset.closing = '';
  assert.notEqual(screen().removed, true, 'the same screen is shown the next time');
  assert.equal(openFirstScreen({getElementById: () => null}), null, 'a page without one shows nothing');
});

test('signing in from the wait list as the setup screen fades out keeps the sign-in on the page', async () => {
  const {document, root} = stubDocument();
  root.setAttribute('data-first-screen', '');
  // The wait list closes the first visit's setup screen: it fades, then leaves the page.
  domView(document).close();
  // "Sign in with your agent", pressed before the fade is over.
  assert.ok(openSignIn(document));
  await new Promise(resolve => setTimeout(resolve, 400));
  assert.equal(root.getAttribute('data-first-screen'), '', 'the sign-in screen is still shown once the fade is over');
  assert.equal(document.getElementById('first-screen').getAttribute('data-closing'), null, 'and nothing on it is fading');
  // A close nothing opened again after still takes the screen off the page.
  domView(document).close();
  await new Promise(resolve => setTimeout(resolve, 400));
  assert.equal(root.getAttribute('data-first-screen'), null, 'a closed screen leaves the page');
});

test('a setup reopened from the card asks for a fresh code, not one an earlier visit kept', () => {
  const {document} = stubDocument();
  const kept = {step: 'done', offer: {code: CODE, expiresAt: Date.now() + 600_000, follow: 'ab'.repeat(32)}};
  const stored = [];
  const storage = {getItem: key => (key === SETUP ? JSON.stringify(kept) : null),
    setItem: (key, value) => { if (key === SETUP) stored.push(JSON.parse(value)); }};
  const controls = openFirstScreen(document, {storage});
  assert.ok(controls);
  assert.equal(stored.at(-1)?.offer, null, 'the code an earlier visit kept is spent or run out, never offered again');
});

test('a first paint that held the screen shows it once the page says the site let the person in', () => {
  const {document, root} = stubDocument();
  root.dataset.firstScreenHold = '';
  const storage = {getItem: () => null, setItem() {}};
  const controls = startFirstScreen(document, {storage});
  assert.ok(controls, 'the screen is wired while the entry decides');
  assert.equal(controls.held, true);
  assert.equal(root.dataset.firstScreen, undefined, 'nothing painted: the wait list may take its place');
  controls.reveal();
  assert.equal(root.dataset.firstScreen, '', 'shown once the page is in');
  assert.equal(controls.held, false);
  controls.close();
  // A screen the first paint showed is left where it was.
  const painted = stubDocument();
  painted.root.dataset.firstScreen = 'waiting';
  const paintedControls = startFirstScreen(painted.document, {storage});
  assert.equal(paintedControls.held, false);
  paintedControls.reveal();
  assert.equal(painted.root.dataset.firstScreen, 'waiting');
  assert.equal(startFirstScreen(stubDocument().document, {storage}), null, 'a page with neither shows nothing');
});

test('a host that did not answer is named, and one on this computer is not blamed on the connection', () => {
  const unreached = origin => Object.assign(new Error('The host could not be reached'), {code: 'unreachable', origin});
  assert.equal(setupFailure(unreached('https://eu1.tilo.sh'), {site: 'https://tilo.sh'}),
    'Couldn’t reach eu1.tilo.sh. Check your connection, then try again.');
  assert.equal(setupFailure(unreached('http://127.0.0.1:24100'), {site: 'http://tilo.localhost:24120'}),
    'Couldn’t reach 127.0.0.1:24100 on this computer. Check that it is running, then try again.');
  assert.equal(setupFailure(unreached('https://eu1.tilo.localhost:24100'), {site: 'https://tilo.localhost:24120'}),
    'Couldn’t reach eu1.tilo.localhost:24100 on this computer. It isn’t running, or this browser doesn’t trust its certificate.');
  assert.equal(setupFailure(new TypeError('Failed to fetch'), {site: 'https://tilo.sh'}),
    'Couldn’t reach tilo.sh. Check your connection, then try again.');
});

// Shown and never copied, the setup code is followed while it is on the
// screen and an agent that uses it moves the screen on: however the person
// reached the screen, whatever step the setup was kept at before it showed.
async function shownCodeIsKept(t, kept, {expire = false} = {}) {
  const stored = new Map();
  const code = fakeCode();
  const next = fakeCode({code: NEXT, expiresAt: 1_300_000});
  if (kept) stored.set(SETUP, JSON.stringify(typeof kept === 'function' ? kept(code) : kept));
  const {screen, last, fire, state, tick, copied} = fixture({stored});
  const was = readSetup({getItem: key => stored.get(key) ?? null})?.step ?? null;
  screen.offerSetup(maker(code, next).make, resumer(code, next));
  screen.offerAgentSignIn(maker(fakeCode({code: 'S1G-NNNN'})).make);
  await settle();
  let shown = code;
  if (expire) {
    await screen.copy();
    await settle();
    state.clock = 600_000;
    await tick();
    assert.deepEqual(last('expired'), ['expired', 'setup', {text: setupPrompt(NEXT)}], `${t}: the fresh prompt shows`);
    shown = next;
  }
  const asked = shown.asked;
  assert.ok(asked >= 1, `${t}: kept as soon as it is on the screen`);
  await fire(SHOWN_FOLLOW_MS);
  await fire(SHOWN_FOLLOW_MS);
  assert.ok(shown.asked >= asked + 2, `${t}: followed again while it is on the screen`);
  if (!expire && (was === 'later' || was === 'done')) {
    assert.equal(readSetup({getItem: key => stored.get(key) ?? null}).step, was,
      `${t}: a prompt only shown keeps the setup at the step it was kept at`);
  }
  Object.assign(shown.polled, {resolved: true, agent: 'Claude Code', steps: ['resolved', 'logged-in']});
  await fire(SHOWN_FOLLOW_MS);
  assert.equal(last('waiting')?.[2]?.text, setupPrompt(shown.code), `${t}: the screen waits for the agent`);
  assert.equal(last('steps')[1][1].text, 'Claude Code logged in as you', `${t}: and shows its steps`);
  assert.equal(readSetup({getItem: key => stored.get(key) ?? null}).step, 'waiting', `${t}: kept as under way`);
  assert.equal(copied.length, expire ? 1 : 0, `${t}: nothing copied but what the person copied`);
}

test('a code on the screen works without a copy however the person reached the screen', async () => {
  await shownCodeIsKept('straight to the site');
  await shownCodeIsKept('opened at the start', {step: 'start'});
  await shownCodeIsKept('after joining by an invitation', {step: 'later', at: 1});
  await shownCodeIsKept('reloaded after joining by an invitation', code => ({step: 'later', at: 1, offer: code.saved}));
  await shownCodeIsKept('reloaded on the start', code => ({step: 'start', offer: code.saved}));
  await shownCodeIsKept('from the workspace’s later setup offer, or the person’s card', {step: 'done', at: 1});
  await shownCodeIsKept('a fresh code after one ran out', null, {expire: true});
  await shownCodeIsKept('a fresh code after one ran out, after an invitation', {step: 'later', at: 1}, {expire: true});
});

test('a prompt only shown after an invitation keeps the setup for later, even when another tab changes it', async () => {
  const stored = new Map([[SETUP, JSON.stringify({step: 'later', at: 1})]]);
  const code = fakeCode();
  const {screen, fire} = fixture({stored});
  screen.offerSetup(maker(code).make, resumer(code));
  await settle();
  assert.equal(setupLater({getItem: key => stored.get(key) ?? null}), true, 'the workspace still offers the setup');
  // Another tab keeps the same setup (its screen drew something): this one
  // stays on the start, following its code.
  stored.set(SETUP, JSON.stringify({...JSON.parse(stored.get(SETUP)), shown: {offer: 'x'}}));
  screen.sync();
  const asked = code.asked;
  await fire(SHOWN_FOLLOW_MS);
  assert.ok(code.asked > asked, 'still followed');
});
