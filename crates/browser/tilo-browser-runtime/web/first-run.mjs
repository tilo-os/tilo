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

/** Where this browser remembers that it saw the first screen. */
export const SEEN = 'tilo-first-run';
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

/**
 * What the screen lists for where a code is (`polled`, the host's
 * `Polled`): each step done so far, then the one under way, as
 * `{text, done}`; `{text, act: true}` when the person has to do it, and
 * `{text, wait: true}` until the host sees the code used: nothing is under
 * way then, so nothing claims to be. `mode` is `setup` or `sign-in`.
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
  if (!steps.has('logged-in')) return [...lines, {text: `${agent} is logging in as you…`}];
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
  if (code === 'not-loaded') return 'Tilo has not finished loading in this tab. Reload the page to try again.';
  if (code === 'timeout' || code === 'unreachable' || error?.name === 'TypeError') return `Couldn’t reach ${hostOf(site)}. Check your connection, then try again.`;
  return `${hostOf(site)} did not make a code. Try again, or skip for now.`;
}

/** What a button that has no code yet says it waits for. */
export function codeWait(site = HOME_SITE) {
  return `Getting your code from ${hostOf(site)}…`;
}

/** `promise`, or a rejection with code `timeout` once `ms` passed, timed by `later`/`clear`. */
function bounded(promise, ms, later, clear) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = later(() => reject(Object.assign(new Error('no answer in time'), {code: 'timeout'})), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clear(timer));
}

/**
 * The screen's behaviour, over a `view` that draws it (`domView` below, a
 * fake in tests): which state shows, what each button does, the codes made
 * ahead and renewed, a code's steps and its time running out.
 * `clipboard.copy(text)` writes `text` inside the click and resolves once
 * the clipboard took it; `storage` remembers that the screen was seen;
 * `every(run, ms)` ticks; `later(run, ms)` and `clear(timer)` time one
 * thing; `site` is the origin the commands name; `reload()` loads the page
 * again.
 */
export function createFirstRun({view, clipboard, storage = null, now = () => Date.now(), site = HOME_SITE,
  every = (run, ms) => { const timer = setInterval(run, ms); return () => clearInterval(timer); },
  later = (run, ms) => setTimeout(run, ms), clear = timer => clearTimeout(timer),
  reload = () => globalThis.location?.reload()}) {
  // The codes the page makes ahead, per kind: `setup` and `signIn`. `made`
  // is the one a click copies now; `error` says why there is none.
  const prompts = {setup: setupPrompt, signIn: signInPrompt};
  const codes = {};
  for (const kind of ['setup', 'signIn']) codes[kind] = {make: null, made: null, making: false, error: null, renew: null, round: 0};
  let passkeyRun = null;
  const passkeyWaiting = [];
  let gone = null;
  let closed = false;
  let stop = () => {};
  let attempt = 0;
  let manualShown = false;
  // What leaving the screen opens (the person's own workspace), once the
  // page can: `null` until then, when leaving only closes it.
  let onDone = null;
  let leaving = false;
  const remember = () => { try { storage?.setItem(SEEN, '1'); } catch { /* No storage: asked again next time. */ } };
  const close = () => {
    if (closed) return;
    closed = true;
    stop();
    for (const kind of Object.keys(codes)) clear(codes[kind].renew);
    clear(offerTimer);
    remember();
    view.close();
  };
  const leave = () => {
    leaving = true;
    onDone?.();
    close();
  };
  // What the screen shows of `kind`'s code now: its text, or why there is none.
  const show = kind => {
    const at = codes[kind];
    const text = at.made ? prompts[kind](at.made.code, {site}) : null;
    view.offer(kind, at.made ? {text, code: at.made.code}
      : at.error ? {problem: setupFailure(at.error, {site})} : {note: codeWait(site)});
    if (kind === 'setup' && manualShown) showManual();
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
      // The prompt on screen stays until its replacement is in.
      at.renew = later(() => { void prepare(kind); },
        Math.max(0, made.expiresAt - now() - RENEW_MS));
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
  // until a computer resolves it, then show each step it reports.
  const follow = (made, mode, mine) => {
    stop();
    let asking = false;
    let resolved = false;
    let claimed = false;
    let finished = false;
    const tick = async () => {
      if (mine !== attempt || closed || finished) return;
      if (!resolved && now() >= made.expiresAt) { finished = true; stop(); view.expired(mode); return; }
      if (!resolved) view.left(minutesLeft(made.expiresAt, now()));
      if (asking) return;
      asking = true;
      let polled;
      try { polled = await made.follow(); } catch (error) { console.warn('first run: follow', error); return; } finally { asking = false; }
      if (mine !== attempt || closed || finished) return;
      if (polled.expired) { finished = true; stop(); view.expired(mode); return; }
      if (!polled.resolved) return;
      if (!resolved) view.left(null);
      resolved = true;
      view.steps(stepLines(polled, mode), {agent: polled.agent || null, detail: polled.detail || null});
      const steps = polled.steps ?? [];
      if (steps.includes('failed') && polled.detail) view.problem(polled.detail);
      if (mode === 'sign-in' && polled.claimed && !claimed) {
        claimed = true;
        Promise.resolve().then(() => made.signIn(polled.claimed)).then(() => {
          if (mine !== attempt || closed) return;
          finished = true;
          stop();
          view.connected({mode});
          later(leave, CONNECTED_MS);
        }, error => {
          if (mine !== attempt || closed) return;
          console.warn('first run: sign-in', error);
          finished = true;
          stop();
          view.failed(setupFailure(error, {site}), mode);
        });
      }
      if (mode === 'setup' && steps.includes('entered')) {
        finished = true;
        stop();
        view.connected({mode, agent: polled.agent || null});
        later(leave, CONNECTED_MS);
      }
    };
    void tick();
    stop = every(() => { void tick(); }, FOLLOW_MS);
  };
  // The code `kind` shows, taken into use: the screen shows the paste
  // instructions beside its prompt and follows it, and the page makes the
  // next one for another try.
  const use = (kind, mode, made, text, copied) => {
    const mine = ++attempt;
    manualShown = false;
    view.waiting(mode, {text, copied});
    if (!copied) view.problem(setupFailure({code: 'copy-failed'}, {site}));
    view.steps(stepLines(null, mode));
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
      const kind = mode === 'sign-in' ? 'signIn' : 'setup';
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
    /** Copy what the screen shows once more (a click of its own). */
    again(text, which = 'prompt') {
      let writing;
      try { writing = Promise.resolve(clipboard.copy(text)); } catch (error) { writing = Promise.reject(error); }
      return writing.then(() => true, () => false)
        .then(ok => { view.copied(ok, which); if (!ok) view.problem(setupFailure({code: 'copy-failed'}, {site})); });
    },
    /** Back to the start, forgetting the code under way. */
    back() {
      if (closed) return;
      ++attempt;
      stop();
      manualShown = false;
      view.start();
      show('setup');
    },
    skip() {
      if (!closed) leave();
    },
    /** What leaving opens (the person's own workspace), run at once when
     * the screen left before the page could open it. */
    whenDone(open) {
      if (leaving) open();
      else if (!closed) onDone = open;
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
    /** The page can make setup codes: `make()` resolves to `{code, expiresAt, follow()}`. */
    offerSetup(make) { codes.setup.make = make; void prepare('setup'); },
    /** The page can make sign-in codes: `make()` resolves to `{code,
     * expiresAt, follow(), signIn(claimed)}`. */
    offerAgentSignIn(make) { codes.signIn.make = make; void prepare('signIn'); },
    /** The page cannot make codes (it was not let in): `error` says why. */
    unavailable(error) {
      gone = error;
      for (const kind of Object.keys(codes)) {
        const at = codes[kind];
        if (at.make) continue;
        at.error = error;
        show(kind);
      }
      if (!passkeyRun) for (const {reject} of passkeyWaiting.splice(0)) reject(error);
    },
    /** The page can sign in with a passkey now. */
    offerSignIn(run) {
      passkeyRun = run;
      for (const {resolve} of passkeyWaiting.splice(0)) resolve(run);
    },
    close,
    get closed() { return closed; },
  };
  show('setup');
  return controller;
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
    offer(kind, {text = null, note = null, problem = null}) {
      // Whether a code is ready, for tests and tools driving the page.
      $('first-run').dataset[kind === 'setup' ? 'setup' : 'signIn'] = text ? 'ready' : problem ? 'failed' : 'waiting';
      if (kind === 'setup') {
        $('first-run-offer').textContent = text ?? '';
        $('first-run-offer-retry').hidden = !problem;
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
      if (text !== null) $('first-run-prompt').textContent = text;
      for (const id of ['first-run-problem', 'first-run-left']) $(id).textContent = '';
      $('first-run-steps').replaceChildren();
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
      $('first-run-steps').replaceChildren(...lines.map(({text, done, act, wait}) => {
        const item = document.createElement('li');
        item.className = done ? 'done' : act ? 'act' : wait ? 'wait' : 'now';
        item.textContent = text;
        return item;
      }));
      const now = lines.at(-1);
      if (now && !now.done && !now.wait) say(now.text);
    },
    expired(mode) { state('expired', mode); say('The prompt expired. Copy a new one to try again.'); },
    problem(text) { $('first-run-problem').textContent = text; },
    failed(text, mode) { state('failed', mode); $('first-run-problem').textContent = text; say(text); },
    connected({mode, agent}) {
      state('connected', mode);
      const heading = mode === 'sign-in' ? 'You’re signed in' : `${agent || 'Your agent'} is in your workspace`;
      $('first-run-connected').textContent = heading;
      say(`${heading}. Opening your workspace…`);
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
      // The screen fades, then leaves the page; reduced motion skips the fade.
      const done = () => { root.removeAttribute('data-first-run'); $('first-run')?.remove(); };
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
