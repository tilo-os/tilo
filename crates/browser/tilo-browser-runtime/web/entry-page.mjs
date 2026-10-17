/**
 * The entry pages' wiring: the real page host and admission doors behind
 * `entry.mjs`, and the few controls a link needs (an invitation to accept,
 * an account to choose, the other mode). Diagnostic query switches stay
 * readable (`?room=`, `?workspace=`, `?invite`), since they say nothing
 * about what the link opens.
 *
 * `?embed=1` is the page inside another site (the homepage's live
 * preview): a workspace whose host has an admission door is first watched
 * read-only as its spectator, so scrolling past mints no member, and the
 * visitor joins as themselves on their first click, tap or key press. A
 * visitor who already joined that host comes back at once. The embedding page,
 * if it is the web host's embedder (`entry.json` `embedder`), hears
 * `{type: 'tilo-embed', state}`: `watching`, `joined`, or `full` /
 * `unavailable` when it should show its static fallback. A visitor the host
 * has no seat for watches live instead and is let in once a seat frees up.
 */
import {AdmissionError} from './admission.mjs';
import {failureMessage, refusedByHost, startArrival, STOPPED_DRAWING} from './arrival.mjs';
import {watchForUpdate} from './bundle-update.mjs';
import {boot, grammar} from './entry.mjs';
import {loadRuntime, mountDesktop, prefetchApplication, prepareGpu} from './host.mjs';
import {connectPeer, httpsPost, inspectInvitation, joinWorkspace, profileDevice, withCustody} from './peer.mjs';
import {readBrowserAccounts} from './browser-accounts.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';
import {createIdentityActions, syncProfile} from './identity-actions.mjs';
import {readInstallSecret, SET_UP, SETUP_PHASES, setUpFailure, setUpMachine} from './machine-setup.mjs';
import {browserIdentity, browserPerson, currentIdentity, IdentityError, MAX_DISPLAY_NAME_BYTES} from './root-identity.mjs';
import {followRefusal, refusalText, rejoinedName, retryRejoin} from './session-refusal.mjs';
import {rendererUnavailable} from './webgpu-safety.mjs';
import {createWorkspaceActions, INVITE} from './workspace-actions.mjs';
import {
  chooseDisplayName, chooseWorkspace, chosenDisplayName, chosenWorkspace, enteredWorkspaces, HostAdmissionError,
  hostToReconnect, joinViaHost, keepJoinedHost, keepOwnedHost, keptMembership, listedWorkspaces, markUnavailable,
  membershipOrigins, nameEntered, OPEN_WORKSPACE, ownedHosts, pairViaHost, randomDisplayName, rememberEntered,
  spectate, workspaceActions as workspaceSwitchActions, workspaceChoices,
} from './workspaces.mjs';

/** Set for the one reload that turns an embed's spectator into a member. */
const JOIN_NOW = 'tilo-embed-join';

const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');

/** The host's certificate; a room that just started refuses briefly. */
async function hostCertificate(runtime, origin, {timeoutMs = 15_000} = {}) {
  const deadline = Date.now() + timeoutMs;
  for (let delay = 100; ; delay = Math.min(delay * 2, 1000)) {
    try {
      return JSON.parse(await runtime.hostInfo(httpsPost(origin))).hostCert;
    } catch (error) {
      if (Date.now() + delay > deadline) throw error;
    }
    await new Promise(resolve => setTimeout(resolve, delay));
  }
}

function card(text, choices) {
  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card';
    const message = document.createElement('p');
    message.textContent = text;
    // One or two answers sit side by side, the first the one to take; a
    // longer list (accounts, machines) is a list of equal options.
    const actions = document.createElement('div');
    actions.className = choices.length <= 2 ? 'tilo-entry-actions' : 'tilo-entry-options';
    choices.forEach(([label, value], index) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = label;
      if (choices.length <= 2 && index === 0) button.className = 'tilo-entry-primary';
      button.addEventListener('click', () => { form.remove(); resolve(value); });
      actions.append(button);
    });
    form.append(message, actions);
    document.body.append(form);
  });
}

/** The person's name, asked over the page; resolves to what `save(typed)`
 * kept, or `null` when they keep the one they have. What `save` throws is
 * said in the form, which stays open. */
function askName(current = '', save = async typed => typed.trim() || null) {
  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card';
    const label = document.createElement('label');
    label.textContent = 'Your name, as others see it';
    const input = document.createElement('input');
    Object.assign(input, {name: 'display-name', value: current, required: true, autocomplete: 'name'});
    label.append(input);
    const error = document.createElement('p');
    error.className = 'tilo-entry-error';
    error.setAttribute('role', 'alert');
    error.hidden = true;
    // Only a name too long blocks the form; a refusal may be tried again.
    const say = (text, invalid = false) => { error.textContent = text; error.hidden = !text; input.setCustomValidity(invalid ? text : ''); };
    // Measured as hosts keep it: UTF-8 bytes, not characters.
    const tooLong = () => new TextEncoder().encode(input.value.trim()).length > MAX_DISPLAY_NAME_BYTES;
    input.addEventListener('input', () => say(tooLong() ? 'That name is too long' : '', tooLong()));
    const submit = document.createElement('button');
    submit.type = 'submit';
    submit.textContent = 'Save';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.textContent = 'Cancel';
    const done = value => { form.remove(); resolve(value); };
    form.addEventListener('submit', async event => {
      event.preventDefault();
      if (tooLong()) return;
      submit.disabled = true;
      try {
        done(await save(input.value));
      } catch (refusal) {
        say(refusal?.message ?? String(refusal));
      } finally {
        submit.disabled = false;
      }
    });
    cancel.addEventListener('click', () => done(null));
    form.append(label, error, submit, cancel);
    document.body.append(form);
    input.select();
  });
}

/**
 * A link the clipboard refused, over the page where the person can copy it
 * themselves: selected in a read-only field, so Ctrl+C or a long press
 * copies it. Gone once copied (`copied()` is called) or closed.
 */
function revealLink(link, copied = () => {}) {
  document.querySelector('[data-card="reveal-link"]')?.remove();
  const card = document.createElement('div');
  card.className = 'tilo-entry-card';
  card.dataset.card = 'reveal-link';
  card.setAttribute('role', 'dialog');
  card.setAttribute('aria-label', 'Pairing link');
  const label = document.createElement('label');
  label.textContent = 'Pairing link: copy it, then open it on your new device';
  const input = document.createElement('input');
  Object.assign(input, {name: 'pairing-link', value: link, readOnly: true, spellcheck: false, autocomplete: 'off'});
  label.append(input);
  const close = document.createElement('button');
  close.type = 'button';
  close.textContent = 'Close';
  const done = () => card.remove();
  input.addEventListener('copy', () => { done(); copied(); });
  close.addEventListener('click', done);
  card.addEventListener('keydown', event => { if (event.key === 'Escape') done(); });
  card.append(label, close);
  document.body.append(card);
  input.focus();
  input.select();
}

/**
 * Setting up a machine over the page: its address, key and install token
 * (filled in from an `install` link), what that means, and the person's
 * go-ahead. `run(values, onPhase)` installs; what it throws is said in the
 * form, which stays open to correct and try again. Resolves to what `run`
 * resolved to, or `null` when the person cancels.
 */
function machineCard({details = {}, run}) {
  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card';
    form.dataset.card = 'machine';
    const about = document.createElement('p');
    about.textContent = 'Set up a machine: it hosts your workspaces and keeps what you put in them. Only set up a machine you started yourself.';
    const inputs = [
      ['origin', 'Address', 'https://machine.example:8443'],
      ['devicePublicKey', 'Machine key (the devicePublicKey it printed)', ''],
      ['installToken', 'Install token (the text in its install token file)', ''],
    ].map(([name, text, placeholder]) => {
      const label = document.createElement('label');
      label.textContent = text;
      const input = document.createElement('input');
      Object.assign(input, {name, value: details[name] ?? '', placeholder, required: true, autocomplete: 'off', spellcheck: false});
      label.append(input);
      form.append(label);
      return input;
    });
    const progress = document.createElement('p');
    progress.setAttribute('role', 'status');
    const error = document.createElement('p');
    error.className = 'tilo-entry-error';
    error.setAttribute('role', 'alert');
    error.hidden = true;
    const submit = document.createElement('button');
    submit.type = 'submit';
    submit.textContent = 'Set up';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.textContent = 'Cancel';
    const done = value => { form.remove(); resolve(value); };
    form.addEventListener('submit', async event => {
      event.preventDefault();
      const values = Object.fromEntries(inputs.map(input => [input.name, input.value]));
      for (const control of [...inputs, submit]) control.disabled = true;
      error.hidden = true;
      try {
        done(await run(values, phase => { progress.textContent = SETUP_PHASES[phase] ?? ''; }));
      } catch (refusal) {
        progress.textContent = '';
        error.textContent = setUpFailure(refusal, values.origin);
        error.hidden = false;
      } finally {
        for (const control of [...inputs, submit]) control.disabled = false;
      }
    });
    cancel.addEventListener('click', () => done(null));
    form.prepend(about);
    form.append(progress, error, submit, cancel);
    document.body.append(form);
    (inputs.find(input => !input.value) ?? submit).focus();
  });
}

/** A form of one line over the page: resolves to what `save(typed)` resolved
 * to, or `null` when cancelled. What `save` throws is said in the form,
 * which stays open. */
function askLine({label: text, action, save}) {
  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card';
    const label = document.createElement('label');
    label.textContent = text;
    const input = Object.assign(document.createElement('input'), {required: true, autocomplete: 'off'});
    label.append(input);
    const error = document.createElement('p');
    error.className = 'tilo-entry-error';
    error.setAttribute('role', 'alert');
    error.hidden = true;
    const submit = Object.assign(document.createElement('button'), {type: 'submit', textContent: action});
    const cancel = Object.assign(document.createElement('button'), {type: 'button', textContent: 'Cancel'});
    const done = value => { form.remove(); resolve(value); };
    form.addEventListener('submit', async event => {
      event.preventDefault();
      if (!input.value.trim()) return;
      submit.disabled = true;
      error.hidden = true;
      try {
        done(await save(input.value.trim()));
      } catch (refusal) {
        error.textContent = refusal?.message ?? String(refusal);
        error.hidden = false;
      } finally {
        submit.disabled = false;
      }
    });
    cancel.addEventListener('click', () => done(null));
    form.append(label, error, submit, cancel);
    document.body.append(form);
    input.focus();
  });
}

/** A link to hand on: shown to select, with a button that copies it
 * (inside the person's click, which a browser's clipboard asks for). */
function shareCard({title, body, link}) {
  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card';
    form.dataset.card = 'share';
    const heading = Object.assign(document.createElement('p'), {textContent: title});
    const about = Object.assign(document.createElement('p'), {textContent: body});
    const field = Object.assign(document.createElement('input'), {name: 'link', value: link, readOnly: true});
    field.setAttribute('aria-label', 'Link');
    field.addEventListener('focus', () => field.select());
    const copy = Object.assign(document.createElement('button'), {type: 'button', textContent: 'Copy link'});
    copy.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(link);
        copy.textContent = 'Copied';
      } catch {
        field.focus();
        copy.textContent = 'Select the link and copy it';
      }
    });
    const done = Object.assign(document.createElement('button'), {type: 'submit', textContent: 'Done'});
    form.addEventListener('submit', event => { event.preventDefault(); form.remove(); resolve(); });
    form.append(heading, about, field, copy, done);
    document.body.append(form);
    copy.focus();
  });
}

/** The people waiting to join, each to let in or turn away; read again
 * after every decision. Resolves when closed. */
function waitingCard({load, admit, decline}) {
  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card';
    form.dataset.card = 'waiting';
    const heading = Object.assign(document.createElement('p'), {textContent: 'Waiting to join'});
    const list = document.createElement('ul');
    const error = document.createElement('p');
    error.className = 'tilo-entry-error';
    error.setAttribute('role', 'alert');
    error.hidden = true;
    const close = Object.assign(document.createElement('button'), {type: 'submit', textContent: 'Close'});
    form.addEventListener('submit', event => { event.preventDefault(); form.remove(); resolve(); });
    const said = refusal => { error.textContent = refusal?.message ?? String(refusal); error.hidden = false; };
    const show = async () => {
      let people = [];
      try { people = await load(); } catch (refusal) { said(refusal); }
      list.replaceChildren(...people.map(person => {
        const item = document.createElement('li');
        const who = Object.assign(document.createElement('span'), {textContent: `${person.name} (${person.role}) `});
        const letIn = Object.assign(document.createElement('button'), {type: 'button', textContent: 'Let in'});
        letIn.addEventListener('click', () => decide(admit, person.requestId));
        const turnAway = Object.assign(document.createElement('button'), {type: 'button', textContent: 'Turn away'});
        turnAway.addEventListener('click', () => decide(decline, person.requestId));
        item.append(who, letIn, turnAway);
        return item;
      }));
      if (!people.length) list.replaceChildren(Object.assign(document.createElement('li'), {textContent: 'Nobody is waiting.'}));
    };
    const decide = async (choice, requestId) => {
      error.hidden = true;
      try { await choice(requestId); } catch (refusal) { said(refusal); }
      await show();
    };
    form.append(heading, list, error, close);
    document.body.append(form);
    void show();
  });
}

/**
 * The root a machine is set up with: the one this page unlocked, else this
 * browser's own (made now on a first visit). Never a root other than the
 * person this page is in its workspace as: a device paired to a person kept
 * elsewhere holds no root of theirs.
 */
async function rootOfThisPage(peer) {
  const person = peer?.userPublicKey;
  const identity = currentIdentity() ?? await browserIdentity({create: !person});
  if (!identity || (person && identity.userPublicKey !== person)) {
    throw new IdentityError('locked', 'This browser does not hold the identity of the person it is here');
  }
  return identity;
}

/** The desktop command that opens the window in front in a browser tab of its own. */
const OPEN_IN_TAB = 'open-in-tab';

/** One desktop's host actions from several owners (`names`, in order):
 * each sets its own, and the desktop lists them all. */
function hostActionParts(mount, names) {
  const parts = new Map(names.map(name => [name, []]));
  return name => ({
    setHostActions: actions => { parts.set(name, actions); mount.setHostActions([...parts.values()].flat()); },
    offerHostAction: offer => mount.offerHostAction(offer),
    acknowledge: text => mount.acknowledge?.(text),
  });
}

/**
 * Why this browser cannot draw Tilo, with the steps that turn WebGPU on and
 * a way to try again once they are done. A changed browser setting (Firefox's
 * about:config) reaches a page only when it loads, so trying again reloads.
 */
function rendererCard({reason, steps}) {
  const form = document.createElement('form');
  form.className = 'tilo-entry-card';
  form.dataset.card = 'renderer';
  const message = document.createElement('p');
  message.textContent = reason;
  form.append(message);
  if (steps.length) {
    const list = document.createElement('ol');
    for (const step of steps) {
      const item = document.createElement('li');
      item.textContent = step;
      list.append(item);
    }
    form.append(list);
  }
  const retry = document.createElement('button');
  retry.type = 'button';
  retry.textContent = 'Try again';
  retry.addEventListener('click', () => location.reload());
  form.append(retry);
  document.body.append(form);
}

// Refusals that pass: the host is at its member cap, busy, or this address
// asked too often. The visitor watches meanwhile.
const seatless = error => error instanceof HostAdmissionError && ['full', 'unavailable', 'too-many'].includes(error.code);

/** Report a page's uncaught errors to the host that admitted it through its
 * door (size-capped and rate-limited there, logged and never stored). */
function reportErrors(door) {
  let sent = 0;
  const report = (kind, message, stack) => {
    if (sent++ >= 5) return;
    try {
      navigator.sendBeacon(new URL('errors', door), new Blob([JSON.stringify({
        kind, message: String(message), source: '', line: 0, column: 0, stack: stack || '', page: location.pathname,
      })], {type: 'text/plain'}));
    } catch { /* Reporting must never throw. */ }
  };
  addEventListener('error', event => report('error', event.message, event.error?.stack));
  addEventListener('unhandledrejection', event => report('unhandledrejection', event.reason?.message ?? event.reason, event.reason?.stack));
}

export async function startEntryPage({mode}) {
  const surface = document.getElementById('surface');
  const status = document.getElementById('status');
  const query = new URLSearchParams(location.search);
  const embed = query.get('embed') === '1';
  const debug = query.has('debug');
  const loading = status.textContent;
  // The step of getting in the page shows (`data-arrival` on the root, see
  // index.html): `load`ing Tilo, `join`ing through a host's door, `open`ing
  // the desktop, `draw`ing its first frame (GPU set up), `done` once it
  // drew, `stopped` when it cannot go on. Each step says itself in the
  // status line, unless something else (admission, a visit) is said there;
  // the screen shows a step's note only to screen readers (arrival.mjs).
  const root = document.documentElement.dataset;
  const opening = mode === 'app' ? 'Opening the app…' : 'Opening the workspace…';
  const drawing = 'Starting graphics…';
  const notes = {open: opening, draw: drawing};
  const arrive = step => {
    if (root.arrival === 'done' && step !== 'stopped') return;
    // The GPU is set up once the host let the page in, while the page
    // opens the workspace. A page that is refused (or left before it got
    // in) compiles nothing: the browser does not cancel a torn-down page's
    // pipeline compiles, and the next page's queue behind them on the same
    // compile threads. Under a software rasteriser that is a minute of CPU
    // and held the next page's first frame past 45 s.
    if (step === 'open' && root.arrival !== 'open') prepareGpu();
    root.arrival = step;
    if (notes[step] && [loading, opening, drawing].includes(status.textContent)) status.textContent = notes[step];
  };
  // The arrival screen's line and bar follow those steps (arrival.mjs).
  startArrival({mode});
  /** Where an embed's state goes: the site the web host names as its embedder, once the entry is read. */
  let embedder = null;
  const tell = state => { if (embed && embedder && parent !== window) parent.postMessage({type: 'tilo-embed', state}, embedder); };
  let joinNow = false;
  try { joinNow = sessionStorage.getItem(JOIN_NOW) === '1'; sessionStorage.removeItem(JOIN_NOW); } catch { /* No storage: watch first. */ }
  /** How this page entered through a host's door: `joined`, `watching` (until a gesture) or `seatless`. */
  let entered = null;
  let retryAfter = null;
  globalThis.tiloInvites = 0;
  /** Keeping the person this page joined as (`identity-actions.mjs`), once there is one. */
  let identityActions = null;
  /** Setting up a machine (`machine-setup.mjs`), once the page is open. */
  let setUp = null;
  /** Workspaces on the person's machines (`workspace-actions.mjs`), once the page is open. */
  let workspaceActions = null;
  let displayName;
  /** The workspace this page is getting into, by name, once it knows it. */
  let workspaceName = query.get('workspace') || null;
  /** The room the link names and the entry it is served under, once read (`deps.onScope`). */
  let admitting = null;
  /** The name the host signed into an invitation this page redeemed. */
  let invitedTo = null;
  /** Whether this page came in on a guest ticket, which is not remembered as membership. */
  let viaTicket = false;
  /** The room in front, remembered as one this browser entered, until the desktop names it. */
  let unnamed = null;
  /** Opening another of the person's workspaces, by host action id, once the page is open. */
  let switchWorkspace = null;
  /** Opening the window in front in a browser tab of its own, once the page is open. */
  let openInTab = null;
  const diagnostics = {
    onHostAction: id => {
      if (id === SET_UP.id) void setUp?.();
      else if (id === OPEN_IN_TAB) openInTab?.();
      else if (id.startsWith(OPEN_WORKSPACE)) void switchWorkspace?.(id);
      else if (workspaceActions?.handles(id)) void workspaceActions.perform(id);
      else void identityActions?.perform(id);
    },
    ...(query.get('room') ? {room: query.get('room')} : {}),
    ...(query.get('workspace') ? {workspaceName: query.get('workspace')} : {}),
    ...(query.has('invite') ? {onInvite: () => { globalThis.tiloInvites += 1; }} : {}),
    // The render state is for tests and tools (data-phase, data-frames on
    // the root element); the bar only tells the person something they can
    // act on, unless the page was opened with ?debug.
    onStatus: state => {
      if (unnamed && typeof state.workspaceName === 'string' && state.workspaceName) {
        const named = {...unnamed, name: state.workspaceName};
        unnamed = null;
        remembering(() => nameEntered(named));
      }
      if (!state.phase) return;
      root.phase = state.phase;
      // What graphics start-up waits on while the first frame is late
      // (device, pipelines, surface, first-frame), for tests and tools.
      if (state.gpuStep) root.gpuStep = state.gpuStep;
      if (state.phase === 'presented') arrive('done');
      else if (state.phase === 'gpu-pending') arrive('draw');
      else if (['failed', 'crashed'].includes(state.phase)) arrive('stopped');
      root.frames = String(state.frames ?? 0);
      if (debug) {
        status.textContent = `${state.phase}${state.error ? `: ${state.error}` : ''} · frames ${state.frames ?? 0}${state.persistence ? ` · ${state.persistence}` : ''}`;
      } else if (['failed', 'crashed'].includes(state.phase)) {
        // The cause is for the console; the person gets what to do.
        if (state.error) console.warn('desktop:', state.error);
        status.textContent = STOPPED_DRAWING;
      } else if (state.phase === 'presented' && [loading, opening, drawing].includes(status.textContent)) {
        // Only the page's own loading note: a visit message stays.
        status.textContent = '';
      }
    },
  };
  let runtime;
  /** Said while one of the person's devices decides on this one. */
  const showPairingCode = code => {
    document.documentElement.dataset.pairingCode = code;
    status.textContent = `Pairing code ${code}. On a device where you are already signed in, choose “Pair a new device” and approve ${code}.`;
  };
  const deps = {
    loadRuntime: async () => (runtime = await loadRuntime()),
    prefetchApplication,
    // An embed always shows the workspace the embedding site names.
    chosenWorkspace: () => (embed ? null : remembering(() => chosenWorkspace())),
    onScope: next => {
      admitting = next;
      embedder = next.entry?.embedder ?? null;
      const known = remembering(() => rememberedRoom(next.scope));
      if (known?.name && !workspaceName) workspaceName = known.name;
    },
    mountDesktop,
    enterViaHost: async ({workspace, room}) => {
      reportErrors(workspace.admission);
      workspaceName = workspace.name || workspaceName;
      const watch = async () => spectate({workspace, runtime});
      if (embed && !joinNow && !keptMembership(workspace)) {
        entered = 'watching';
        return watch();
      }
      try {
        // A device paired to a person kept elsewhere reconnects as them,
        // and never makes a person of its own.
        const kept = keptMembership(workspace);
        const identity = kept?.paired ? undefined : await browserPerson();
        // A page that came back here by itself because the host turned it
        // away (session-refusal.mjs) joins under the name it went by.
        displayName = chosenDisplayName(identity?.userPublicKey ?? kept?.userPublicKey) ?? rejoinedName() ?? randomDisplayName();
        const peer = await joinViaHost({workspace, identity, displayName, runtime, onPairingCode: showPairingCode});
        entered = 'joined';
        return peer;
      } catch (error) {
        if (!seatless(error)) throw error;
        entered = 'seatless';
        retryAfter = error.retryAfter;
        return watch();
      }
    },
    inspectInvitation: (origin, invitation) => inspectInvitation(origin, invitation, {runtime}),
    confirmInvitation: offer => {
      workspaceName = offer.workspaceName || workspaceName;
      invitedTo = offer.workspaceName || null;
      return card(`${offer.inviter?.name ?? 'Someone'} invites you to “${offer.workspaceName}” as ${offer.role}.`,
        [['Join', true], ['Not now', false]]);
    },
    joinWorkspace: async options => {
      const identity = await browserPerson();
      const peer = await joinWorkspace({...options, identity, runtime});
      // Kept, so the next visit reconnects as the person let in.
      keepJoinedHost({origin: options.origin, room: peer.room, userPublicKey: identity.userPublicKey});
      return peer;
    },
    reconnect: async ({origin, room}) => {
      const identity = currentIdentity() ?? await browserIdentity({create: false});
      const kept = identity && hostToReconnect(origin, identity.userPublicKey);
      if (!kept) return null;
      displayName = chosenDisplayName(identity.userPublicKey) ?? randomDisplayName();
      if (kept.owner) {
        // On a machine of their own the owner enters any of its rooms
        // (the host admits this device as its owner); the host refuses
        // one that does not exist.
        const lobby = await connectPeer({origin, identity, runtime, displayName, live: false});
        try { await lobby.workspaces.enter(room); } finally { await lobby.dispose?.(); }
      }
      const peer = await connectPeer({origin, room, identity, runtime, displayName});
      keepJoinedHost({origin, room, userPublicKey: identity.userPublicKey});
      return peer;
    },
    onInvite: () => { void workspaceActions?.perform(INVITE.id); },
    pairDevice: ({origin, room, userPublicKey, workspace}) => {
      displayName = chosenDisplayName(userPublicKey) ?? randomDisplayName();
      return pairViaHost({origin, room, userPublicKey, workspace, runtime, displayName, label: deviceLabel(),
        onPairingCode: showPairingCode});
    },
    readAccounts: async origin => readBrowserAccounts({origin, runtime: await loadRuntime(), currentDevice: profile => profileDevice(profile)}),
    chooseAccount: accounts => card('Open this link as',
      accounts.map(account => [account.displayName || account.userPublicKey.slice(0, 12), account])),
    // A guest ticket: a fresh device of the room's root, certified once the
    // web host spent the ticket on this page's pairing code. The root key is
    // read from the host's own certificate, so the link need not carry it.
    redeemTicket: async ({origin, room, ticket, tickets}) => {
      viaTicket = true;
      const {connectRelayPeer} = await import('./relay.mjs');
      const hostCert = await hostCertificate(await loadRuntime(), origin);
      const admit = new URL(`${encodeURIComponent(room)}/admit`, tickets).href;
      return connectRelayPeer({origin, room, userPublicKey: hex(hostCert.user_public_key), label: 'Guest',
        onPairingCode: async pairingCode => {
          const response = await fetch(admit, {method: 'POST', headers: {'content-type': 'application/json'},
            body: JSON.stringify({ticket, pairingCode})});
          if (!response.ok) status.textContent = (await response.json().catch(() => ({}))).error ?? 'The workspace did not let this device in';
        }});
    },
    connectMember: async ({origin, room, account}) => {
      const {userPublicKey, profile, device} = account;
      const custody = await openDeviceCustody({profile, storage: idbStorage(), requireExisting: true});
      return withCustody(custody, () => connectPeer({origin, room, userPublicKey, profile, device, custody,
        runtime, requireExistingCertificate: true}));
    },
  };
  const unavailable = await rendererUnavailable();
  if (unavailable) {
    // Said before admission, so a browser that cannot draw the desktop
    // never takes a seat or leaves a person behind.
    tell('unsupported');
    arrive('stopped');
    document.documentElement.dataset.entry = 'unsupported';
    status.textContent = '';
    rendererCard(unavailable);
    return;
  }
  try {
    const page = await boot({mode, surface, status, deps, diagnostics, onStep: arrive});
    if (!page) return;
    if (entered) showVisitState({entered, retryAfter, tell, status});
    // Only the desktop lists host actions: a single app's mount refuses them.
    const hostActions = mode === 'desktop' && page.mount?.setHostActions && !embed
      ? hostActionParts(page.mount, ['identity', 'machine', 'workspaces', 'workspace', 'tab']) : null;
    const here = page.router.route.target.scope;
    if (!embed && !entered && !viaTicket && page.peer && here.kind === 'room') {
      // A room entered as a member: offered again from the person's other workspaces.
      const room = {origin: here.host, room: here.room ?? page.peer.room};
      remembering(() => {
        rememberEntered({...room, name: invitedTo});
        if (!rememberedRoom({kind: 'room', host: room.origin, room: room.room})?.name) unnamed = room;
      });
    }
    if (hostActions) {
      const offered = remembering(() => workspaceChoices({home: page.entry?.home, homeRoom: homeRoomOf(page.entry),
        entered: enteredWorkspaces(), current: here})) ?? [];
      hostActions('workspaces').setHostActions(workspaceSwitchActions(offered));
      switchWorkspace = id => {
        const choice = offered.find(candidate => candidate.id === id);
        return choice ? openWorkspace({choice, page, status}) : undefined;
      };
    }
    const attachIdentity = () => keepIdentity({peer: page.peer, mount: hostActions?.('identity') ?? page.mount, home: page.entry?.home,
      name: () => displayName, runtime, link: page.link, naming: {
        ask: (current, save) => askName(current, save),
        set: chosen => {
          displayName = chooseDisplayName(page.peer.userPublicKey, chosen);
          page.peer.setDisplayName?.(displayName);
          return displayName;
        },
      }});
    identityActions = attachIdentity();
    void followProfile(page, name => { displayName = name; });
    setUp = async (details = {}) => {
      const machine = await machineCard({details, run: (values, onPhase) => setUpMachine({
        details: values, person: () => rootOfThisPage(page.peer), runtime, onProgress: event => onPhase(event.phase)})});
      if (!machine) return;
      keepOwnedHost(machine);
      // A first visit made its person just now: the machine trusts that
      // root from here on, so offer to keep it.
      identityActions ??= attachIdentity();
      identityActions?.authored();
      if (workspaceActions) void workspaceActions.attach(hostActions('workspace'));
      const next = await card(`${new URL(machine.origin).host} is yours now. It hosts your workspaces from here on.`,
        [...(workspaceActions ? [['New workspace on it', true]] : []), ['Done', false]]);
      if (next) await workspaceActions.createOn(machine.origin);
    };
    hostActions?.('machine').setHostActions([SET_UP]);
    const person = () => page.peer?.userPublicKey ?? currentIdentity()?.userPublicKey;
    const roomLink = (origin, room) => page.link.format({mode: 'desktop', target: {scope: {kind: 'room', host: origin, room}}});
    // Only a desktop lists host actions and shows banners.
    workspaceActions = hostActions ? createWorkspaceActions({
      peer: page.peer,
      machines: () => (person() ? ownedHosts(person()) : []),
      lobby: async origin => connectPeer({origin, identity: await rootOfThisPage(page.peer), runtime, displayName, live: false}),
      inviteLink: invitation => {
        const url = roomLink(page.peer.origin, page.peer.room);
        return `${url}${new URL(url).hash.includes('?') ? '&' : '?'}invite=${invitation}`;
      },
      open: async ({origin, room, name}) => {
        keepJoinedHost({origin, room, userPublicKey: (await rootOfThisPage(page.peer)).userPublicKey});
        remembering(() => rememberEntered({origin, room, name}));
        // Creating a room leaves through the same save-and-close path as switching.
        await openWorkspace({choice: {origin, room, name}, page, status});
      },
      ui: {
        askWorkspaceName: save => askLine({label: 'Name of the new workspace', action: 'Create', save}),
        chooseMachine: origins => card('Create the workspace on', [...origins.map(origin => [new URL(origin).host, origin]), ['Cancel', null]]),
        share: shareCard,
        waitingList: waitingCard,
        fail: text => card(text, [['Close', null]]),
      },
    }) : null;
    void workspaceActions?.attach(hostActions('workspace'));
    if (page.peer) {
      // The host turning this page's session away while it is open: a page
      // that came in through the host's door joins again through it, as on
      // arrival; anything else is said, with the one thing left to do.
      const home = page.entry?.home ?? null;
      followRefusal({
        peer: page.peer,
        displayName: () => displayName,
        canRejoin: () => Boolean(home?.admission) && ['joined', 'watching', 'seatless'].includes(entered)
          && !remembering(() => keptMembership(home))?.paired,
        rejoin: async () => {
          await closeRoom(page);
          // The bare entry is the door's: a link naming the room the host
          // forgot would be refused. An embed keeps the address it was given.
          if (embed) location.reload(); else loadPage(page.entry.base);
        },
        refuse: async code => {
          tell('unavailable');
          document.documentElement.dataset.refusal = code;
          const known = remembering(() => rememberedRoom(here));
          if (known) remembering(() => markUnavailable(known));
          if (embed) return;
          const own = await card(refusalText(code, workspaceName), [['Open your own desktop', true]]);
          if (!own) return;
          await closeRoom(page);
          loadPage(page.link.format({mode: 'desktop', target: {scope: {kind: 'own'}}}));
        },
      });
    }
    if (page.install && !embed) void setUpFromLink(page.install, setUp);
    // The outcome, for tests and tools driving the page: open, refused or failed.
    document.documentElement.dataset.entry = 'open';
    globalThis.tiloMount = page.mount;
    globalThis.tiloRouter = page.router;
    // A window that opens after a newer version was deployed finds this
    // version's files gone: offer the reload rather than fail it quietly.
    watchForUpdate({offer: () => offerUpdate({page, status}),
      onChecked: update => { if (update) document.documentElement.dataset.update = update.deployed; }});
    const other = document.getElementById('other-mode');
    if (other) {
      // Written when followed, so it carries whatever is in front by then.
      const refresh = () => {
        other.href = page.router.url(mode === 'app' ? 'desktop' : 'app');
        return Boolean(page.router.route.target.app);
      };
      other.hidden = !refresh();
      for (const type of ['pointerdown', 'focus', 'click']) other.addEventListener(type, refresh);
      page.navigation?.subscribe(() => { other.hidden = !refresh(); });
    }
    if (hostActions) {
      // The desktop's way to the window in front as a page of its own: a
      // command beside "Close window", there while an app is in front.
      const tab = hostActions('tab');
      const offerTab = () => tab.setHostActions(page.router.route.target.app
        ? [{id: OPEN_IN_TAB, title: 'Open in new tab', subtitle: 'This window, in a browser tab of its own'}] : []);
      openInTab = () => globalThis.open(page.router.url('app'), '_blank', 'noopener');
      offerTab();
      page.navigation?.subscribe(offerTab);
    }
  } catch (error) {
    // A page that joined again by itself and found the door not answering
    // yet (the host is still starting) tries again, still loading.
    const again = error instanceof HostAdmissionError && error.code === 'unavailable' ? retryRejoin() : null;
    if (again !== null) {
      console.warn('admission: the door did not answer yet, trying again', error?.message);
      setTimeout(() => location.reload(), again);
      return;
    }
    if (entered || embed) tell(error instanceof HostAdmissionError && error.code === 'full' ? 'full' : 'unavailable');
    arrive('stopped');
    // A refusal is the host's answer, which trying again does not change.
    document.documentElement.dataset.entry = error instanceof AdmissionError || refusedByHost(error) ? 'refused' : 'failed';
    // Plain words and a way to try again (index.html); the cause, thrown
    // on, reaches the console and the host's error report.
    status.textContent = failureMessage(error, {workspace: workspaceName, loaded: Boolean(runtime)});
    const known = admitting && remembering(() => rememberedRoom(admitting.scope));
    // A declined or unusable invitation, a damaged link or this browser's
    // device key held by another tab says nothing about the room.
    const local = error?.name === 'CustodyError' || (error instanceof AdmissionError && !['no-access', 'refused', 'host-changed', 'removed'].includes(error.code));
    if (known && !local) {
      // A room this browser was a member of: say what is known, and stop
      // reopening it on a bare entry.
      remembering(() => markUnavailable(known));
      if (error instanceof AdmissionError && error.code === 'no-access') {
        status.textContent = `${known.name ?? 'This workspace'} did not let this browser in. You may have been removed from it, or it no longer exists.`;
      }
    }
    if (!embed && mode === 'desktop' && admitting) {
      const refused = document.documentElement.dataset.entry === 'refused';
      void offerOtherWorkspaces({...admitting, runtime}).then(offered => {
        // Nothing to try again and nowhere else of theirs to go: the
        // person's own desktop is always there.
        if (!offered && refused) offerOwnDesktop({...admitting, runtime});
      });
    }
    throw error;
  }
}

/** `read()`, or `null` where this browser keeps no storage for the page:
 * remembering workspaces never stops one from opening. */
function remembering(read) {
  try {
    return read();
  } catch (error) {
    console.warn('workspaces: this browser cannot remember them', error);
    return null;
  }
}

/** The remembered room `scope` names, if this browser entered it as a member. */
function rememberedRoom(scope) {
  if (scope?.kind !== 'room' || !scope.room) return null;
  const origin = new URL(scope.host).origin;
  return enteredWorkspaces().find(entry => entry.origin === origin && entry.room === scope.room) ?? null;
}

/** The room this browser was admitted to through the home workspace's door. */
function homeRoomOf(entry) {
  return entry?.home ? keptMembership(entry.home)?.room ?? null : null;
}

/** The address that opens `choice` (`workspaceChoices`): the bare entry for the home. */
function choiceUrl(choice, entry, link) {
  if (choice.home) return entry.base;
  return link.format({mode: 'desktop', target: {scope: {kind: 'room', host: choice.origin, room: choice.room}}});
}

/**
 * Load `url` as a new page. Only the fragment differs between rooms under
 * one entry, so that is written as a new history entry and the page loads
 * again: every room is admitted by a page of its own.
 */
function loadPage(url) {
  const next = new URL(url, location.href);
  const here = new URL(location.href);
  if (next.origin === here.origin && next.pathname === here.pathname && next.search === here.search) {
    history.pushState(null, '', next.href);
    location.reload();
  } else {
    location.assign(next.href);
  }
}

/**
 * Switch to another of the person's workspaces: save what is pending in the
 * room in front (staying there when that fails), close its desktop and its
 * peer, then load the other room's page, which admits the person afresh.
 * A bare entry reopens the choice from now on.
 */
async function openWorkspace({choice, page, status}) {
  const url = choiceUrl(choice, page.entry, page.link);
  try {
    await page.mount?.flush?.();
  } catch (error) {
    console.warn('switching workspace: saving failed', error);
    await card('Your latest changes here are not saved yet, so Tilo stayed in this workspace. Try again in a moment.', [['OK', null]]);
    return;
  }
  remembering(() => chooseWorkspace(choice.home ? null : choice));
  status.textContent = `Opening ${choice.name}…`;
  await closeRoom(page);
  loadPage(url);
}

/** Stop following links, then close the room's desktop and its peer, saving
 * what they hold; the person decides whether to leave without what cannot
 * be saved. */
async function closeRoom(page) {
  page.router.stop();
  for (;;) {
    try {
      await page.mount?.cleanup?.();
      await page.peer?.dispose?.();
      return;
    } catch (error) {
      console.warn('leaving the page: closing failed', error);
      const again = await card('Some changes in this workspace could not be saved yet.', [['Try again', true], ['Leave without them', false]]);
      if (!again) return;
    }
  }
}

/**
 * This page runs a version of Tilo the web entry no longer serves, and just
 * missed a file of it (bundle-update.mjs). Offer to reload into the new
 * version: the desktop and its documents are kept in this browser, so the
 * reloaded page reopens the same windows, the one that failed included.
 * Saved first, as for a switch of workspace; when saving fails the page
 * stays.
 */
async function offerUpdate({page, status}) {
  const reload = await card('Tilo was updated while this page was open, so it could not open what you asked for. Reload to use the new version; your windows and documents stay as they are.',
    [['Reload', true], ['Not now', false]]);
  if (!reload) return;
  try {
    await page.mount?.flush?.();
  } catch (error) {
    console.warn('updating: saving failed', error);
    await card('Your latest changes here are not saved yet, so Tilo did not reload. Try again in a moment.', [['OK', null]]);
    return;
  }
  status.textContent = 'Updating Tilo…';
  await closeRoom(page);
  location.reload();
}

/** The page's own-desktop button (index.html), shown on a refusal: it opens
 * the person's own desktop under this entry. */
function offerOwnDesktop({entry, runtime}) {
  const button = document.getElementById('arrival-own');
  if (!button) return;
  let url;
  try {
    url = grammar(runtime, entry).format({mode: 'desktop', target: {scope: {kind: 'own'}}});
  } catch {
    url = new URL('#/me', entry.base).href;
  }
  button.addEventListener('click', () => loadPage(url), {once: true});
  document.documentElement.dataset.ownDesktop = '';
}

/** After a failure to open `scope`, offer the person's other workspaces;
 * resolves to whether there were any to offer. */
async function offerOtherWorkspaces({scope, entry, runtime}) {
  const link = runtime ? grammar(runtime, entry) : null;
  const choices = (remembering(() => workspaceChoices({home: entry.home, homeRoom: homeRoomOf(entry),
    entered: enteredWorkspaces(), current: scope})) ?? []).filter(choice => choice.home || link);
  if (!choices.length) return false;
  const choice = await card('Open another of your workspaces',
    [...choices.map(option => [option.unavailable ? `${option.name} (did not open last time)` : option.name, option]), ['Not now', null]]);
  if (!choice) return true;
  remembering(() => chooseWorkspace(choice.home ? null : choice));
  loadPage(choiceUrl(choice, entry, link));
  return true;
}

/** Offer to set up the machine an `install` link names (`machine-setup.mjs`). */
async function setUpFromLink(secret, setUp) {
  let details;
  try {
    details = readInstallSecret(secret);
  } catch (error) {
    await card(setUpFailure(error), [['Close', null]]);
    return;
  }
  await setUp(details);
}

/**
 * The name the person this page joined as chose on any of their devices,
 * from the profile their hosts keep; one chosen only here goes to them.
 */
async function followProfile(page, onName) {
  const userPublicKey = page.peer?.userPublicKey;
  if (!userPublicKey || !page.peer.setDisplayName) return;
  const origins = [page.peer.origin, ...membershipOrigins(userPublicKey),
    ...listedWorkspaces({home: page.entry?.home}).map(workspace => workspace.host.origin)].filter(Boolean);
  try {
    await syncProfile({userPublicKey, origins, identity: currentIdentity(), chosen: chosenDisplayName(userPublicKey),
      apply: name => {
        const kept = chooseDisplayName(userPublicKey, name);
        page.peer.setDisplayName(kept);
        onName(kept);
      }});
  } catch (error) {
    console.warn('identity:', error);
  }
}

/** Reload this page to join the workspace it watches. */
function joinByReload() {
  try { sessionStorage.setItem(JOIN_NOW, '1'); } catch { /* A kept membership or a full room decides then. */ }
  location.reload();
}

/**
 * What a visitor let in through a host's door sees beyond the workspace: as
 * a watching embed, the invitation to join on the first gesture (captured,
 * so the spectator surface never sees it); without a seat, that they watch
 * live and are let in when one frees up (retried after the host's
 * Retry-After, with jitter so a full room is not asked by everyone at once).
 */
function showVisitState({entered, retryAfter, tell, status}) {
  if (entered === 'joined') { tell('joined'); return; }
  tell('watching');
  if (entered === 'watching') {
    status.textContent = 'Click or type to join';
    const gesture = event => {
      for (const type of ['pointerdown', 'keydown']) removeEventListener(type, gesture, true);
      event.preventDefault();
      event.stopPropagation();
      status.textContent = 'Joining…';
      joinByReload();
    };
    for (const type of ['pointerdown', 'keydown']) addEventListener(type, gesture, true);
    return;
  }
  status.textContent = 'It’s full right now, so you’re watching live. You’ll join when a seat frees up.';
  const wait = retryAfter ?? 30;
  setTimeout(joinByReload, (wait + Math.random() * wait / 2) * 1000);
}

/**
 * The identity actions of the person this page joined as, when this
 * browser holds them (`browserIdentity`): shown in the desktop, offered once
 * the person has done enough in the room to be worth keeping, at a pause.
 */
function keepIdentity({peer, mount, home, name, runtime, link, naming}) {
  const identity = currentIdentity();
  if (!identity?.keepWithPasskey || !mount?.setHostActions) return null;
  // A device paired to a person whose root is elsewhere has nothing to keep.
  if (peer?.userPublicKey && peer.userPublicKey !== identity.userPublicKey) return null;
  const mac = /Mac|iPhone|iPad/.test(navigator.platform ?? navigator.userAgent ?? '');
  const actions = createIdentityActions({
    identity, name, pairing: pairingOf({peer, identity, runtime, link}), naming: peer?.setDisplayName ? naming : null,
    hosts: () => membershipOrigins(identity.userPublicKey),
    lookups: () => [...membershipOrigins(), ...listedWorkspaces({home}).map(workspace => workspace.host.origin)],
    pause: inputPause(), searchChord: mac ? '⌘K' : 'Ctrl+K', revealLink,
  });
  void actions.attach(mount);
  if (peer?.authoredChanges) {
    // Read on every peer change, and every few seconds in case a send
    // changed nothing the peer reports; stops once the identity is kept.
    let stop = () => {};
    const timer = setInterval(() => check(), 2000);
    const check = () => {
      if (identity.kept) { stop(); clearInterval(timer); return; }
      actions.authored(peer.authoredChanges());
    };
    stop = peer.subscribe?.(check) ?? stop;
  }
  globalThis.tiloIdentity = actions;
  return actions;
}

/** How long without input counts as a pause in what the person is doing. */
const PAUSE_MS = 2500;

/**
 * Whether the person is between things: no key, pointer or wheel input for
 * `PAUSE_MS` and no button held (a drag in progress). Only when, never what:
 * nothing about the input is kept.
 */
function inputPause() {
  let last = Date.now();
  let held = 0;
  const touch = () => { last = Date.now(); };
  addEventListener('pointerdown', () => { held += 1; touch(); }, {capture: true, passive: true});
  for (const type of ['pointerup', 'pointercancel']) {
    addEventListener(type, () => { held = Math.max(0, held - 1); touch(); }, {capture: true, passive: true});
  }
  for (const type of ['keydown', 'wheel', 'pointermove']) addEventListener(type, touch, {capture: true, passive: true});
  return () => held === 0 && Date.now() - last >= PAUSE_MS;
}

/**
 * Pairing another device of `identity` through the host `peer` is in: the
 * link that opens this workspace there as a new device of this person, and
 * the host's requests waiting for this root to approve them.
 */
function pairingOf({peer, identity, runtime, link}) {
  if (!peer?.origin || !peer.room || !runtime?.pendingPairings || !link) return null;
  const post = httpsPost(peer.origin);
  return {
    link: () => {
      const url = link.format({mode: 'desktop', target: {scope: {kind: 'room', host: peer.origin, room: peer.room}}});
      return `${url}${new URL(url).hash.includes('?') ? '&' : '?'}pair=${identity.userPublicKey}`;
    },
    pending: async () => JSON.parse(await runtime.pendingPairings(post, identity)),
    answer: (digest, approved) => runtime.answerPairing(post, identity, digest, approved),
  };
}

/** What a new device is called where the person approves it. */
function deviceLabel() {
  const platform = navigator.userAgentData?.platform || navigator.platform || '';
  return platform ? `Browser on ${platform}`.slice(0, 64) : 'Browser';
}
