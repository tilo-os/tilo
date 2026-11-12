/**
 * The first screen of a first visit: set Tilo up with your agent, skip for
 * now, or sign in again.
 *
 * It is plain HTML in the entry page (`#first-run` in index.html), shown
 * from the first paint by `first-run.inline.js`, while the runtime loads
 * behind it. This module only wires it up, and costs nothing until a
 * button is used.
 *
 * Everything an agent does for the person goes through a login code
 * (`K7Q-MX2P`, `tilo-relay-protocol`'s `login_code`): the tab asks its host
 * for one, the agent runs `tilo login <code>`, and the tab follows the code
 * (`follow()`, about once a second), showing each step the computer reports
 * as it happens.
 *
 * The page makes its codes as soon as it can (`offerSetup`,
 * `offerAgentSignIn`), before anyone asks, and keeps a fresh one ready,
 * renewed shortly before it expires. So what a button copies is already on
 * the screen: the click writes exactly that text, inside the click, and the
 * screen says it copied only once the clipboard took it. A code the page has
 * not made yet is never promised: the button says what it waits for, and a
 * wait that runs too long says why, with a way to try again.
 *
 * - **Copy prompt** copies `setupPrompt`, shown above it. The code names a
 *   pairing link to this tab, which holds the person's new root: the
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
 *   asks to be certified as that person (`signIn(claimed)`). **Use a
 *   passkey** asks for one, once the page can (a click before then waits,
 *   for a while).
 */

/**
 * Where this browser keeps the person's setup (`readSetup`): the step the
 * first screen is at, the codes it shows and follows, and what the agent did
 * so far. The page decides from it alone whether a bare entry opens on the
 * first screen, and at which step (first-run.inline.js), so a reload, a
 * closed and reopened tab, back and forward, or a second tab show the same
 * step; another tab's change reaches this one as a `storage` event.
 */
export const SETUP = 'tilo-setup';
/** The steps a setup is kept at: on the start (`start`), following a code
 * (`waiting`), its code ran out (`expired`), the agent is in (`connected`),
 * the person's own workspace opening (`opening`), and over (`done`). */
const SETUP_STEPS = new Set(['start', 'waiting', 'expired', 'connected', 'opening', 'done']);
/** How long the screen says the agent connected before it leaves. */
export const CONNECTED_MS = 1600;
/** How often the tab asks where its code is. */
export const FOLLOW_MS = 1000;
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
/** The site whose commands need no `--site`. */
export const HOME_SITE = 'https://tilo.sh';
/** Tilo's MCP server, for an agent added by hand. */
export const MCP_URL = 'https://mcp.tilo.sh';

/** `site` as the curl command and `tilo login` read it: tilo.sh is the default. */
const siteOf = site => (!site || site === HOME_SITE ? null : site);
/** The command that installs `tilo` from `site`. */
export const installCommand = site => `curl -fsSL ${siteOf(site) ?? 'tilo.sh'} | sh`;
/** `tilo login <code>`, naming `site` unless it is tilo.sh. */
export const loginCommand = (code, site) => `tilo login ${code}${siteOf(site) ? ` --site ${site}` : ''}`;

/** The prompt that has an agent set Tilo up: one line, complete on its own,
 * since `tilo login` tells the agent the rest. */
export function setupPrompt(code, {site} = {}) {
  return `Set up Tilo for me: run ${installCommand(site)} && ${loginCommand(code, site)}, then do what it tells you.`;
}

/** The prompt that has an agent sign this tab in as the person. */
export function signInPrompt(code, {site} = {}) {
  return `Sign me in to Tilo: run ${loginCommand(code, site)}, then do what it tells you.`;
}

/** The commands for someone who sets up by hand, one per line. */
export function manualCommands(code, {site} = {}) {
  return [installCommand(site), loginCommand(code, site), `claude mcp add --transport http tilo ${MCP_URL}`].join('\n');
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

/** What the screen says when the page could not make or use a code, from
 * `error`, on `site`. */
export function setupFailure(error, {site = HOME_SITE} = {}) {
  const code = error?.code;
  if (code === 'no-workspace' || code === 'refused') return 'Tilo could not open a workspace for you here yet. Skip for now to see why.';
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
 * (`shown`), which first-run.inline.js paints at the first frame.
 *
 * `clipboard.copy(text)` writes `text` inside the click and resolves once
 * the clipboard took it; `every(run, ms)` ticks; `later(run, ms)` and
 * `clear(timer)` time one thing; `site` is the origin the commands name;
 * `reload()` loads the page again; `online()` says whether the browser
 * thinks it is online.
 */
export function createFirstRun({view: drawn, clipboard, storage = null, now = () => Date.now(), site = HOME_SITE,
  every = (run, ms) => { const timer = setInterval(run, ms); return () => clearInterval(timer); },
  later = (run, ms) => setTimeout(run, ms), clear = timer => clearTimeout(timer),
  reload = () => globalThis.location?.reload(), online = () => globalThis.navigator?.onLine !== false}) {
  // The codes the page makes ahead, per kind: `setup` and `signIn`. `made`
  // is the one a click copies now; `error` says why there is none;
  // `resume(kept)` follows a code kept with the setup again.
  const prompts = {setup: setupPrompt, signIn: signInPrompt};
  const modeOf = kind => (kind === 'signIn' ? 'sign-in' : 'setup');
  const kindOf = mode => (mode === 'sign-in' ? 'signIn' : 'setup');
  const host = hostOf(site);
  const codes = {};
  for (const kind of ['setup', 'signIn']) codes[kind] = {make: null, resume: null, made: null, making: false, error: null, renew: null, round: 0};
  let state = readSetup(storage) ?? {step: 'start'};
  if (state.step === 'done') state = {step: 'start'};
  let closed = false;
  // What the screen shows now, kept with the setup for the next first paint.
  const frame = {offer: null, prompt: null, lines: null, heading: null, mode: 'setup', copied: false};
  const keepFrame = () => {
    if (closed) return;
    const kept = readSetup(storage);
    if (!kept || kept.step === 'done' || JSON.stringify(kept.shown ?? null) === JSON.stringify(frame)) return;
    writeSetup({...kept, shown: {...frame}}, storage);
  };
  const view = new Proxy(drawn, {get: (target, name) => (...args) => {
    if (name === 'offer' && args[0] === 'setup') frame.offer = args[1]?.text ?? null;
    else if (name === 'waiting') {
      if (typeof args[1]?.text === 'string') frame.prompt = args[1].text;
      frame.mode = args[0];
      frame.copied = args[1]?.copied === true;
    } else if (name === 'expired' && typeof args[1]?.text === 'string') frame.prompt = args[1].text;
    else if (name === 'steps') frame.lines = args[0];
    else if (name === 'connected') frame.heading = args[0]?.heading ?? null;
    else if (name === 'copied' && args[0] && (args[1] ?? 'prompt') === 'prompt') frame.copied = true;
    const result = target[name]?.(...args);
    keepFrame();
    return result;
  }});
  // Keep `patch` as the setup now, over whatever another tab kept since.
  const save = patch => {
    if (closed) return;
    state = {...(readSetup(storage) ?? state), ...patch, shown: {...frame}, at: now()};
    writeSetup(state, storage);
  };
  const sameState = (a, b) => JSON.stringify({...a, shown: null, at: null}) === JSON.stringify({...b, shown: null, at: null});
  let passkeyRun = null;
  const passkeyWaiting = [];
  let gone = null;
  let stop = () => {};
  let askNow = () => {};
  let attempt = 0;
  let manualShown = false;
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
    for (const kind of Object.keys(codes)) clear(codes[kind].renew);
    clear(offerTimer);
    clear(leaveTimer);
    clear(slowTimer);
    closed = true;
    view.close();
  };
  // Open the person's own workspace, once leaving and the page can.
  const openOwn = () => {
    if (!leaving || !onDone || opened || closed) return;
    opened = true;
    Promise.resolve().then(onDone).catch(error => console.warn('first run: opening your workspace', error)).finally(() => {
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
    onDone();
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
  };
  // What the screen shows of `kind`'s code now: its text, or why there is none.
  const show = kind => {
    const at = codes[kind];
    const text = at.made ? prompts[kind](at.made.code, {site}) : null;
    // A page that did not load is said once, in the setup's box, whose
    // button reloads it: the sign-in beside it waits on the same load.
    const unloaded = !at.make && at.error?.code === 'not-loaded';
    view.offer(kind, at.made ? {text, code: at.made.code}
      : unloaded && kind !== 'setup' ? {}
      : at.error ? {problem: setupFailure(at.error, {site}), ...(unloaded ? {reload: true} : {})} : {note: codeWait(site)});
    if (kind === 'setup' && manualShown) showManual();
    if (expiredKind === kind) drawExpired();
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
      console.warn('first run: making a code', error);
      at.made = null;
      at.error = error;
    }).finally(() => {
      if (round !== at.round) return;
      at.making = false;
      if (!closed) show(kind);
    });
  };
  // The page offered no codes in time: its runtime did not load, or it
  // failed in a way it did not say. Said instead of waiting on.
  const offerTimer = later(() => {
    for (const kind of Object.keys(codes)) {
      const at = codes[kind];
      if (!at.make && !at.error) { at.error = gone ?? {code: 'not-loaded'}; show(kind); }
    }
  }, OFFER_MS);
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
        console.warn('first run: follow', error);
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
          console.warn('first run: sign-in', error);
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
    ++attempt;
    stop();
    following = null;
    expiredKind = null;
    manualShown = false;
    view.start();
    if (keep) save({step: 'start', code: null, polled: null, copied: false, manual: false});
    if (!adoptOffer()) show('setup');
  };
  // Bring the screen to the setup `next` (kept here before, or by another tab).
  const apply = next => {
    if (closed) return;
    state = next;
    if (next.step === 'done') { close(); reload(); return; }
    if (next.step === 'opening') {
      if (!leaving) {
        connectedShown = Boolean(next.shown?.heading);
        if (connectedShown) frame.heading = next.shown.heading;
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
      view.waiting(mode, {text: prompts[kind](next.code.code, {site}), copied: next.copied === true, manual: next.manual === true});
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
  // The code `kind` shows, taken into use: the screen shows the paste
  // instructions beside its prompt and follows it, and the page makes the
  // next one for another try.
  const use = (kind, mode, made, text, copied) => {
    const mine = ++attempt;
    manualShown = false;
    expiredKind = null;
    view.waiting(mode, {text, copied});
    view.agentFailed(null);
    view.net(null);
    if (!copied) view.problem(setupFailure({code: 'copy-failed'}, {site}));
    view.steps(stepLines(null, mode));
    following = made.code;
    save({step: 'waiting', mode, code: made.saved ? {...made.saved, kind} : null, copied, manual: false, polled: null,
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
      if (at.error && at.make) void prepare(kind);
      view.notReady(kind, at.error && !at.make ? setupFailure(at.error, {site}) : codeWait(site));
      return Promise.resolve(false);
    }
    const made = at.made;
    const text = prompts[kind](made.code, {site});
    let writing;
    try { writing = Promise.resolve(clipboard.copy(text)); } catch (error) { writing = Promise.reject(error); }
    return writing.then(() => true, error => { console.warn('first run: copy', error); return false; }).then(copied => {
      if (closed) return copied;
      use(kind, mode, made, text, copied);
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
      use('setup', 'setup', at.made, prompts.setup(at.made.code, {site}), true);
    },
    /** Make the code that could not be made, or copy a fresh one after one
     * expired or failed (a click of its own). */
    retry(mode = 'setup') {
      const kind = kindOf(mode);
      if (closed) return Promise.resolve(false);
      if (!codes[kind].make && codes[kind].error) { reload(); return Promise.resolve(false); }
      if (!codes[kind].made && codes[kind].error) return prepare(kind).then(() => false);
      return copy(kind, mode);
    },
    /** Show (or hide) the manual setup, with the setup code, followed. */
    manual() {
      if (closed) return;
      manualShown = !manualShown;
      view.manual(manualShown);
      if (!manualShown) { ++attempt; stop(); return; }
      showManual();
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
    /** Back to the start, forgetting the code under way. */
    back() {
      if (!closed) toStart(true);
    },
    skip() {
      if (!closed) leave();
    },
    /** What leaving opens (the person's own workspace), run at once when
     * the screen is leaving already. */
    whenDone(open) {
      if (closed) return;
      onDone = open;
      openSignedIn();
      openOwn();
    },
    /** Sign in with a passkey: at once once the page offers it, else once it
     * does, for `PASSKEY_WAIT_MS`. */
    async passkey() {
      if (closed) return;
      view.signingIn(true);
      try {
        const run = passkeyRun ?? await bounded(new Promise((resolve, reject) => {
          if (gone) reject(gone); else passkeyWaiting.push({resolve, reject});
        }), PASSKEY_WAIT_MS, later, clear);
        await run();
      } catch (error) { console.warn('first run: sign-in', error); view.signInFailed(); } finally { view.signingIn(false); }
    },
    /** The page can make setup codes: `make()` resolves to `{code, expiresAt,
     * follow(), saved}`, `saved` what the setup keeps of it, and
     * `resume(saved)` follows a kept one again (`null` when it is no longer
     * this tab's to follow). */
    offerSetup(make, resume = null) {
      codes.setup.make = make;
      codes.setup.resume = resume;
      if (!adoptOffer()) void prepare('setup');
      resumeFollow();
    },
    /** The page can make sign-in codes: as `offerSetup`, each also with
     * `signIn(claimed)`. */
    offerAgentSignIn(make, resume = null) {
      codes.signIn.make = make;
      codes.signIn.resume = resume;
      void prepare('signIn');
      resumeFollow();
    },
    /** The page cannot make codes (it was not let in): `error` says why. A
     * screen opening the person's workspace leaves for what the page says. */
    unavailable(error) {
      gone = error;
      for (const kind of Object.keys(codes)) {
        const at = codes[kind];
        if (at.make) continue;
        at.error = error;
        show(kind);
      }
      if (!passkeyRun) for (const {reject} of passkeyWaiting.splice(0)) reject(error);
      if (leaving) close();
    },
    /** The page can sign in with a passkey now. */
    offerSignIn(run) {
      passkeyRun = run;
      for (const {resolve} of passkeyWaiting.splice(0)) resolve(run);
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
    close,
    get closed() { return closed; },
  };
  apply(state);
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

/** The screen in `document` (index.html's `#first-run`): what each state
 * shows, said once to screen readers through its status line. */
export function domView(document) {
  const root = document.documentElement;
  const $ = id => document.getElementById(id);
  const say = text => { $('first-run-status').textContent = text; };
  const state = (name, mode) => { root.dataset.firstRun = name; if (mode) $('first-run').dataset.mode = mode; };
  const button = (id, ok) => {
    const target = $(id);
    target.textContent = ok ? 'Copied' : 'Copy';
    if (ok) setTimeout(() => { target.textContent = 'Copy'; }, 1500);
  };
  const notes = {setup: 'first-run-offer-note', signIn: 'first-run-sign-in-note'};
  return {
    start() { state(''); this.manual(false); $('first-run-heading').focus({preventScroll: true}); },
    offer(kind, {text = null, note = null, problem = null, reload = false}) {
      // Whether a code is ready, for tests and tools driving the page.
      $('first-run').dataset[kind === 'setup' ? 'setup' : 'signIn'] = text ? 'ready' : problem ? 'failed' : 'waiting';
      if (kind === 'setup') {
        showPrompt($('first-run-offer'), text);
        $('first-run-offer-retry').hidden = !problem;
        // Only reloading the page helps a page that did not load.
        $('first-run-offer-retry').textContent = reload ? 'Reload' : 'Try again';
        // Nothing to copy until there is a prompt again.
        $('first-run-agent').disabled = Boolean(problem);
      }
      const line = $(notes[kind]);
      line.textContent = problem ?? (kind === 'setup' ? note ?? '' : '');
      line.dataset.problem = String(Boolean(problem));
      if (problem && kind === 'setup') say(problem);
    },
    notReady(kind, text) {
      // Said beside the button that was pressed: on the start, under it;
      // after a prompt expired or failed, where its problem shows.
      $(root.dataset.firstRun ? 'first-run-problem' : notes[kind]).textContent = text;
      say(text);
    },
    waiting(mode, {text = null, copied = false, manual = false} = {}) {
      state('waiting', mode);
      if (text !== null) showPrompt($('first-run-prompt'), text);
      for (const id of ['first-run-problem', 'first-run-left', 'first-run-net']) $(id).textContent = '';
      $('first-run-steps').replaceChildren();
      $('first-run').dataset.agentFailed = 'false';
      $('first-run').dataset.manual = String(manual);
      $('first-run').dataset.copied = String(copied);
      this.copied(false);
      say(manual ? 'Your computer is setting Tilo up…' : copied ? 'Prompt copied. Paste it into your agent.' : 'Copy the prompt, then paste it into your agent.');
      $('first-run-heading-wait').focus({preventScroll: true});
    },
    copied(ok, which = 'prompt') {
      if (which === 'prompt' && ok) $('first-run').dataset.copied = 'true';
      button(which === 'prompt' ? 'first-run-again' : `first-run-copy-${which}`, ok);
    },
    left(text) { $('first-run-left').textContent = text ? `Works once, for ${text}.` : ''; },
    steps(lines) {
      $('first-run-steps').replaceChildren(...lines.map(({text, done, act, wait, failed}) => {
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
      if ($('first-run-net').textContent === (text ?? '')) return;
      $('first-run-net').textContent = text ?? '';
      if (text) say(text);
    },
    // The agent reported it failed, and why (`null`: it did not, or no longer).
    agentFailed(detail) {
      const was = $('first-run').dataset.agentFailed === 'true';
      $('first-run').dataset.agentFailed = String(Boolean(detail));
      if (detail) { if ($('first-run-problem').textContent !== detail) { $('first-run-problem').textContent = detail; say(detail); } }
      else if (was) $('first-run-problem').textContent = '';
    },
    // The prompt ran out: the fresh one (`text`), or why there is none yet.
    expired(mode, {text = null, note = null} = {}) {
      state('expired', mode);
      showPrompt($('first-run-prompt'), text);
      $('first-run-problem').textContent = note ?? '';
      $('first-run-net').textContent = '';
      $('first-run').dataset.copied = 'false';
      say(text ? 'That prompt ran out before an agent used it. Here is a fresh one: copy it to try again.' : note ?? '');
    },
    problem(text) { $('first-run-problem').textContent = text; },
    failed(text, mode) { state('failed', mode); $('first-run-problem').textContent = text; say(text); },
    connected({mode, heading}) {
      state('connected', mode);
      $('first-run').dataset.skipped = 'false';
      $('first-run-connected').textContent = heading;
      $('first-run-done-lead').textContent = 'Opening your workspace…';
      say(`${heading}. Opening your workspace…`);
    },
    // Opening the person's own workspace: under the agent's news when it is
    // in (`heading`), or alone after Skip; `note` says why it takes long.
    opening({heading = null, note = null} = {}) {
      state('connected');
      $('first-run').dataset.skipped = String(!heading);
      $('first-run-connected').textContent = heading ?? 'Opening your workspace';
      $('first-run-done-lead').textContent = note ?? (heading ? 'Opening your workspace…' : '');
      say(note ?? 'Opening your workspace…');
    },
    manual(shown) {
      $('first-run').dataset.open = shown ? 'manual' : '';
      $('first-run-manual-toggle').setAttribute('aria-expanded', String(shown));
    },
    commands(text, mcp, note) {
      $('first-run-mcp').textContent = mcp;
      $('first-run-commands').textContent = text ?? '';
      $('first-run-commands').closest('.fr-well').hidden = !text;
      $('first-run-manual-left').textContent = note ?? 'The code works once, for 10 minutes, only for this tab.';
    },
    signingIn(busy) { $('first-run-passkey').setAttribute('aria-busy', String(busy)); },
    signInFailed() { say('That passkey did not sign you in. Try another, or sign in with your agent.'); },
    close() {
      $('first-run').dataset.closing = '';
      // The screen fades, then leaves the page: reduced motion skips the fade. It stays in the
      // document, hidden, so the person's card can show it again ("Connect an agent"): the
      // setup is one flow, wherever it is opened.
      const done = () => {
        root.removeAttribute('data-first-run');
        $('first-run')?.removeAttribute('data-closing');
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
 * once it can (`offerSetup`, `offerAgentSignIn`, `offerSignIn`). */
export function startFirstRun(document = globalThis.document, {storage} = {}) {
  if (document?.documentElement?.dataset?.firstRun === undefined || !document.getElementById?.('first-run')) return null;
  return wireFirstRunScreen(document, storage);
}

/**
 * Show the first screen again, once the person is set up: the same flow, in
 * the same place, for a person who wants their agent connected later (their
 * card's "Connect an agent"). The page hands it a code as it did the first
 * time (`offerSetup`); Skip here only closes it, since there is nothing left
 * to open.
 */
export function openFirstRun(document = globalThis.document, {storage} = {}) {
  const screen = document?.getElementById?.('first-run');
  if (!screen) return null;
  const root = document.documentElement;
  // Hidden while the fresh screen is written into, so it appears at once and whole.
  root.removeAttribute('data-first-run');
  screen.removeAttribute('data-closing');
  const run = wireFirstRunScreen(document, storage);
  run?.whenDone(() => {});
  root.setAttribute('data-first-run', '');
  return run;
}

/** The screen's buttons wired to a controller of its own. */
function wireFirstRunScreen(document, storage) {
  const run = createFirstRun({view: domView(document), clipboard: pageClipboard(), storage: storage ?? globalThis.localStorage,
    site: globalThis.location?.origin});
  const on = (id, handler) => document.getElementById(id)?.addEventListener('click', handler);
  const text = id => document.getElementById(id).textContent;
  on('first-run-agent', () => run.copy());
  on('first-run-agent-sign-in', () => run.signInWithAgent());
  on('first-run-offer-retry', () => run.retry('setup'));
  on('first-run-retry', () => run.retry(document.getElementById('first-run').dataset.mode));
  on('first-run-again', () => run.again(text('first-run-prompt')));
  on('first-run-copy-mcp', () => run.again(text('first-run-mcp'), 'mcp'));
  on('first-run-copy-commands', () => run.again(text('first-run-commands'), 'commands'));
  on('first-run-manual-toggle', () => run.manual());
  on('first-run-back', () => run.back());
  for (const id of ['first-run-skip', 'first-run-skip-wait']) on(id, () => run.skip());
  on('first-run-passkey', () => run.passkey());
  // A prompt selected and copied by hand is copied all the same.
  document.addEventListener('copy', () => {
    const offer = document.getElementById('first-run-offer');
    const selection = document.getSelection?.();
    if (offer?.textContent && selection && !selection.isCollapsed && offer.contains(selection.anchorNode)) run.copiedByHand();
  });
  // Another tab's step is this one's: the setup is one, whichever tab shows it.
  globalThis.addEventListener?.('storage', event => { if (event.key === SETUP || event.key === null) run.sync(); });
  globalThis.addEventListener?.('online', () => run.online());
  // A page back from the back/forward cache missed what other tabs did meanwhile.
  globalThis.addEventListener?.('pageshow', event => { if (event.persisted) run.sync(); });
  pressEarly(document);
  return run;
}

/** The button a person pressed before this module could answer
 * (`first-run.inline.js` kept it), pressed again now: the last one only,
 * since each press replaces what the one before asked for. */
export function pressEarly(document, scope = globalThis) {
  scope.__tiloFirstRunReady = true;
  const early = scope.__tiloFirstRunEarly ?? [];
  const last = early.at(-1);
  early.length = 0;
  if (last) document.getElementById(last)?.click?.();
}

/** This page's screen, started once on first import. */
export const firstRun = startFirstRun(globalThis.document);
