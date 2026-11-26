/**
 * The first screen of a first visit: set Tilo up with your agent, skip for
 * now, or sign in again.
 *
 * It is plain HTML in the entry page (`#first-screen` in index.html), shown
 * from the first paint by `first-screen.inline.js`, while the runtime loads
 * behind it. This module only wires it up, and costs nothing until a
 * button is used.
 *
 * A visitor this browser knows nothing of is held back
 * (`first-screen.inline.js`): a site that takes people by invitation only
 * turns them away to its wait list, and the setup is no way in there. The
 * page shows the screen once the site's entry let them in (`reveal`), and
 * the visitor it turned away sees the wait list instead.
 *
 * Everything an agent does for the person goes through a ticket the tab's
 * host serves it (`tilo_device_identity::login`): the tab asks its host for
 * one, the agent runs `tilo login <ticket>` on a computer (or
 * `tilo device add <ticket>` on one of the person's own, to sign this tab
 * in), and the tab follows the ticket (`follow()`, about once a second),
 * showing each step the computer reports as it happens.
 *
 * The page makes its codes as soon as it can (`offerSetup`,
 * `offerAgentSignIn`), before anyone asks, and keeps a fresh one ready,
 * renewed shortly before it expires — but for a first visit's setup only
 * once the person asks for it (`offerSetup({ahead: false})`): the workspace
 * its code names is made on the server with it, and one made for a browser
 * still deciding is left behind by everyone who signs in as someone else.
 * So what a button copies is already on the screen: the click writes exactly
 * that text, inside the click, and the screen says it copied only once the
 * clipboard took it. A code the page has not made yet is never promised: the
 * button says what it waits for, and a wait that takes too long says why, with
 * a way to try again.
 *
 * A code on the screen has to work however it leaves the page, and the host
 * keeps a code only from its tab's first follow: a person who selects the
 * prompt and copies it by hand, or reads the code out and types it to their
 * agent, never presses the button. So a shown code is followed at once
 * (`watchShown`, the follow that has the host keep it) and followed quietly
 * again every couple of seconds while it is on the screen; an agent that
 * used it moves the screen on to its steps, as a copied prompt does. Nothing
 * was copied there, so the screen says what the agent is doing instead of
 * asking for a paste (`handed`). A prompt kept from before a reload that
 * this page cannot follow any more is not looked at in silence either: after
 * a while the screen says so, and reloading makes a fresh code.
 *
 * - **Set up with your agent** (a first visit; **Copy prompt** once the
 *   person asked for it) copies `setupPrompt`, shown above it. The code names
 *   a pairing link to this tab, which holds the person's new root: the
 *   agent's `tilo` command becomes a device of the person with a key of its
 *   own, certified here (the root never leaves). Once the agent is in the
 *   person's workspace the screen says so and leaves (`whenDone` opens that
 *   workspace).
 * - **Set up manually** shows the MCP server's address and the commands to
 *   run by hand, with the same code, which this tab follows the same way.
 * - **Skip for now** opens the person's own workspace (`whenDone`): the
 *   page joined the site's home workspace behind the screen, so they are in
 *   it, but it is not what they see first.
 * - **Returning?** signs in as someone who has been here: **Sign in with
 *   your agent** copies `signInPrompt`, whose code names nothing; a
 *   computer that is already a device of the person claims it, and the page
 *   asks to be certified as that person (`signIn(claimed)`). The sign-in
 *   screen offers the ways side by side: the prompt for their agent, the same
 *   ticket as a `tilo` command (`signInCommand`) the person types into a
 *   terminal on such a computer, and **Use a passkey**, which asks for one
 *   once the page can (a click before then waits, for a while).
 */

/**
 * Where this browser keeps the person's setup (`readSetup`): the step the
 * first screen is at, the codes it shows and follows, and what the agent did
 * so far. The page decides from it alone whether a bare entry opens on the
 * first screen, and at which step (first-screen.inline.js), so a reload, a
 * closed and reopened tab, back and forward, or a second tab show the same
 * step; another tab's change reaches this one as a `storage` event.
 */
export const SETUP = 'tilo-setup';
/**
 * What the first screen shows (the prompts, the agent's steps, the heading),
 * kept apart from the setup for the next first paint (first-screen.inline.js),
 * with the code it was drawn for (`code`). A redraw writes only this: every
 * tab keeps its own cached copy of storage, so a tab rewriting the setup on a
 * redraw would put back the step another tab just left.
 */
export const SHOWN = 'tilo-setup-shown';

/** What the first screen showed last (`SHOWN`), or `null`. */
export function readShown(storage = globalThis.localStorage) {
  try {
    const shown = JSON.parse(storage?.getItem(SHOWN) ?? 'null');
    return shown && typeof shown === 'object' ? shown : null;
  } catch { return null; }
}
/** The steps a setup is kept at: kept for later (`later`: the person
 * arrived by an invitation and joined that workspace first), on the start
 * (`start`), following a code (`waiting`), its code ran out (`expired`), the
 * agent is in (`connected`), the person's own workspace opening (`opening`),
 * and over (`done`). */
const SETUP_STEPS = new Set(['later', 'start', 'waiting', 'expired', 'connected', 'opening', 'done']);

/** Whether this browser's person arrived by an invitation and has not set
 * up their own Tilo yet: the workspace offers the setup once they used it a
 * while, and the site itself shows it only while they have joined none. */
export function setupLater(storage = globalThis.localStorage) {
  return readSetup(storage)?.step === 'later';
}
/** How long the screen says the agent connected before it leaves. */
export const CONNECTED_MS = 1600;
/** How often the tab asks where its code is. */
export const FOLLOW_MS = 1000;
/** How often a prompt on the screen asks where its code is, until someone
 * uses it: slower than the active follow, quick enough that an agent that
 * took the code without the button shows its steps at once. */
export const SHOWN_FOLLOW_MS = 2000;
/** How long a prompt kept from before a reload may stay one this page cannot
 * follow before the screen says so, with the reload that makes a fresh one. */
export const KEPT_LOST_MS = 15_000;
/** How long the host gets to make a code before the screen says it did not. */
export const MAKE_MS = 15_000;
/** How long the page gets to offer codes at all (its runtime loading) before the screen says so. */
export const OFFER_MS = 30_000;
/** How long before a ready code expires a fresh one replaces it. */
export const RENEW_MS = 60_000;
/** How long one question about a code may take before it counts as unanswered. */
export const ASK_MS = 8_000;
/** How long the agent may report nothing new before the screen says where to look. */
export const QUIET_MS = 3 * 60_000;
/** How long opening the person's own workspace waits before it says why it is slow. */
export const OPEN_SLOW_MS = 20_000;
/** How long a passkey sign-in waits for the page to be able to ask. */
export const PASSKEY_WAIT_MS = 30_000;
/** The site whose commands need no `--site`: a ticket names its own host. */
export const HOME_SITE = 'https://tilo.sh';
/** Tilo's MCP server, for an agent added by hand. */
export const MCP_URL = 'https://mcp.tilo.sh';

/** `site` as the curl command and `tilo login` read it: tilo.sh is the default. */
const siteOf = site => (!site || site === HOME_SITE ? null : site);
/** The command that installs `tilo` from `site`; with `args`, the installer
 * then runs the installed `tilo` with them by its full path, so the line works
 * on a machine whose PATH does not name where it went (a fresh one, or an
 * agent's shell, which never rereads the profile that would add it). */
export const installCommand = (site, args = '') => `curl -fsSL ${siteOf(site) ?? 'tilo.sh'} | sh${args ? ` -s ${args}` : ''}`;
/** `login <ticket>`: a ticket names the host it is served from, so no site is said. */
const loginArgs = code => `login ${code}`;
/** `tilo login <ticket>`. */
export const loginCommand = code => `tilo ${loginArgs(code)}`;

/** The prompt that has an agent set Tilo up: one line, complete on its own,
 * since `tilo login` tells the agent the rest. The command ends it, so nothing
 * of the sentence touches the ticket and the command copies as it is. */
export function setupPrompt(code, {site} = {}) {
  return `Set up Tilo for me: run this command, then do what it tells you: ${installCommand(site, loginArgs(code))}`;
}

/** The command that signs this tab in as the person, run on a computer that is
 * already one of their devices: by the person in a terminal, or by their agent. */
export const signInCommand = code => `tilo device add ${code}`;

/** The prompt that has an agent sign this tab in as the person, on a computer
 * that is already one of their devices: it approves this tab with the ticket,
 * which ends the prompt with no punctuation after it. */
export function signInPrompt(code) {
  return `Sign me in to Tilo: run this command, then do what it tells you: ${signInCommand(code)}`;
}

/** The command for someone who sets up by hand: their agent then adds the MCP server
 * (`MCP_URL`) where its own configuration keeps servers. */
export function manualCommands(code, {site} = {}) {
  return installCommand(site, loginArgs(code));
}

/** "10 more minutes", the time left until `expiresAt` (ms) at `now` in whole
 * minutes, counted up so a code with time left never reads as none. */
export function minutesLeft(expiresAt, now) {
  const minutes = Math.max(1, Math.ceil((expiresAt - now) / 60_000));
  return minutes === 1 ? '1 more minute' : `${minutes} more minutes`;
}

/** A code as the setup keeps it: `{code, expiresAt, follow}` (the secret
 * that follows it), with the pairing invite it names when it sets up. */
const keptCode = code => Boolean(code) && typeof code.code === 'string' && Number.isFinite(code.expiresAt)
  && typeof code.follow === 'string';

/** The setup this browser keeps in `storage` (`SETUP`), or `null`. */
export function readSetup(storage = globalThis.localStorage) {
  let state;
  try { state = JSON.parse(storage?.getItem(SETUP) ?? 'null'); } catch { return null; }
  if (!state || typeof state !== 'object' || !SETUP_STEPS.has(state.step)) return null;
  if (state.offer && !keptCode(state.offer)) state.offer = null;
  if (state.code && !keptCode(state.code)) state.code = null;
  return state;
}

/** Keep `state` as this browser's setup: whether storage took it. */
export function writeSetup(state, storage = globalThis.localStorage) {
  try { storage?.setItem(SETUP, JSON.stringify(state)); return true; } catch { return false; }
}

/** The setup is over (the person's own workspace opens, or a link signed
 * this tab in): a bare entry opens where the page would from now on. */
export function setupDone(storage = globalThis.localStorage, now = Date.now()) {
  return writeSetup({step: 'done', at: now}, storage);
}

/**
 * What the screen lists for where a code is (`polled`, the host's
 * `Polled`): each step done so far, then the one under way, as
 * `{text, done}`; `{text, act: true}` when the person has to do it, and
 * `{text, wait: true}` until the host sees the code used: nothing is under
 * way then, so nothing claims to be. Once the agent reported it failed, the
 * step it stopped at is `{text, failed: true}`, under way no longer. `mode`
 * is `setup` or `sign-in`.
 */
export function stepLines(polled, mode = 'setup') {
  const agent = polled?.agent || 'Your agent';
  const steps = new Set(polled?.steps ?? []);
  const lines = [];
  const done = text => lines.push({text, done: true});
  if (mode === 'sign-in') {
    if (!polled?.resolved) return [{text: 'Waiting for your agent', wait: true}];
    if (steps.has('failed') && !steps.has('approved')) return [{text: `${agent} could not sign this tab in`, failed: true}];
    if (!polled.claimed) return [{text: `${agent} is finding you…`}];
    done(`${polled.claimed.by || agent} vouched for you`);
    lines.push(steps.has('approved') ? {text: 'This tab is signed in', done: true} : {text: 'Signing this tab in…'});
    return lines;
  }
  if (!polled?.resolved) return [{text: 'Waiting for your agent', wait: true}];
  done('Tilo is installed');
  const failed = steps.has('failed') && !steps.has('entered');
  if (!steps.has('logged-in')) {
    return [...lines, failed ? {text: `${agent} could not log in as you`, failed: true} : {text: `${agent} is logging in as you…`}];
  }
  done(`${agent} logged in as you`);
  // Each line the host lists is one the computer reported: the tools are
  // said to be added only when it confirmed them, which an agent that adds
  // them itself confirms in its own time.
  if (steps.has('tools-added')) done(`Tilo’s tools are added to ${agent}`);
  // Nor is a restart the computer asked for a restart that happened.
  if (steps.has('entered')) {
    done(`${agent} is in your workspace`);
    return lines;
  }
  // `tilo login` reports a failure once it logged in only when entering the
  // workspace failed.
  if (failed) return [...lines, {text: `${agent} could not enter your workspace`, failed: true}];
  if (steps.has('restart')) return [...lines, {text: `Restart ${agent}, then it continues`, act: true}];
  if (!steps.has('tools-added')) return [...lines, {text: `Adding Tilo’s tools to ${agent}…`}];
  return [...lines, {text: `${agent} is entering your workspace…`}];
}

/** The host a site's codes come from, as people read it: `tilo.sh`. */
const hostOf = site => { try { return new URL(site).host || 'tilo.sh'; } catch { return 'tilo.sh'; } };

/** `site` as its person reads it in the address bar (`tilo.sh`,
 * `tilo.example:8443`), or "this site" where that is an address by number,
 * which names nothing to a person. */
function nameOf(site) {
  let url;
  try { url = new URL(site); } catch { return 'this site'; }
  const name = url.hostname;
  if (!name || /^[\d.]+$/.test(name) || name.startsWith('[')) return 'this site';
  return url.port ? `${name}:${url.port}` : name;
}

/** What the screen says of a passkey sign-in that did not work, from `error`.
 * Where the place cannot use passkeys at all (a web view inside an app that
 * may not use this site's), another passkey changes nothing: it says the ways
 * in that work there. */
export function signInFailure(error) {
  if (['passkeys-refused', 'passkeys-unavailable'].includes(error?.code)) {
    return 'Passkeys can’t be used here. Sign in with your agent, or pair this device from one where you are signed in.';
  }
  return 'That passkey did not sign you in. Try another, or sign in with your agent.';
}

/** What the screen says when the page could not make or use a code, from
 * `error`, on `site`: what failed, and what still works. It says a workspace
 * could not be opened only for a page that stopped (`stopped`), and promises
 * a reason only there: that page shows its own under this screen, which Skip
 * leaves for at once (`skip`). A code that could not be made leaves the
 * person's workspace as it is, and the sentence says so. */
export function setupFailure(error, {site = HOME_SITE} = {}) {
  const code = error?.code;
  if (code === 'stopped') return 'Tilo could not open a workspace for you here yet. Skip for now to see why.';
  if (code === 'no-workspace') return 'There is no workspace here yet to set your agent up in. Try again, or skip for now.';
  if (code === 'no-codes') return 'This tab can’t make the code an agent needs. Try again, or skip for now.';
  // The site's server answered and would not serve a ticket (a `TicketError`):
  // said of the site by the name in the person's address bar, never by the
  // address its tickets come from, which may be one by number. Refused by
  // anything else, the sentence names nobody.
  if (code === 'refused') {
    const by = error?.name === 'TicketError' ? `: ${nameOf(site)} refused it` : '';
    return `The code for your agent could not be made here${by}. Try again, or skip for now and use Tilo without an agent.`;
  }
  if (code === 'copy-failed') return 'Your browser did not let Tilo copy the prompt. Select it and copy it yourself.';
  if (code === 'not-a-device') return 'That computer is not signed in to Tilo as you. Try your agent on one that is, or use a passkey.';
  if (code === 'not-let-in') return 'This tab isn’t let in yet. Try again in a moment.';
  if (code === 'not-loaded') return 'Tilo didn’t finish loading in this tab.';
  // The host that did not answer is the one the codes come from
  // (`error.origin`, which the entry page names), not necessarily the page's.
  const from = typeof error?.origin === 'string' ? error.origin : site;
  const host = hostOf(from);
  if (code === 'timeout' || code === 'unreachable' || error?.name === 'TypeError') {
    // A page cannot read why a request failed, but a host on this computer
    // is not behind a connection: it is not running, or (over https) the
    // browser refused its certificate, which a fetch reports as nothing more.
    const local = onThisComputer(from);
    if (local === 'http:') return `Couldn’t reach ${host} on this computer. Check that it is running, then try again.`;
    if (local === 'https:') return `Couldn’t reach ${host} on this computer. It isn’t running, or this browser doesn’t trust its certificate.`;
    return `Couldn’t reach ${host}. Check your connection, then try again.`;
  }
  return `${host} did not make a code. Try again, or skip for now.`;
}

/** The scheme of `origin` when it is on this computer (a `localhost` name or
 * a loopback address), else `null`. */
function onThisComputer(origin) {
  let url;
  try { url = new URL(origin); } catch { return null; }
  const name = url.hostname;
  const local = name === 'localhost' || name.endsWith('.localhost') || /^127\./.test(name) || name === '[::1]';
  return local ? url.protocol : null;
}

/** What a button that has no code yet says it waits for. */
export function codeWait(site = HOME_SITE) {
  return `Getting your code from ${hostOf(site)}…`;
}

/** `promise`, or a rejection with code `timeout` once `ms` passed, timed by `later`/`clear`. */
function bounded(promise, ms, later, clear) {
  return new Promise((resolve, reject) => {
    const timer = later(() => reject(Object.assign(new Error('no answer in time'), {code: 'timeout'})), ms);
    Promise.resolve(promise).then(value => { clear(timer); resolve(value); }, error => { clear(timer); reject(error); });
  });
}

/**
 * The screen's behaviour, over a `view` that draws it (`domView` below, a
 * fake in tests): which state shows, what each button does, the codes made
 * ahead and renewed, a code's steps and its time running out.
 *
 * The person's setup is kept in `storage` (`SETUP`) at every step, with the
 * codes it shows and follows and what the agent did so far, so the screen
 * starts where it was: a reload, a tab closed and opened again, or back and
 * forward show the same step, drawn from what was kept before the host
 * answers, then confirmed by it. A second tab follows the first (`sync`,
 * on another tab's change): it takes the code shown or followed there rather
 * than making a competing one. What the screen last drew is kept with it
 * (`shown`), which first-screen.inline.js paints at the first frame.
 *
 * `clipboard.copy(text)` writes `text` inside the click and resolves once
 * the clipboard took it; `every(callback, ms)` ticks; `later(callback, ms)` and
 * `clear(timer)` time one thing; `site` is the origin the commands name;
 * `reload()` loads the page again; `online()` says whether the browser
 * thinks it is online. `held`: the screen waits for the page's word before
 * it may show (`reveal`).
 */
export function createFirstScreen({view: drawn, clipboard, storage = null, now = () => Date.now(), site = HOME_SITE,
  held: heldAtPaint = false, signInOnly = false, onBack = () => {},
  every = (callback, ms) => { const timer = setInterval(callback, ms); return () => clearInterval(timer); },
  later = (callback, ms) => setTimeout(callback, ms), clear = timer => clearTimeout(timer),
  reload = () => globalThis.location?.reload(), online = () => globalThis.navigator?.onLine !== false}) {
  // The codes the page makes ahead, per kind: `setup` and `signIn`. `made`
  // is the one a click copies now; `error` says why there is none;
  // `resume(kept)` follows a code kept with the setup again.
  const prompts = {setup: setupPrompt, signIn: signInPrompt};
  const modeOf = kind => (kind === 'signIn' ? 'sign-in' : 'setup');
  const kindOf = mode => (mode === 'sign-in' ? 'signIn' : 'setup');
  const host = hostOf(site);
  const codes = {};
  for (const kind of ['setup', 'signIn']) codes[kind] = {make: null, resume: null, made: null, making: false, error: null, renew: null, round: 0, note: null};
  // A setup kept for later (the person arrived by an invitation) or over
  // (the person's card shows the setup again) is on the start once this
  // screen shows it. Storage keeps that step until the person takes a code
  // into use, so the workspace still offers the setup and a bare entry opens
  // where it did; this screen is on the start all the same.
  const held = step => step === 'later' || step === 'done';
  const onStart = kept => (held(kept.step) ? {...kept, step: 'start'} : kept);
  let state = readSetup(storage) ?? {step: 'start'};
  if (held(state.step)) state = {step: 'start'};
  let closed = false;
  // Whether the screen waits for the page to say the person may set Tilo up
  // here: the first paint held a visit this browser knows nothing of, since
  // a site taking people by invitation only may send them to its wait list
  // instead (`reveal`).
  let heldBack = Boolean(heldAtPaint);
  // What the screen shows now, kept with the setup for the next first paint.
  const frame = {offer: null, prompt: null, lines: null, heading: null, mode: 'setup', copied: false, handed: false};
  // What the screen shows, for the next first paint: its own key, never the
  // setup, so a redraw changes no step (`SHOWN`).
  let keptFrame = null;
  const keepFrame = () => {
    if (closed || !storage) return;
    const next = JSON.stringify({...frame, code: state.code?.code ?? null});
    if (next === keptFrame) return;
    keptFrame = next;
    try { storage.setItem(SHOWN, next); } catch { /* The next first paint draws less of it. */ }
  };
  const view = new Proxy(drawn, {get: (target, name) => (...args) => {
    // A closed screen draws nothing: the person is done with it, or the page
    // was turned away at an entry, and what comes after is not the setup. Every
    // write it makes is the page's first screen itself (`domView`'s `state`),
    // so a late answer cannot bring it back over what took its place.
    if (closed && name !== 'close') return undefined;
    if (name === 'offer' && args[0] === 'setup') frame.offer = args[1]?.text ?? null;
    else if (name === 'waiting') {
      if (typeof args[1]?.text === 'string') frame.prompt = args[1].text;
      frame.mode = args[0];
      frame.copied = args[1]?.copied === true;
      frame.handed = args[1]?.handed === true;
    } else if (name === 'expired' && typeof args[1]?.text === 'string') frame.prompt = args[1].text;
    else if (name === 'steps') frame.lines = args[0];
    else if (name === 'connected') frame.heading = args[0]?.heading ?? null;
    else if (name === 'copied' && args[0] && (args[1] ?? 'prompt') === 'prompt') frame.copied = true;
    const result = target[name]?.(...args);
    keepFrame();
    return result;
  }});
  // Keep `patch` as the setup now, merged into what storage holds at this
  // moment (another tab may have moved it on). Only a change of the setup
  // itself saves: a code taken up, Back, an answer heard. A save that names no step (a code made ahead, say) leaves a held step
  // in storage, and this screen where it is: read back as the step, `later`
  // would take the shown code off the screen (`onScreen`), and the host would
  // forget a code the person may still hand their agent.
  const save = patch => {
    if (closed) return;
    const kept = readSetup(storage) ?? state;
    const next = {...kept, ...patch, at: now()};
    writeSetup(next, storage);
    state = 'step' in patch || !held(kept.step) ? next : {...next, step: state.step};
    keepFrame();
  };
  const sameState = (a, b) => JSON.stringify({...onStart(a), shown: null, at: null})
    === JSON.stringify({...onStart(b), shown: null, at: null});
  let passkeyAction = null;
  const passkeyWaiting = [];
  let gone = null;
  let stop = () => {};
  let askNow = () => {};
  let attempt = 0;
  let manualShown = false;
  // Whether the setup code is made before the person asks for it
  // (`offerSetup`): a first visit's is not, since the workspace it names is
  // made with it.
  let ahead = true;
  // Whether the person asked for a setup code before the page could make
  // one: made once it can (`offerSetup`).
  let asked = false;
  // The code the screen follows (or showed run out), so another tab's
  // change to the same code changes nothing here.
  let following = null;
  let expiredKind = null;
  let connectedShown = false;
  // What leaving the screen opens (the person's own workspace), once the
  // page can: `null` until then, while the screen says it is opening.
  let onDone = null;
  let leaving = false;
  let opened = false;
  let leaveTimer = null;
  let slowTimer = null;
  const close = () => {
    if (closed) return;
    stop();
    stopWatch();
    for (const kind of Object.keys(codes)) clear(codes[kind].renew);
    clear(offerTimer);
    clear(leaveTimer);
    clear(slowTimer);
    closed = true;
    heldBack = false;
    view.close();
  };
  // Open the person's own workspace, once leaving and the page can.
  const openOwn = () => {
    if (!leaving || !onDone || opened || closed) return;
    opened = true;
    Promise.resolve().then(() => onDone({signedIn: false})).catch(error => console.warn('first screen: opening your workspace', error)).finally(() => {
      // Opening loads the page anew; where it did not, the setup is over here.
      if (readSetup(storage)?.step !== 'done') setupDone(storage, now());
      close();
    });
  };
  // Signed in: open what the page opens for the person (`whenDone`), once it can.
  let signedIn = false;
  const openSignedIn = () => {
    if (!signedIn || !onDone || closed) return;
    leaving = true;
    onDone({signedIn: true});
    close();
  };
  // Leave the screen for the person's own workspace (`whenDone`). Until it
  // opens the screen says so, and the setup is kept as opening: a reload in
  // between opens it as well, never the site's home workspace.
  const leave = () => {
    if (closed || leaving) return;
    leaving = true;
    clear(leaveTimer);
    stop();
    stopWatch();
    ++attempt;
    save({step: 'opening'});
    view.opening({heading: connectedShown ? frame.heading : null});
    slowTimer = later(() => view.opening({heading: connectedShown ? frame.heading : null,
      note: `Still opening: ${host} is slow to answer.`}), OPEN_SLOW_MS);
    openOwn();
  };
  // The agent is in (or this tab is signed in): said for a moment, then the
  // screen leaves.
  const connect = (mode, agent) => {
    if (connectedShown || closed) return;
    connectedShown = true;
    const heading = mode === 'sign-in' ? 'You’re signed in' : `${agent || 'Your agent'} is in your workspace`;
    view.connected({mode, agent, heading});
    if (mode === 'sign-in') {
      // Signing in loads the page anew through the pairing link, once the
      // page can open the person's own workspace: until then the screen
      // stays, rather than closing on the workspace the tab was let into.
      leaveTimer = later(() => { signedIn = true; openSignedIn(); }, CONNECTED_MS);
      return;
    }
    save({step: 'connected'});
    leaveTimer = later(leave, CONNECTED_MS);
  };
  const drawExpired = () => {
    const at = codes[expiredKind];
    view.expired(modeOf(expiredKind), at.made ? {text: prompts[expiredKind](at.made.code, {site})}
      : {text: null, note: at.error ? setupFailure(at.error, {site}) : codeWait(site)});
    // The fresh prompt is on the screen now, and works however it leaves it.
    watchShown();
  };
  // What the screen shows of `kind`'s code now: its text, and why it is a
  // new one when the last could not be followed (`note`), or why there is
  // none at all.
  const show = kind => {
    const at = codes[kind];
    const text = at.made ? prompts[kind](at.made.code, {site}) : null;
    // A page that did not load is said once, in the setup's box, whose
    // button reloads it: the sign-in beside it waits on the same load.
    const unloaded = !at.make && at.error?.code === 'not-loaded';
    const note = at.note ? {note: at.note} : {};
    // A setup the person has not asked for yet is not being got: the screen
    // waits for them, and makes the code when they ask (`copy`, `manual`).
    const asking = kind !== 'setup' || at.making || Boolean(at.made) || Boolean(at.error)
      || asked || (ahead && Boolean(at.make));
    view.offer(kind, at.made ? {text, code: at.made.code, ...note}
      : unloaded && kind !== 'setup' ? {}
      : at.error ? {problem: setupFailure(at.error, {site}), ...(unloaded ? {reload: true} : {})}
      : asking ? {note: at.note ?? codeWait(site)} : {asking: false});
    if (kind === 'setup' && manualShown) showManual();
    if (expiredKind === kind) drawExpired();
    watchShown();
  };
  // Whether `kind`'s prompt, with its code, is on the screen for a person to
  // take: the start shows both, and a prompt that ran out shows its fresh one.
  const onScreen = kind => !closed && !leaving
    && (state.step === 'start' || (state.step === 'expired' && expiredKind === kind));
  let watchRound = 0;
  let watchTimer = null;
  // When a prompt kept from before a reload was first found to be one this
  // page cannot follow (`KEPT_LOST_MS` says how long that may go unsaid).
  let lostSince = null;
  const stopWatch = () => {
    ++watchRound;
    clear(watchTimer);
    watchTimer = null;
  };
  // A prompt kept from before a reload that this page cannot follow at all
  // (it could not be linked to this tab's pairing again): said in its box,
  // rather than looked at every couple of seconds without a word. The page
  // makes a fresh code in its place once it can, and otherwise the box's
  // button reloads it, which makes one.
  const lostKept = () => {
    lostSince = null;
    const at = codes.setup;
    at.made = null;
    at.note = null;
    if (at.make) {
      at.note = 'This tab couldn’t follow that prompt. Here’s a new one.';
      show('setup');
      void prepare('setup');
      return;
    }
    at.error = {code: 'not-loaded'};
    show('setup');
  };
  // Keep what is on the screen and follow it quietly: the host keeps a code
  // from its tab's first follow, and a code a person copies by hand or types
  // must be one it knows. Until a computer uses it the code is followed at
  // `SHOWN_FOLLOW_MS`; once one does, the screen steps on exactly as it does
  // for a prompt copied with the button, and nothing is said of a copy that
  // did not happen. A kept prompt this page cannot follow at all is said
  // after a while (`lostKept`), never looked at in silence forever.
  const watchShown = () => {
    stopWatch();
    if (closed) return;
    const round = watchRound;
    const look = async () => {
      watchTimer = null;
      if (round !== watchRound || closed) return;
      let showing = false;
      let lost = false;
      for (const kind of Object.keys(codes)) {
        const at = codes[kind];
        // Shown by the manual setup's panel, which follows that same code
        // itself and opens the steps when it is used.
        if (!at.made || !onScreen(kind) || (kind === 'setup' && manualShown)) continue;
        showing = true;
        const made = at.made;
        let polled;
        try {
          polled = await bounded(Promise.resolve().then(() => made.follow()), ASK_MS, later, clear);
        } catch (error) {
          // A page that cannot follow codes yet looks again once it can; a
          // host that did not answer, too (asking for the prompt says so).
          if (round !== watchRound || closed) return;
          if (error?.code === 'not-loaded' && made.early) lost = true;
          continue;
        }
        if (round !== watchRound || closed) return;
        if (at.made !== made || !onScreen(kind)) continue;
        // A code the host forgot is replaced when it is renewed; one nobody
        // used is simply looked at again.
        if (polled?.expired || !polled?.resolved) continue;
        use(kind, modeOf(kind), made, prompts[kind](made.code, {site}),
          {handed: true, agent: polled.agent || null});
      }
      if (lost) {
        lostSince ??= now();
        if (now() - lostSince >= KEPT_LOST_MS) { lostKept(); return; }
      } else lostSince = null;
      if (showing && round === watchRound && !closed) {
        watchTimer = later(() => { void look(); }, SHOWN_FOLLOW_MS);
      }
    };
    void look();
  };
  const renewIn = (kind, made) => {
    clear(codes[kind].renew);
    codes[kind].renew = later(() => { if (kind !== 'setup' || !adoptOffer()) void prepare(kind); },
      Math.max(0, made.expiresAt - now() - RENEW_MS));
  };
  // The setup code kept on the start (by this tab before it loaded again, or
  // by another tab), with time left to use it.
  const keptOffer = () => {
    const kept = readSetup(storage)?.offer;
    return kept && kept.expiresAt - now() > RENEW_MS ? kept : null;
  };
  // A kept code shown and copied before the page can follow codes again:
  // following it waits for the page (`not-loaded`, no failure of the host).
  const waitingFor = saved => ({code: saved.code, expiresAt: saved.expiresAt, saved, early: true, follow: async () => {
    const made = codes.setup.resume?.(saved);
    if (!made) throw Object.assign(new Error('this page cannot follow its code yet'), {code: 'not-loaded'});
    return made.follow();
  }});
  // The setup code another tab keeps on its start, taken in place of this
  // one's: the same prompt shows in both. Whether there was one to take.
  const adoptOffer = () => {
    const kept = keptOffer();
    const at = codes.setup;
    if (closed || !kept || !at.resume || (kept.code === at.made?.code && !at.made.early)) return false;
    const made = at.resume(kept);
    if (!made) return false;
    at.round += 1;
    at.making = false;
    at.error = null;
    at.note = null;
    at.made = made;
    renewIn('setup', made);
    show('setup');
    return true;
  };
  // Make `kind`'s next code, within `MAKE_MS`, and renew it shortly before it
  // expires. A failure is said, and stays until the person tries again.
  const prepare = kind => {
    const at = codes[kind];
    if (closed || !at.make || at.making) return Promise.resolve();
    at.making = true;
    at.error = null;
    clear(at.renew);
    const round = ++at.round;
    if (!at.made) show(kind);
    return bounded(Promise.resolve().then(() => at.make()), MAKE_MS, later, clear).then(made => {
      if (closed || round !== at.round) return;
      at.made = made;
      if (kind === 'setup' && made.saved) save({offer: made.saved});
      // The prompt on screen stays until its replacement is in.
      renewIn(kind, made);
    }, error => {
      if (closed || round !== at.round) return;
      console.warn('first screen: making a code', error);
      at.made = null;
      at.error = error;
    }).finally(() => {
      if (round !== at.round) return;
      at.making = false;
      if (!closed) show(kind);
    });
  };
  // The person asked for `kind`'s code, by copying it or opening the manual
  // setup: made now, or, where the page can make none yet, as soon as it can
  // (`offerSetup`). A code already on the screen is left as it is.
  const askForCode = kind => {
    if (kind === 'setup') asked = true;
    if (!codes[kind].made) void prepare(kind);
  };
  // The page offered no codes in time: its runtime did not load, or it
  // failed in a way it did not say. Said instead of waiting on, but only
  // once the screen is on the page: one still held for the entry's answer is
  // not waiting on a load the person cannot see, and the page usually has
  // its codes by the time the entry lets them in.
  let offerTimer = null;
  const watchOffers = () => {
    if (offerTimer !== null || closed) return;
    offerTimer = later(() => {
      offerTimer = null;
      for (const kind of Object.keys(codes)) {
        const at = codes[kind];
        if (!at.make && !at.error) { at.error = gone ?? {code: 'not-loaded'}; show(kind); }
      }
    }, OFFER_MS);
  };
  // The page offers its codes: it did load, so a wait that said it did not
  // no longer holds.
  const loaded = at => { if (at.error?.code === 'not-loaded') at.error = null; };
  // Follow `made` (`{code, expiresAt, follow()}`): count its time down
  // until a computer resolves it, then show each step it reports. `known`
  // is what was heard of it before this page (kept with the setup);
  // `resumed` says the screen shows what was kept until the host answers.
  const follow = (made, mode, mine, {known = null, resumed = false} = {}) => {
    stop();
    const kind = kindOf(mode);
    let asking = false;
    let resolved = Boolean(known?.resolved);
    let claimed = false;
    let finished = false;
    let heard = known ? JSON.stringify(known) : null;
    let quietSince = now();
    let misses = 0;
    let checking = resumed;
    const expire = () => {
      finished = true;
      stop();
      expiredKind = kind;
      save({step: 'expired'});
      drawExpired();
    };
    // The host forgot the code: it ran out, or its steps did. An agent that
    // logged in is a device of the person by now, and their own workspace
    // was named for it, so that opens; otherwise the code is said to have
    // run out, with a fresh one.
    const forgotten = () => {
      const steps = JSON.parse(heard ?? '{}').steps ?? [];
      if (mode === 'setup' && steps.includes('logged-in')) { finished = true; stop(); leave(); return; }
      expire();
    };
    const tick = async () => {
      if (mine !== attempt || closed || finished) return;
      if (!resolved && now() >= made.expiresAt) { expire(); return; }
      if (!resolved) view.left(minutesLeft(made.expiresAt, now()));
      if (asking) return;
      asking = true;
      let polled;
      try {
        let asked;
        try { asked = made.follow(); } catch (error) { asked = Promise.reject(error); }
        polled = await bounded(asked, ASK_MS, later, clear);
      } catch (error) {
        if (mine !== attempt || closed || finished) return;
        if (error?.status === 404) { forgotten(); return; }
        if (error?.code === 'not-loaded') return;
        console.warn('first screen: follow', error);
        misses += 1;
        if (!online()) view.net('You’re offline.');
        else if (misses >= 2 || error?.code === 'timeout') view.net(`${host} isn’t answering.`);
        return;
      } finally { asking = false; }
      if (mine !== attempt || closed || finished) return;
      if (misses || checking) { misses = 0; checking = false; view.net(null); }
      if (polled.expired) { if (resolved) forgotten(); else expire(); return; }
      if (!polled.resolved) return;
      if (!resolved) view.left(null);
      resolved = true;
      const steps = polled.steps ?? [];
      const seen = JSON.stringify({resolved: true, steps, agent: polled.agent ?? null, detail: polled.detail ?? null,
        ...(polled.claimed ? {claimed: polled.claimed} : {})});
      if (seen !== heard) {
        heard = seen;
        quietSince = now();
        view.net(null);
        save({polled: JSON.parse(seen)});
      }
      const agent = polled.agent || 'Your agent';
      view.steps(stepLines(polled, mode), {agent: polled.agent || null, detail: polled.detail || null});
      const failed = steps.includes('failed') && !steps.includes('entered');
      // A sign-in the agent gave up on is over: the screen says why, once,
      // with one way on, a fresh code.
      if (failed && mode === 'sign-in' && !steps.includes('approved')) {
        finished = true;
        stop();
        save({step: 'start', code: null, polled: null});
        view.failed(polled.detail || `${agent} did not say why.`, mode);
        return;
      }
      view.agentFailed(failed ? (polled.detail || `${agent} did not say why.`) : null);
      if (!failed && mode === 'setup' && !steps.includes('restart') && now() - quietSince >= QUIET_MS) {
        view.net(`${agent} has reported nothing new for ${Math.round(QUIET_MS / 60_000)} minutes. Look at its window: it may be waiting for you.`);
      }
      if (mode === 'sign-in' && polled.claimed && !claimed) {
        claimed = true;
        Promise.resolve().then(() => made.signIn(polled.claimed)).then(() => {
          if (mine !== attempt || closed) return;
          finished = true;
          stop();
          connect(mode, null);
        }, error => {
          if (mine !== attempt || closed) return;
          console.warn('first screen: sign-in', error);
          finished = true;
          stop();
          save({step: 'start', code: null, polled: null});
          view.failed(setupFailure(error, {site}), mode);
        });
      }
      if (mode === 'setup' && steps.includes('entered')) {
        finished = true;
        stop();
        connect(mode, polled.agent || null);
      }
    };
    void tick();
    askNow = () => { void tick(); };
    stop = every(() => { void tick(); }, FOLLOW_MS);
  };
  // Follow the code the setup kept again, once the page can (`resume`).
  const resumeFollow = () => {
    if (closed || leaving || !['waiting', 'expired'].includes(state.step) || !state.code) return;
    const kind = state.code.kind === 'signIn' ? 'signIn' : 'setup';
    if (following || !codes[kind].resume) return;
    const made = codes[kind].resume(state.code);
    if (!made) { toStart(true); return; }
    following = made.code;
    if (state.step === 'expired') return;
    follow(made, modeOf(kind), ++attempt, {known: state.polled ?? null, resumed: true});
  };
  // Back to the start, forgetting the code under way.
  const toStart = keep => {
    // Whether the person had a setup under way: the start then offers the next prompt. A screen
    // newly shown to a first visit (`reveal`) makes nothing until they ask.
    const underWay = asked || state.step === 'waiting' || state.step === 'expired' || Boolean(state.code);
    ++attempt;
    stop();
    following = null;
    expiredKind = null;
    manualShown = false;
    for (const kind of Object.keys(codes)) codes[kind].note = null;
    view.start();
    if (keep) save({step: 'start', code: null, polled: null, copied: false, manual: false});
    if (!adoptOffer()) {
      show('setup');
      // A code the page was making for them after a copy may not have finished before a reload:
      // the start they come back to offers the next one to copy. A tab following another's step
      // takes what that tab made, never a competing code.
      if (keep && underWay) askForCode('setup');
    }
  };
  // Bring the screen to the setup `next` (kept here before, or by another tab).
  const apply = next => {
    if (closed) return;
    state = next.step === 'later' ? onStart(next) : next;
    // Whatever is on the screen now is what is watched, if anything (the
    // branches that draw a prompt start it again).
    stopWatch();
    if (next.step === 'done') { close(); reload(); return; }
    if (next.step === 'opening') {
      if (!leaving) {
        const shown = readShown(storage);
        connectedShown = Boolean(shown?.heading);
        if (connectedShown) frame.heading = shown.heading;
        leave();
      }
      return;
    }
    if (next.step === 'connected') { connect('setup', next.polled?.agent ?? null); return; }
    if (next.step === 'waiting' || next.step === 'expired') {
      if (!next.code) { toStart(true); return; }
      const kind = next.code.kind === 'signIn' ? 'signIn' : 'setup';
      const mode = modeOf(kind);
      const expired = next.step === 'expired' || (!next.polled?.resolved && next.code.expiresAt <= now());
      if (following === next.code.code && (!expired || expiredKind === kind)) return;
      ++attempt;
      stop();
      manualShown = false;
      expiredKind = null;
      following = null;
      view.waiting(mode, {text: prompts[kind](next.code.code, {site}), copied: next.copied === true, manual: next.manual === true,
        ...(kind === 'signIn' ? {command: signInCommand(next.code.code)} : {}),
        ...(next.handed ? {handed: true, agent: next.polled?.agent ?? null} : {})});
      view.steps(stepLines(next.polled ?? null, mode));
      const steps = next.polled?.steps ?? [];
      if (steps.includes('failed') && !steps.includes('entered')) view.agentFailed(next.polled.detail || `${next.polled.agent || 'Your agent'} did not say why.`);
      if (expired) {
        following = next.code.code;
        expiredKind = kind;
        if (next.step !== 'expired') save({step: 'expired'});
        drawExpired();
        return;
      }
      if (!next.polled?.resolved) view.left(minutesLeft(next.code.expiresAt, now()));
      view.net(`Checking with ${host}…`);
      resumeFollow();
      return;
    }
    // The start: another tab went back, or this is where the setup is. Its
    // kept prompt stays on the screen while the page loads.
    if (following || expiredKind) toStart(false);
    else if (!adoptOffer()) {
      const kept = keptOffer();
      if (kept && !codes.setup.made && !codes.setup.resume) codes.setup.made = waitingFor(kept);
      show('setup');
    }
  };
  // The code `kind` shows, taken into use: the screen shows what is happening
  // beside its prompt and follows it, and the page makes the next one for
  // another try. `handed` says an agent got the code without a copy in this
  // tab (typed, or copied by hand), so the screen asks for no paste and says
  // nothing was copied; `agent` is the computer's name, once it said one.
  const use = (kind, mode, made, text, {copied = false, handed = false, agent = null, shown = false} = {}) => {
    const mine = ++attempt;
    stopWatch();
    manualShown = false;
    expiredKind = null;
    lostSince = null;
    codes[kind].note = null;
    view.waiting(mode, {text, copied, ...(kind === 'signIn' ? {command: signInCommand(made.code)} : {}),
      ...(handed ? {handed: true, agent} : {})});
    view.agentFailed(null);
    view.net(null);
    if (!copied && !handed && !shown) view.problem(setupFailure({code: 'copy-failed'}, {site}));
    view.steps(stepLines(null, mode));
    following = made.code;
    save({step: 'waiting', mode, code: made.saved ? {...made.saved, kind} : null, copied, handed, manual: false, polled: null,
      ...(kind === 'setup' ? {offer: null} : {})});
    follow(made, mode, mine);
    codes[kind].made = null;
    void prepare(kind);
  };
  // Copy what `kind` shows, inside the click. Without a code the button
  // stays and says what it waits for (or why there is none, trying again).
  const copy = (kind, mode) => {
    if (closed) return Promise.resolve(false);
    const at = codes[kind];
    if (!at.made) {
      askForCode(kind);
      view.notReady(kind, at.error && !at.make ? setupFailure(at.error, {site}) : codeWait(site));
      return Promise.resolve(false);
    }
    const made = at.made;
    const text = prompts[kind](made.code, {site});
    // Followed before it is handed over: the host keeps a code from its tab's
    // first follow, and while the prompt is on the screen `watchShown` may
    // not have had its answer yet. The clipboard holds this prompt the moment
    // the click returns — an agent handed it that fast is told a code the
    // host does not know yet. What the screen says of the code still waits
    // for the copy's answer, which only the click can give.
    follow(made, mode, ++attempt);
    let writing;
    try { writing = Promise.resolve(clipboard.copy(text)); } catch (error) { writing = Promise.reject(error); }
    return writing.then(() => true, error => { console.warn('first screen: copy', error); return false; }).then(copied => {
      if (closed) return copied;
      use(kind, mode, made, text, {copied});
      return copied;
    });
  };
  const showManual = () => {
    const at = codes.setup;
    if (!at.made) {
      view.commands(null, MCP_URL, at.error ? setupFailure(at.error, {site}) : codeWait(site));
      return;
    }
    view.commands(manualCommands(at.made.code, {site}), MCP_URL);
    // Once a computer resolves it, the screen follows its steps.
    const made = at.made;
    const mine = ++attempt;
    follow({...made, follow: async () => {
      const polled = await made.follow();
      if (polled.resolved && manualShown && mine === attempt) {
        manualShown = false;
        view.manual(false);
        view.waiting('setup', {manual: true});
        following = made.code;
        save({step: 'waiting', mode: 'setup', code: made.saved ? {...made.saved, kind: 'setup'} : null, copied: false,
          manual: true, polled: null, offer: null});
        codes.setup.made = null;
        void prepare('setup');
      }
      return polled;
    }}, 'setup', mine);
  };
  const controller = {
    /** Copy the setup prompt the screen shows. */
    copy() { return copy('setup', 'setup'); },
    /** Copy the prompt that has the person's agent sign this tab in. */
    signInWithAgent() { return copy('signIn', 'sign-in'); },
    /** The person copied the shown setup prompt by hand. */
    copiedByHand() {
      const at = codes.setup;
      if (closed || !at.made) return;
      use('setup', 'setup', at.made, prompts.setup(at.made.code, {site}), {copied: true});
    },
    /** Make the code that could not be made, or copy a fresh one after one
     * expired or failed (a click of its own). */
    retry(mode = 'setup') {
      const kind = kindOf(mode);
      if (closed) return Promise.resolve(false);
      if (!codes[kind].make && codes[kind].error) { reload(); return Promise.resolve(false); }
      if (!codes[kind].made) {
        return prepare(kind).then(() => (codes[kind].made ? copy(kind, mode) : false));
      }
      return copy(kind, mode);
    },
    /** Show (or hide) the manual setup, with the setup code, followed. */
    manual() {
      if (closed) return;
      manualShown = !manualShown;
      view.manual(manualShown);
      if (!manualShown) { ++attempt; stop(); }
      else askForCode('setup');
      // The panel follows the setup code itself; the offer behind it is
      // watched again once the panel is closed.
      watchShown();
      if (manualShown) showManual();
    },
    /** Copy what the screen shows once more (a click of its own). After a
     * prompt ran out, what shows is a fresh one, taken into use. */
    again(text, which = 'prompt') {
      if (expiredKind && which === 'prompt') return copy(expiredKind, modeOf(expiredKind));
      let writing;
      try { writing = Promise.resolve(clipboard.copy(text)); } catch (error) { writing = Promise.reject(error); }
      return writing.then(() => true, () => false)
        .then(ok => { view.copied(ok, which); if (!ok) view.problem(setupFailure({code: 'copy-failed'}, {site})); });
    },
    /** Back to the start, forgetting the code under way; signing in from
     * the wait list, back to it. */
    back() {
      if (closed) return;
      if (!signInOnly) { toStart(true); return; }
      close();
      onBack();
    },
    skip() {
      if (closed) return;
      // A page that stopped has no workspace to open: the screen leaves for
      // what the page says of it underneath, as its sentence promised.
      if (gone?.code === 'stopped') { close(); return; }
      leave();
    },
    /** What leaving opens (the person's own workspace), run at once when
     * the screen is leaving already. `open({signedIn})`: `false` for a
     * screen the person left to stay where they are, `true` for one a
     * sign-in left for another person's workspace, which the loaded page
     * opens itself. */
    whenDone(open) {
      if (closed) return;
      onDone = open;
      openSignedIn();
      openOwn();
    },
    /** Sign in with a passkey: at once once the page offers it, else once it
     * does, for `PASSKEY_WAIT_MS`. What the page's sign-in rejects with is
     * said (`signInFailure`), but for the person closing the prompt. */
    async passkey() {
      if (closed) return;
      view.signingIn(true);
      try {
        const action = passkeyAction ?? await bounded(new Promise((resolve, reject) => {
          if (gone) reject(gone); else passkeyWaiting.push({resolve, reject});
        }), PASSKEY_WAIT_MS, later, clear);
        await action();
      } catch (error) {
        console.warn('first screen: sign-in', error);
        // Closing the passkey prompt is the person's own answer.
        if (error?.code !== 'passkey-cancelled' && !['NotAllowedError', 'AbortError'].includes(error?.name)) view.signInFailed(error);
      } finally { view.signingIn(false); }
    },
    /**
     * The page can make setup codes: `make()` resolves to `{code, expiresAt,
     * follow(), saved}`, `saved` what the setup keeps of it, and
     * `resume(saved)` follows a kept one again (`null` when it is no longer
     * this tab's to follow). A first visit's code names a workspace the
     * server makes when it is asked for, so it is made when the person asks
     * for it (`ahead: false`), never while they are still deciding — or at
     * once, where they asked before the page could make one.
     */
    offerSetup(make, resume = null, {ahead: madeAhead = true} = {}) {
      codes.setup.make = make;
      codes.setup.resume = resume;
      ahead = madeAhead;
      loaded(codes.setup);
      watchOffers();
      if (!adoptOffer()) {
        if (ahead || asked) void prepare('setup');
        else show('setup');
      }
      resumeFollow();
    },
    /** The page can make sign-in codes: as `offerSetup`, each also with
     * `signIn(claimed)`. */
    offerAgentSignIn(make, resume = null) {
      codes.signIn.make = make;
      codes.signIn.resume = resume;
      loaded(codes.signIn);
      watchOffers();
      // Signing in from the wait list, the prompt is what the screen is
      // for: shown as soon as its code is in, to copy or select.
      void prepare('signIn').then(() => {
        const made = codes.signIn.made;
        if (!signInOnly || closed || following || !made) return;
        use('signIn', 'sign-in', made, prompts.signIn(made.code, {site}), {shown: true});
      });
      if (!signInOnly) resumeFollow();
    },
    /** The page cannot make codes: `error` says why (`stopped`: the page
     * cannot go on, and says so itself under this screen; `no-codes`: this
     * tab has nothing to make one with). A screen opening the person's
     * workspace leaves for what the page says. */
    unavailable(error) {
      gone = error;
      for (const kind of Object.keys(codes)) {
        const at = codes[kind];
        if (at.make) continue;
        at.error = error;
        show(kind);
      }
      if (!passkeyAction) for (const {reject} of passkeyWaiting.splice(0)) reject(error);
      if (leaving) close();
    },
    /** The page can sign in with a passkey now. */
    offerSignIn(signIn) {
      passkeyAction = signIn;
      for (const {resolve} of passkeyWaiting.splice(0)) resolve(signIn);
    },
    /** Another tab changed the setup: show what it keeps. */
    sync() {
      const next = readSetup(storage);
      if (!next || closed || sameState(next, state)) return;
      apply(next);
    },
    /** The browser is online again: ask at once, and make what could not be made. */
    online() {
      if (closed) return;
      askNow();
      for (const kind of Object.keys(codes)) if (codes[kind].error && codes[kind].make && !codes[kind].made) void prepare(kind);
    },
    /** Whether the screen waits for the page to say the person may set
     * Tilo up here, rather than being on the screen already. */
    get held() { return heldBack && !closed; },
    /** Show the screen: the page is in, so the setup is the person's. A
     * screen the first paint already showed is left where it is. */
    reveal() {
      if (closed || !heldBack) return;
      heldBack = false;
      watchOffers();
      toStart(true);
    },
    close,
    get closed() { return closed; },
  };
  apply(state);
  // A screen that is on the page now is waiting on its codes: one held for
  // the entry's answer starts that wait when it is shown (`reveal`).
  if (!heldBack) watchOffers();
  return controller;
}

/** Write `text` (the prompt an agent is given) into `element` so it wraps only between its words:
 * each word stands in a span the page keeps whole (`.fr-word`), so a flag or a URL never breaks at
 * its own hyphen or slash ("curl -" / "fsSL"). The element's text is `text` exactly, so what is
 * copied from it is the prompt. */
export function showPrompt(element, text) {
  const document = element.ownerDocument;
  const parts = String(text ?? '').split(/( +)/).filter(Boolean);
  element.replaceChildren(...parts.map(part => {
    if (/^ +$/.test(part)) return document.createTextNode(part);
    const word = document.createElement('span');
    word.className = 'fr-word';
    word.textContent = part;
    return word;
  }));
}

/** The screen in `document` (index.html's `#first-screen`): what each state
 * shows, said once to screen readers through its status line. */
export function domView(document) {
  const root = document.documentElement;
  const $ = id => document.getElementById(id);
  const say = text => { $('first-screen-status').textContent = text; };
  const state = (name, mode) => { root.dataset.firstScreen = name; if (mode) $('first-screen').dataset.mode = mode; };
  const button = (id, ok) => {
    const target = $(id);
    target.textContent = ok ? 'Copied' : 'Copy';
    if (ok) setTimeout(() => { target.textContent = 'Copy'; }, 1500);
  };
  const notes = {setup: 'first-screen-offer-note', signIn: 'first-screen-sign-in-note'};
  return {
    start() {
      state('');
      // The screen is the page's now: nothing is held back any more.
      root.removeAttribute('data-first-screen-hold');
      // The start is the setup: a screen reopened for a person who is already
      // set up shows the setup's column, never the one a past sign-in left.
      $('first-screen').dataset.mode = 'setup';
      this.manual(false);
      $('first-screen').dataset.handed = 'false';
      $('first-screen-heading').focus({preventScroll: true});
    },
    offer(kind, {text = null, note = null, problem = null, reload = false, asking = true}) {
      // Whether a code is ready, for tests and tools driving the page; and
      // for the styles, which the button and the box read: no attribute
      // while the person has not asked for a setup code yet.
      const marks = $('first-screen').dataset;
      const code = kind === 'setup' ? 'setup' : 'signIn';
      if (text) marks[code] = 'ready';
      else if (problem) marks[code] = 'failed';
      else if (asking) marks[code] = 'waiting';
      else delete marks[code];
      if (kind === 'setup') {
        showPrompt($('first-screen-offer'), text);
        $('first-screen-offer-retry').hidden = !problem;
        // Only reloading the page helps a page that did not load.
        $('first-screen-offer-retry').textContent = reload ? 'Reload' : 'Try again';
        // Nothing to copy until there is a prompt again.
        $('first-screen-agent').disabled = Boolean(problem);
      }
      const line = $(notes[kind]);
      line.textContent = problem ?? (kind === 'setup' ? note ?? '' : '');
      line.dataset.problem = String(Boolean(problem));
      if (problem && kind === 'setup') say(problem);
    },
    notReady(kind, text) {
      // Said beside the button that was pressed: on the start, under it;
      // after a prompt expired or failed, where its problem shows.
      $(root.dataset.firstScreen ? 'first-screen-problem' : notes[kind]).textContent = text;
      say(text);
    },
    waiting(mode, {text = null, command = null, copied = false, manual = false, handed = false, agent = null} = {}) {
      state('waiting', mode);
      if (text !== null) showPrompt($('first-screen-prompt'), text);
      // Signing in, the same ticket works typed into a terminal by the person themselves.
      const typed = document.getElementById('first-screen-sign-in-command');
      if (typed && (command !== null || mode !== 'sign-in')) typed.textContent = command ?? '';
      for (const id of ['first-screen-problem', 'first-screen-left', 'first-screen-net']) $(id).textContent = '';
      $('first-screen-steps').replaceChildren();
      $('first-screen').dataset.agentFailed = 'false';
      $('first-screen').dataset.manual = String(manual);
      $('first-screen').dataset.copied = String(copied);
      // An agent took the code without a copy here: the screen says what it
      // is doing, in place of the paste instructions (index.html).
      $('first-screen').dataset.handed = String(handed);
      const who = agent || 'Your agent';
      for (const span of $('first-screen').querySelectorAll('.fr-hand-agent')) span.textContent = who;
      this.copied(false);
      say(manual ? 'Your computer is setting Tilo up…'
        : handed ? `${who} is ${mode === 'sign-in' ? 'signing this tab in' : 'setting up Tilo'}…`
        : copied ? 'Prompt copied. Paste it into your agent.' : 'Copy the prompt, then paste it into your agent.');
      $(handed ? (mode === 'sign-in' ? 'first-screen-heading-hand-sign-in' : 'first-screen-heading-hand') : 'first-screen-heading-wait')
        .focus({preventScroll: true});
    },
    copied(ok, which = 'prompt') {
      if (which === 'prompt' && ok) $('first-screen').dataset.copied = 'true';
      button(which === 'prompt' ? 'first-screen-again' : `first-screen-copy-${which}`, ok);
    },
    left(text) { $('first-screen-left').textContent = text ? `Works once, for ${text}.` : ''; },
    steps(lines) {
      $('first-screen-steps').replaceChildren(...lines.map(({text, done, act, wait, failed}) => {
        const item = document.createElement('li');
        item.className = done ? 'done' : act ? 'act' : wait ? 'wait' : failed ? 'failed' : 'now';
        item.textContent = text;
        return item;
      }));
      const now = lines.at(-1);
      if (now && !now.done && !now.wait) say(now.text);
    },
    // Why the screen has heard nothing new (offline, a host not answering,
    // an agent quiet for long), or `null` once it has.
    net(text) {
      if ($('first-screen-net').textContent === (text ?? '')) return;
      $('first-screen-net').textContent = text ?? '';
      if (text) say(text);
    },
    // The agent reported it failed, and why (`null`: it did not, or no longer).
    agentFailed(detail) {
      const was = $('first-screen').dataset.agentFailed === 'true';
      $('first-screen').dataset.agentFailed = String(Boolean(detail));
      if (detail) { if ($('first-screen-problem').textContent !== detail) { $('first-screen-problem').textContent = detail; say(detail); } }
      else if (was) $('first-screen-problem').textContent = '';
    },
    // The prompt ran out: the fresh one (`text`), or why there is none yet.
    expired(mode, {text = null, note = null} = {}) {
      state('expired', mode);
      showPrompt($('first-screen-prompt'), text);
      $('first-screen-problem').textContent = note ?? '';
      $('first-screen-net').textContent = '';
      $('first-screen').dataset.copied = 'false';
      $('first-screen').dataset.handed = 'false';
      say(text ? 'That prompt ran out before an agent used it. Here is a fresh one: copy it to try again.' : note ?? '');
    },
    problem(text) { $('first-screen-problem').textContent = text; },
    // It did not work: the reason, alone, and one button that makes a fresh code.
    failed(text, mode) {
      state('failed', mode);
      $('first-screen').dataset.handed = 'false';
      $('first-screen-problem').textContent = text;
      say(text);
    },
    connected({mode, heading}) {
      state('connected', mode);
      $('first-screen').dataset.skipped = 'false';
      $('first-screen-connected').textContent = heading;
      $('first-screen-done-lead').textContent = 'Opening your workspace…';
      say(`${heading}. Opening your workspace…`);
    },
    // Opening the person's own workspace: under the agent's news when it is
    // in (`heading`), or alone after Skip; `note` says why it takes long.
    opening({heading = null, note = null} = {}) {
      state('connected');
      $('first-screen').dataset.skipped = String(!heading);
      $('first-screen-connected').textContent = heading ?? 'Opening your workspace';
      $('first-screen-done-lead').textContent = note ?? (heading ? 'Opening your workspace…' : '');
      say(note ?? 'Opening your workspace…');
    },
    manual(shown) {
      $('first-screen').dataset.open = shown ? 'manual' : '';
      $('first-screen-manual-toggle').setAttribute('aria-expanded', String(shown));
    },
    commands(text, mcp, note) {
      $('first-screen-mcp').textContent = mcp;
      $('first-screen-commands').textContent = text ?? '';
      $('first-screen-commands').closest('.fr-well').hidden = !text;
      $('first-screen-manual-left').textContent = note ?? 'The code works once, for 10 minutes, only for this tab.';
    },
    signingIn(busy) {
      for (const id of ['first-screen-passkey', 'first-screen-passkey-sign-in']) document.getElementById(id)?.setAttribute('aria-busy', String(busy));
    },
    signInFailed(error) { say(signInFailure(error)); },
    close() {
      $('first-screen').setAttribute('data-closing', '');
      // The screen fades, then leaves the page: reduced motion skips the fade. It stays in the
      // document, hidden, so the person's card can show it again ("Connect an agent"): the
      // setup is one flow, wherever it is opened. Opened again during the fade (signing in
      // from the wait list the moment it shows), the screen is no longer closing, and the
      // fade's end leaves it on the page.
      const done = () => {
        if ($('first-screen')?.getAttribute('data-closing') === null) return;
        root.removeAttribute('data-first-screen');
        $('first-screen')?.removeAttribute('data-closing');
      };
      if (globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches) done();
      else setTimeout(done, 280);
    },
  };
}

/** The clipboard as the screen uses it: `copy(text)` writes `text` at once,
 * inside the click that asked, and resolves once the clipboard took it. A
 * browser without the clipboard API copies a selection of it instead. */
export function pageClipboard(navigator = globalThis.navigator, document = globalThis.document) {
  return {
    copy(text) {
      const clipboard = navigator?.clipboard;
      if (typeof clipboard?.writeText === 'function') return clipboard.writeText(text);
      return copyBySelection(text, document) ? Promise.resolve() : Promise.reject(new Error('no clipboard'));
    },
  };
}

/** Copy `text` the old way, through a selection in `document`: whether the browser did. */
function copyBySelection(text, document) {
  if (!document?.body || typeof document.execCommand !== 'function') return false;
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.style.cssText = 'position:fixed;top:0;left:0;opacity:0;pointer-events:none';
  document.body.append(area);
  area.select();
  try { return document.execCommand('copy'); } catch { return false; } finally { area.remove(); }
}

/** The screen of this page, when it shows one: `null` otherwise. Its
 * buttons work from here on; the entry page offers setup and sign-in
 * once it can (`offerSetup`, `offerAgentSignIn`, `offerSignIn`).
 *
 * A first visit this browser knows nothing of is held
 * (`data-first-screen-hold`, first-screen.inline.js) until the page says the site
 * let the person in (`reveal`): a site taking people by invitation only may
 * send them to its wait list, and the setup is not what they see first. */
export function startFirstScreen(document = globalThis.document, {storage} = {}) {
  const root = document?.documentElement;
  if (!document?.getElementById?.('first-screen')) return null;
  const held = root?.dataset?.firstScreen === undefined && root?.dataset?.firstScreenHold !== undefined;
  if (root?.dataset?.firstScreen === undefined && !held) return null;
  // This screen is the first visit's, not a setup reopened later.
  document.getElementById('first-screen')?.removeAttribute('data-reopened');
  return wireFirstScreenScreen(document, storage, {held});
}

/**
 * Show the first screen again, once the person is set up: the same flow, in
 * the same place, for a person who wants their agent connected later (their
 * card's "Connect an agent"). The page hands it a code as it did the first
 * time (`offerSetup`); Skip here only closes it, since there is nothing left
 * to open.
 */
export function openFirstScreen(document = globalThis.document, {storage} = {}) {
  const screen = document?.getElementById?.('first-screen');
  if (!screen) return null;
  const root = document.documentElement;
  // Hidden while the fresh screen is written into, so it appears at once and whole.
  root.removeAttribute('data-first-screen');
  screen.removeAttribute('data-closing');
  // The person is set up already: the setup is what this reopens, without the
  // "Returning? Sign in" column they have no use for, and whichever column a
  // past visit left the screen in.
  screen.dataset.reopened = 'true';
  screen.dataset.mode = 'setup';
  // A code an earlier visit left on the start is no code to offer now: it was
  // used with the agent it set up, or ran out. The screen asks the host for a
  // fresh one, as the card's "Connect an agent" says it does.
  const kept = readSetup(storage ?? globalThis.localStorage);
  if (kept?.offer) writeSetup({...kept, offer: null}, storage ?? globalThis.localStorage);
  const controls = wireFirstScreenScreen(document, storage);
  controls?.whenDone(() => {});
  root.setAttribute('data-first-screen', '');
  return controls;
}

/**
 * Signing in from a site's wait list, as someone who has been here: the
 * screen opens on the ways to sign in, side by side: the prompt that has the
 * person's agent sign this tab in (`offerAgentSignIn`, then
 * `signIn(claimed)`), the same ticket as a `tilo` command they run
 * themselves, and, with `passkey`, a passkey (`offerSignIn`). Back closes it
 * onto the wait list again (`onBack`). It keeps nothing with the setup: this browser
 * has none here, and its next visit opens on the wait list as this one did.
 */
export function openSignIn(document = globalThis.document, {onBack = () => {}, passkey = true} = {}) {
  const element = document?.getElementById?.('first-screen');
  if (!element) return null;
  const withPasskey = document.getElementById('first-screen-way-passkey');
  if (withPasskey) withPasskey.hidden = !passkey;
  const root = document.documentElement;
  root.removeAttribute('data-first-screen');
  element.removeAttribute('data-closing');
  element.dataset.reopened = 'true';
  element.dataset.mode = 'sign-in';
  const screen = wireFirstScreenScreen(document, null, {signInOnly: true, onBack});
  screen?.whenDone(() => {});
  root.setAttribute('data-first-screen', '');
  return screen;
}

/** The screen's buttons wired to a controller of its own. A sign-in from the
 * wait list (`signInOnly`) keeps nothing in `storage`. */
function wireFirstScreenScreen(document, storage, {held = false, signInOnly = false, onBack} = {}) {
  const screen = createFirstScreen({view: domView(document), clipboard: pageClipboard(),
    storage: signInOnly ? null : storage ?? globalThis.localStorage, site: globalThis.location?.origin, held, signInOnly, onBack});
  const on = (id, handler) => document.getElementById(id)?.addEventListener('click', handler);
  const text = id => document.getElementById(id).textContent;
  on('first-screen-agent', () => screen.copy());
  on('first-screen-agent-sign-in', () => screen.signInWithAgent());
  on('first-screen-offer-retry', () => screen.retry('setup'));
  on('first-screen-retry', () => screen.retry(document.getElementById('first-screen').dataset.mode));
  on('first-screen-again', () => screen.again(text('first-screen-prompt')));
  on('first-screen-copy-mcp', () => screen.again(text('first-screen-mcp'), 'mcp'));
  on('first-screen-copy-commands', () => screen.again(text('first-screen-commands'), 'commands'));
  on('first-screen-manual-toggle', () => screen.manual());
  on('first-screen-back', () => screen.back());
  on('first-screen-skip', () => screen.skip());
  on('first-screen-passkey', () => screen.passkey());
  on('first-screen-passkey-sign-in', () => screen.passkey());
  on('first-screen-copy-sign-in-command', () => screen.again(text('first-screen-sign-in-command'), 'sign-in-command'));
  // A prompt selected and copied by hand is copied all the same.
  document.addEventListener('copy', () => {
    const offer = document.getElementById('first-screen-offer');
    const selection = document.getSelection?.();
    if (offer?.textContent && selection && !selection.isCollapsed && offer.contains(selection.anchorNode)) screen.copiedByHand();
  });
  // Another tab's step is this one's: the setup is one, whichever tab shows it.
  globalThis.addEventListener?.('storage', event => { if (event.key === SETUP || event.key === null) screen.sync(); });
  globalThis.addEventListener?.('online', () => screen.online());
  // A page back from the back/forward cache missed what other tabs did meanwhile.
  globalThis.addEventListener?.('pageshow', event => { if (event.persisted) screen.sync(); });
  pressEarly(document);
  return screen;
}

/** The button a person pressed before this module could answer
 * (`first-screen.inline.js` kept it), pressed again now: the last one only,
 * since each press replaces what the one before asked for. */
export function pressEarly(document, scope = globalThis) {
  scope.__tiloFirstScreenReady = true;
  const early = scope.__tiloFirstScreenEarly ?? [];
  const last = early.at(-1);
  early.length = 0;
  if (last) document.getElementById(last)?.click?.();
}

/** This page's screen, started once on first import. */
export const firstScreen = startFirstScreen(globalThis.document);
