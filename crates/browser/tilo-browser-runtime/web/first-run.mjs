/**
 * The first screen of a first visit: set Tilo up with your agent, skip for
 * now, or sign in again.
 *
 * It is plain HTML in the entry page (`#first-run` in index.html), shown
 * from the first paint by `first-run.inline.js`, while the runtime loads
 * behind it. This module only wires it up, and costs nothing until a
 * button is used.
 *
 * - **Set up with your agent** copies a prompt for Claude Code, Codex or
 *   any agent (`agentPrompt`). It names a pairing link to this tab: the
 *   agent installs the `tilo` command, which becomes a device of the
 *   person with a key of its own, certified by the root this tab holds
 *   (the root never leaves). The link works once, for a short time, and
 *   only this tab approves it (`identity-actions.mjs` `connectAgent`). The
 *   copy happens inside the click, as the clipboard asks, even while the
 *   page is still loading: the clipboard takes the prompt as a promise and
 *   writes it once the page made the link.
 * - **Skip for now** opens the person's own workspace (`whenSkipped`): the
 *   page joined the site's home workspace behind the screen, so they are in
 *   it, but it is not what they see first.
 * - **Sign in** (for someone who has been here) uses a passkey, once the
 *   page can ask for one.
 *
 * Once the agent signed in, the screen says so and leaves the page to the
 * workspace, where the agent shows as present with what it does.
 */

/** Where this browser remembers that it saw the first screen. */
export const SEEN = 'tilo-first-run';
/** How long the screen says the agent connected before it leaves. */
export const CONNECTED_MS = 1600;
/** Where the `tilo` command is installed from. */
export const INSTALL = 'curl -fsSL https://tilo.sh | sh';

/**
 * The prompt the person pastes into an agent: short, and complete without
 * anything else, since the agent knows nothing of Tilo. `link` is the
 * one-time pairing link to this tab; `minutes` how long it works.
 */
export function agentPrompt(link, {minutes = 10} = {}) {
  return [
    'Set up Tilo for me (https://tilo.sh). Do each step yourself, then tell me what you did.',
    '',
    `1. Install the Tilo command: ${INSTALL}`,
    `2. Log in as me with the code from my browser tab. It works once, for ${minutes} minutes, and only for that tab:`,
    `   tilo login '${link}' --agent "<your name, e.g. Claude Code>"`,
    '3. Add Tilo\'s MCP server to yourself, named tilo: run `tilo agent connect claude` in Claude Code, '
      + '`tilo agent connect codex` in Codex; any other agent adds the stdio server `tilo mcp` as tilo.',
    '4. Check that you can act: call the tilo_status tool (if your tools load only after a restart, tell me to restart you), '
      + 'then tell me what you can do in Tilo.',
  ].join('\n');
}

/** "9:41", the time left until `expiresAt` (ms) at `now`, never below 0:00. */
export function timeLeft(expiresAt, now) {
  const seconds = Math.max(0, Math.ceil((expiresAt - now) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
}

/** What the screen says when the page could not make a link, from `error`. */
export function setupFailure(error) {
  const code = error?.code;
  if (code === 'no-workspace' || code === 'refused') return 'Tilo could not open a workspace for you here yet. Skip for now to see why.';
  if (code === 'copy-failed') return 'Your browser did not let Tilo copy the prompt. Select it below and copy it yourself.';
  return 'Something went wrong preparing the prompt. Try again, or skip for now.';
}

/**
 * The screen's behaviour, over a `view` that draws it (`domView` below, a
 * fake in tests): which state shows, what each button does, and the link's
 * time running out. `clipboard.copy(promiseOfText)` copies inside the click;
 * `storage` remembers that the screen was seen; `every(run, ms)` ticks.
 */
export function createFirstRun({view, clipboard, storage = null, now = () => Date.now(),
  every = (run, ms) => { const timer = setInterval(run, ms); return () => clearInterval(timer); },
  later = (run, ms) => setTimeout(run, ms)}) {
  let setup = null;
  let gone = null;
  let signIn = null;
  const waiting = [];
  let closed = false;
  let stopTicking = () => {};
  let attempt = 0;
  // What Skip for now opens, once the page can: `null` until then, when
  // Skip only leaves the screen.
  let onSkip = null;
  let skipped = false;
  const remember = () => { try { storage?.setItem(SEEN, '1'); } catch { /* No storage: asked again next time. */ } };
  const close = () => {
    if (closed) return;
    closed = true;
    stopTicking();
    remember();
    view.close();
  };
  // The page's setup, once it can make links: a click before then waits,
  // unless the page already said it cannot make any.
  const ready = () => {
    if (setup) return Promise.resolve(setup);
    if (gone) return Promise.reject(gone);
    return new Promise((resolve, reject) => waiting.push({resolve, reject}));
  };
  const controller = {
    /** The person asked their agent to set Tilo up: copy now, link later. */
    copy() {
      if (closed) return;
      const mine = ++attempt;
      stopTicking();
      view.waiting();
      const made = ready().then(make => make());
      const text = made.then(({link}) => agentPrompt(link));
      const copied = Promise.resolve().then(() => clipboard.copy(text)).then(() => true, () => false);
      return Promise.all([made, text, copied]).then(([{expiresAt}, prompt, ok]) => {
        if (mine !== attempt || closed) return;
        view.prompt(prompt, {copied: ok});
        if (!ok) view.problem(setupFailure({code: 'copy-failed'}));
        const tick = () => {
          if (now() >= expiresAt) { stopTicking(); view.expired(); return; }
          view.left(timeLeft(expiresAt, now()));
        };
        tick();
        stopTicking = every(tick, 1000);
      }, error => {
        if (mine !== attempt || closed) return;
        console.warn('first run:', error);
        view.failed(setupFailure(error));
      });
    },
    /** Copy the prompt the screen shows once more (a click of its own). */
    again(prompt) {
      return Promise.resolve().then(() => clipboard.copy(Promise.resolve(prompt))).then(() => true, () => false)
        .then(ok => { view.copied(ok); if (!ok) view.problem(setupFailure({code: 'copy-failed'})); });
    },
    skip() {
      if (closed) return;
      skipped = true;
      onSkip?.();
      close();
    },
    /** What Skip for now opens (the person's own workspace), run at once
     * when the person skipped before the page could open it. */
    whenSkipped(open) {
      if (skipped) open();
      else if (!closed) onSkip = open;
    },
    /** Sign in with a passkey, once the page offered it. */
    async passkey() {
      if (!signIn) return;
      view.signingIn(true);
      try { await signIn(); } catch (error) { console.warn('first run: sign-in', error); view.signInFailed(); } finally { view.signingIn(false); }
    },
    /** The page can make agent links now: `make()` resolves to `{link, expiresAt}`. */
    offerSetup(make) {
      setup = make;
      for (const {resolve} of waiting.splice(0)) resolve(make);
    },
    /** The page cannot make agent links (it was not let in): `error` says why. */
    unavailable(error) {
      if (setup) return;
      gone = error;
      for (const {reject} of waiting.splice(0)) reject(error);
    },
    /** The page can sign in with a passkey now. */
    offerSignIn(run) {
      signIn = run;
      view.signInReady(true);
    },
    /** The agent signed in as the person (`label`: its device's name). */
    connected(label) {
      if (closed) return;
      stopTicking();
      view.connected(label);
      later(close, CONNECTED_MS);
    },
    close,
    get closed() { return closed; },
  };
  return controller;
}

/** The screen in `document` (index.html's `#first-run`): what each state
 * shows, said once to screen readers through its status line. */
export function domView(document) {
  const root = document.documentElement;
  const $ = id => document.getElementById(id);
  const status = $('first-run-status');
  const say = text => { status.textContent = text; };
  const state = name => { root.dataset.firstRun = name; };
  return {
    waiting() {
      state('waiting');
      $('first-run-prompt').textContent = '';
      $('first-run-problem').textContent = '';
      $('first-run-left').textContent = '';
      say('Preparing your prompt…');
    },
    prompt(text, {copied}) {
      $('first-run-prompt').textContent = text;
      this.copied(copied);
      if (copied) say('Prompt copied. Paste it into your agent. Waiting for it to log in…');
      $('first-run-heading-wait').focus({preventScroll: true});
    },
    copied(ok) { $('first-run-again').textContent = ok ? 'Copied' : 'Copy'; if (ok) setTimeout(() => { $('first-run-again').textContent = 'Copy again'; }, 1500); },
    left(text) { $('first-run-left').textContent = `Works once, for ${text} more, only in this tab.`; },
    expired() { state('expired'); say('The prompt expired. Copy a new one to try again.'); },
    problem(text) { $('first-run-problem').textContent = text; },
    failed(text) { state('failed'); $('first-run-problem').textContent = text; say(text); },
    connected(label) {
      state('connected');
      const name = label || 'Your agent';
      $('first-run-connected').textContent = `${name} is connected`;
      say(`${name} logged in as you. Opening your workspace…`);
    },
    signInReady(ready) { $('first-run-passkey').disabled = !ready; },
    signingIn(busy) { $('first-run-passkey').setAttribute('aria-busy', String(busy)); },
    signInFailed() { say('That passkey did not sign you in. Try another, or set up with your agent.'); },
    close() {
      state('closing');
      // The screen fades, then leaves the page; reduced motion skips the fade.
      const done = () => { root.removeAttribute('data-first-run'); $('first-run')?.remove(); };
      if (globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches) done();
      else setTimeout(done, 280);
    },
  };
}

/** The clipboard as the screen uses it: `text` is a promise, written inside
 * the click (`ClipboardItem` takes it as one where the browser can), else
 * written once it resolves. */
export function pageClipboard(navigator = globalThis.navigator, Item = globalThis.ClipboardItem) {
  return {
    copy(text) {
      const clipboard = navigator?.clipboard;
      if (clipboard?.write && typeof Item === 'function') {
        try {
          return clipboard.write([new Item({'text/plain': Promise.resolve(text).then(value => new Blob([value], {type: 'text/plain'}))})]);
        } catch { /* An older ClipboardItem without promises: write the text once it is made. */ }
      }
      if (!clipboard?.writeText) return Promise.reject(new Error('no clipboard'));
      return Promise.resolve(text).then(value => clipboard.writeText(value));
    },
  };
}

/** The screen of this page, when it shows one: `null` otherwise. Its
 * buttons work from here on; the entry page offers setup and sign-in
 * once it can (`offerSetup`, `offerSignIn`). */
export function startFirstRun(document = globalThis.document, {storage} = {}) {
  if (document?.documentElement?.dataset?.firstRun === undefined || !document.getElementById?.('first-run')) return null;
  const run = createFirstRun({view: domView(document), clipboard: pageClipboard(), storage: storage ?? globalThis.localStorage});
  const on = (id, handler) => document.getElementById(id)?.addEventListener('click', handler);
  on('first-run-agent', () => run.copy());
  on('first-run-retry', () => run.copy());
  on('first-run-again', () => run.again(document.getElementById('first-run-prompt').textContent));
  for (const id of ['first-run-skip', 'first-run-skip-wait']) on(id, () => run.skip());
  on('first-run-passkey', () => run.passkey());
  return run;
}

/** This page's screen, started once on first import. */
export const firstRun = startFirstRun(globalThis.document);
