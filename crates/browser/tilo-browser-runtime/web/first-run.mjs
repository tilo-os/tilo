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
 * - **Set up with your agent** copies `setupPrompt`. The code names a
 *   pairing link to this tab, which holds the person's new root: the
 *   agent's `tilo` command becomes a device of the person with a key of its
 *   own, certified here (the root never leaves). The copy happens inside
 *   the click, as the clipboard asks, even while the page is still loading:
 *   the clipboard takes the prompt as a promise and writes it once the page
 *   made the code. Once the agent is in the person's workspace the screen
 *   says so and leaves (`whenDone` opens that workspace).
 * - **Set up manually** shows the MCP server's address and the commands to
 *   run by hand, with a fresh code this tab follows the same way.
 * - **Skip for now** opens the person's own workspace (`whenDone`): the
 *   page joined the site's home workspace behind the screen, so they are in
 *   it, but it is not what they see first.
 * - **Returning?** signs in as someone who has been here: **Sign in with
 *   your agent** copies `signInPrompt`, whose code names nothing; a
 *   computer that is already a device of the person claims it, and the page
 *   asks to be certified as that person (`signIn(claimed)`). **Use a
 *   passkey** asks for one, once the page can (a click before then waits).
 */

/** Where this browser remembers that it saw the first screen. */
export const SEEN = 'tilo-first-run';
/** How long the screen says the agent connected before it leaves. */
export const CONNECTED_MS = 1600;
/** How often the tab asks where its code is. */
export const FOLLOW_MS = 1000;
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

/** "9:41", the time left until `expiresAt` (ms) at `now`, never below 0:00. */
export function timeLeft(expiresAt, now) {
  const seconds = Math.max(0, Math.ceil((expiresAt - now) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
}

/**
 * What the screen lists for where a code is (`polled`, the host's
 * `Polled`): each step done so far, then the one under way, as
 * `{text, done}`; `{text, act: true}` when the person has to do it. `mode`
 * is `setup` or `sign-in`.
 */
export function stepLines(polled, mode = 'setup') {
  const agent = polled?.agent || 'Your agent';
  const steps = new Set(polled?.steps ?? []);
  const lines = [];
  const done = text => lines.push({text, done: true});
  if (mode === 'sign-in') {
    if (!polled?.resolved) return [{text: 'Waiting for your agent…'}];
    if (!polled.claimed) return [{text: `${agent} is finding you…`}];
    done(`${polled.claimed.by || agent} vouched for you`);
    lines.push(steps.has('approved') ? {text: 'This tab is signed in', done: true} : {text: 'Signing this tab in…'});
    return lines;
  }
  if (!polled?.resolved) return [{text: 'Your agent is installing Tilo…'}];
  done('Tilo is installed');
  if (!steps.has('logged-in')) return [...lines, {text: `${agent} is logging in as you…`}];
  done(`${agent} logged in as you`);
  if (!steps.has('tools-added')) return [...lines, {text: `Adding Tilo’s tools to ${agent}…`}];
  done(`Tilo’s tools are added to ${agent}`);
  if (steps.has('entered')) {
    if (steps.has('restart')) done(`${agent} restarted`);
    done(`${agent} is in your workspace`);
    return lines;
  }
  if (steps.has('restart')) return [...lines, {text: `Restart ${agent}, then it continues`, act: true}];
  return [...lines, {text: `${agent} is entering your workspace…`}];
}

/** What the screen says when the page could not make a code, from `error`. */
export function setupFailure(error) {
  const code = error?.code;
  if (code === 'no-workspace' || code === 'refused') return 'Tilo could not open a workspace for you here yet. Skip for now to see why.';
  if (code === 'copy-failed') return 'Your browser did not let Tilo copy the prompt. Select it below and copy it yourself.';
  if (code === 'not-a-device') return 'That computer is not signed in to Tilo as you. Try your agent on one that is, or use a passkey.';
  return 'Something went wrong preparing the prompt. Try again, or skip for now.';
}

/**
 * The screen's behaviour, over a `view` that draws it (`domView` below, a
 * fake in tests): which state shows, what each button does, the code's
 * steps and its time running out. `clipboard.copy(promiseOfText)` copies
 * inside the click; `storage` remembers that the screen was seen;
 * `every(run, ms)` ticks; `site` is the origin the commands name.
 */
export function createFirstRun({view, clipboard, storage = null, now = () => Date.now(), site = HOME_SITE,
  every = (run, ms) => { const timer = setInterval(run, ms); return () => clearInterval(timer); },
  later = (run, ms) => setTimeout(run, ms)}) {
  // What the page offers, once it can: `setup` and `signIn` make codes,
  // `passkey` signs in. A click before then waits for it, unless the page
  // already said it cannot (`gone`).
  const offers = {setup: null, signIn: null, passkey: null};
  const waiting = {setup: [], signIn: [], passkey: []};
  let gone = null;
  let closed = false;
  let stop = () => {};
  let attempt = 0;
  // What leaving the screen opens (the person's own workspace), once the
  // page can: `null` until then, when leaving only closes it.
  let onDone = null;
  let leaving = false;
  const remember = () => { try { storage?.setItem(SEEN, '1'); } catch { /* No storage: asked again next time. */ } };
  const close = () => {
    if (closed) return;
    closed = true;
    stop();
    remember();
    view.close();
  };
  const leave = () => {
    leaving = true;
    onDone?.();
    close();
  };
  const ready = kind => {
    if (offers[kind]) return Promise.resolve(offers[kind]);
    if (gone) return Promise.reject(gone);
    return new Promise((resolve, reject) => waiting[kind].push({resolve, reject}));
  };
  const offer = (kind, run) => {
    offers[kind] = run;
    for (const {resolve} of waiting[kind].splice(0)) resolve(run);
  };
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
      if (!resolved) view.left(timeLeft(made.expiresAt, now()));
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
          view.failed(setupFailure(error), mode);
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
  // Copy a prompt for a fresh code, made by the page's `kind` offer.
  const start = (kind, mode, prompt) => {
    if (closed) return;
    const mine = ++attempt;
    stop();
    view.waiting(mode);
    const made = ready(kind).then(make => make());
    const text = made.then(({code}) => prompt(code, {site}));
    const copied = Promise.resolve().then(() => clipboard.copy(text)).then(() => true, () => false);
    return Promise.all([made, text, copied]).then(([code, shown, ok]) => {
      if (mine !== attempt || closed) return;
      view.prompt(shown, {copied: ok});
      if (!ok) view.problem(setupFailure({code: 'copy-failed'}));
      view.steps(stepLines(null, mode));
      follow(code, mode, mine);
    }, error => {
      if (mine !== attempt || closed) return;
      console.warn('first run:', error);
      view.failed(setupFailure(error), mode);
    });
  };
  let manualShown = false;
  const controller = {
    /** The person asked their agent to set Tilo up: copy now, code later. */
    copy() { return start('setup', 'setup', setupPrompt); },
    /** The person asked their agent to sign this tab in. */
    signInWithAgent() { return start('signIn', 'sign-in', signInPrompt); },
    /** Show (or hide) the manual setup, with a fresh code it follows. */
    manual() {
      if (closed) return;
      manualShown = !manualShown;
      view.manual(manualShown);
      if (!manualShown) { ++attempt; stop(); return; }
      const mine = ++attempt;
      return ready('setup').then(make => make()).then(made => {
        if (mine !== attempt || closed) return;
        view.commands(manualCommands(made.code, {site}), MCP_URL);
        // Once a computer resolves it, the screen follows its steps.
        const followed = {...made, follow: async () => {
          const polled = await made.follow();
          if (polled.resolved && manualShown) { manualShown = false; view.manual(false); view.waiting('setup', {manual: true}); }
          return polled;
        }};
        follow(followed, 'setup', mine);
      }, error => {
        if (mine !== attempt || closed) return;
        view.commands(null, MCP_URL, setupFailure(error));
      });
    },
    /** Copy what the screen shows once more (a click of its own). */
    again(text, which = 'prompt') {
      return Promise.resolve().then(() => clipboard.copy(Promise.resolve(text))).then(() => true, () => false)
        .then(ok => { view.copied(ok, which); if (!ok) view.problem(setupFailure({code: 'copy-failed'})); });
    },
    /** Back to the start, forgetting the code under way. */
    back() {
      if (closed) return;
      ++attempt;
      stop();
      manualShown = false;
      view.start();
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
    /** Sign in with a passkey: at once once the page offers it, else once it does. */
    async passkey() {
      if (closed) return;
      view.signingIn(true);
      try {
        const run = await ready('passkey');
        await run();
      } catch (error) { console.warn('first run: sign-in', error); view.signInFailed(); } finally { view.signingIn(false); }
    },
    /** The page can make setup codes: `make()` resolves to `{code, expiresAt, follow()}`. */
    offerSetup(make) { offer('setup', make); },
    /** The page can make sign-in codes: `make()` resolves to `{code,
     * expiresAt, follow(), signIn(claimed)}`. */
    offerAgentSignIn(make) { offer('signIn', make); },
    /** The page cannot make codes (it was not let in): `error` says why. */
    unavailable(error) {
      gone = error;
      for (const kind of ['setup', 'signIn', 'passkey']) {
        if (offers[kind]) continue;
        for (const {reject} of waiting[kind].splice(0)) reject(error);
      }
    },
    /** The page can sign in with a passkey now. */
    offerSignIn(run) { offer('passkey', run); },
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
  const say = text => { $('first-run-status').textContent = text; };
  const state = (name, mode) => { root.dataset.firstRun = name; if (mode) $('first-run').dataset.mode = mode; };
  const button = (id, ok) => {
    const target = $(id);
    target.textContent = ok ? 'Copied' : 'Copy';
    if (ok) setTimeout(() => { target.textContent = 'Copy'; }, 1500);
  };
  return {
    start() { state(''); this.manual(false); $('first-run-heading').focus({preventScroll: true}); },
    waiting(mode, {manual = false} = {}) {
      state('waiting', mode);
      if (!manual) $('first-run-prompt').textContent = '';
      for (const id of ['first-run-problem', 'first-run-left']) $(id).textContent = '';
      $('first-run-steps').replaceChildren();
      $('first-run').dataset.manual = String(manual);
      say(manual ? 'Your computer is setting Tilo up…' : 'Preparing your prompt…');
    },
    prompt(text, {copied}) {
      $('first-run-prompt').textContent = text;
      this.copied(copied);
      if (copied) say('Prompt copied. Paste it into your agent.');
      $('first-run-heading-wait').focus({preventScroll: true});
    },
    copied(ok, which = 'prompt') { button(which === 'prompt' ? 'first-run-again' : `first-run-copy-${which}`, ok); },
    left(text) { $('first-run-left').textContent = text ? `Works once, for ${text} more, only in this tab.` : ''; },
    steps(lines) {
      $('first-run-steps').replaceChildren(...lines.map(({text, done, act}) => {
        const item = document.createElement('li');
        item.className = done ? 'done' : act ? 'act' : 'now';
        item.textContent = text;
        return item;
      }));
      const now = lines.at(-1);
      if (now && !now.done) say(now.text);
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
      if (shown) { $('first-run-commands').textContent = ''; $('first-run-manual-left').textContent = ''; }
    },
    commands(text, mcp, problem) {
      $('first-run-mcp').textContent = mcp;
      $('first-run-commands').textContent = text ?? '';
      $('first-run-manual-left').textContent = problem ?? 'The code works once, for 10 minutes, only for this tab.';
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
 * once it can (`offerSetup`, `offerAgentSignIn`, `offerSignIn`). */
export function startFirstRun(document = globalThis.document, {storage} = {}) {
  if (document?.documentElement?.dataset?.firstRun === undefined || !document.getElementById?.('first-run')) return null;
  const run = createFirstRun({view: domView(document), clipboard: pageClipboard(), storage: storage ?? globalThis.localStorage,
    site: globalThis.location?.origin});
  const on = (id, handler) => document.getElementById(id)?.addEventListener('click', handler);
  const text = id => document.getElementById(id).textContent;
  on('first-run-agent', () => run.copy());
  on('first-run-agent-sign-in', () => run.signInWithAgent());
  on('first-run-retry', () => (document.getElementById('first-run').dataset.mode === 'sign-in' ? run.signInWithAgent() : run.copy()));
  on('first-run-again', () => run.again(text('first-run-prompt')));
  on('first-run-copy-mcp', () => run.again(text('first-run-mcp'), 'mcp'));
  on('first-run-copy-commands', () => run.again(text('first-run-commands'), 'commands'));
  on('first-run-manual-toggle', () => run.manual());
  on('first-run-back', () => run.back());
  for (const id of ['first-run-skip', 'first-run-skip-wait']) on(id, () => run.skip());
  on('first-run-passkey', () => run.passkey());
  return run;
}

/** This page's screen, started once on first import. */
export const firstRun = startFirstRun(globalThis.document);
