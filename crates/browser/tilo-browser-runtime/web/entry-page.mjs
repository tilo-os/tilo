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
 * visitor who already joined that host comes back at once. The embedding page
 * hears `{type: 'tilo-embed', state}`: `watching`, `joined`, or `full` /
 * `unavailable` when it should show its static fallback. A visitor the host
 * has no seat for watches live instead and is let in once a seat frees up.
 */
import {AdmissionError} from './admission.mjs';
import {failureMessage, startArrival, STOPPED_DRAWING} from './arrival.mjs';
import {boot} from './entry.mjs';
import {loadRuntime, mountDesktop, prefetchApplication, prepareGpu} from './host.mjs';
import {connectPeer, httpsPost, inspectInvitation, joinWorkspace, withCustody} from './peer.mjs';
import {readBrowserAccounts} from './browser-accounts.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';
import {createIdentityActions, syncProfile} from './identity-actions.mjs';
import {readInstallSecret, SET_UP, SETUP_PHASES, setUpFailure, setUpMachine} from './machine-setup.mjs';
import {browserIdentity, browserPerson, currentIdentity, IdentityError, MAX_DISPLAY_NAME_BYTES} from './root-identity.mjs';
import {rendererUnavailable} from './webgpu-safety.mjs';
import {chooseDisplayName, chosenDisplayName, HostAdmissionError, joinViaHost, keepOwnedHost, keptMembership, listedWorkspaces, membershipOrigins, pairViaHost, randomDisplayName, spectate} from './workspaces.mjs';

/** Where an embed's state goes: the site that embeds the workspace. */
const EMBEDDER = 'https://tilo.so';
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
    form.append(message);
    for (const [label, value] of choices) {
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = label;
      button.addEventListener('click', () => { form.remove(); resolve(value); });
      form.append(button);
    }
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

/** One desktop's host actions from several owners (`names`, in order):
 * each sets its own, and the desktop lists them all. */
function hostActionParts(mount, names) {
  const parts = new Map(names.map(name => [name, []]));
  return name => ({
    setHostActions: actions => { parts.set(name, actions); mount.setHostActions([...parts.values()].flat()); },
    offerHostAction: offer => mount.offerHostAction(offer),
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
    root.arrival = step;
    if (notes[step] && [loading, opening, drawing].includes(status.textContent)) status.textContent = notes[step];
  };
  // The arrival screen's line and bar follow those steps (arrival.mjs).
  startArrival({mode});
  const tell =state => { if (embed && parent !== window) parent.postMessage({type: 'tilo-embed', state}, EMBEDDER); };
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
  let displayName;
  /** The workspace this page is getting into, by name, once it knows it. */
  let workspaceName = query.get('workspace') || null;
  const diagnostics = {
    onHostAction: id => { if (id === SET_UP.id) void setUp?.(); else void identityActions?.perform(id); },
    ...(query.get('room') ? {room: query.get('room')} : {}),
    ...(query.get('workspace') ? {workspaceName: query.get('workspace')} : {}),
    ...(query.has('invite') ? {onInvite: () => { globalThis.tiloInvites += 1; }} : {}),
    // The render state is for tests and tools (data-phase, data-frames on
    // the root element); the bar only tells the person something they can
    // act on, unless the page was opened with ?debug.
    onStatus: state => {
      if (!state.phase) return;
      root.phase = state.phase;
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
    mountDesktop,
    enterViaHost: async ({workspace}) => {
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
        displayName = chosenDisplayName(identity?.userPublicKey ?? kept?.userPublicKey) ?? randomDisplayName();
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
      return card(`${offer.inviter?.name ?? 'Someone'} invites you to “${offer.workspaceName}” as ${offer.role}.`,
        [['Join', true], ['Not now', false]]);
    },
    joinWorkspace: async options => {
      return joinWorkspace({...options, identity: await browserPerson(), runtime});
    },
    pairDevice: ({origin, room, userPublicKey, workspace}) => {
      displayName = chosenDisplayName(userPublicKey) ?? randomDisplayName();
      return pairViaHost({origin, room, userPublicKey, workspace, runtime, displayName, label: deviceLabel(),
        onPairingCode: showPairingCode});
    },
    readAccounts: async origin => readBrowserAccounts({origin, runtime: await loadRuntime()}),
    chooseAccount: accounts => card('Open this link as',
      accounts.map(account => [account.displayName || account.userPublicKey.slice(0, 12), account])),
    // A guest ticket: a fresh device of the room's root, certified once the
    // web host spent the ticket on this page's pairing code. The root key is
    // read from the host's own certificate, so the link need not carry it.
    redeemTicket: async ({origin, room, ticket, tickets}) => {
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
  // The GPU is set up while the page joins and opens the workspace.
  prepareGpu();
  try {
    const page = await boot({mode, surface, status, deps, diagnostics, onStep: arrive});
    if (!page) return;
    if (entered) showVisitState({entered, retryAfter, tell, status});
    // Only the desktop lists host actions: a single app's mount refuses them.
    const hostActions = mode === 'desktop' && page.mount?.setHostActions && !embed ? hostActionParts(page.mount, ['identity', 'machine']) : null;
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
      await card(`${new URL(machine.origin).host} is yours now. It hosts your workspaces from here on.`, [['Done', null]]);
    };
    hostActions?.('machine').setHostActions([SET_UP]);
    if (page.install && !embed) void setUpFromLink(page.install, setUp);
    // The outcome, for tests and tools driving the page: open, refused or failed.
    document.documentElement.dataset.entry = 'open';
    globalThis.tiloMount = page.mount;
    globalThis.tiloRouter = page.router;
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
  } catch (error) {
    if (entered || embed) tell(error instanceof HostAdmissionError && error.code === 'full' ? 'full' : 'unavailable');
    arrive('stopped');
    document.documentElement.dataset.entry = error instanceof AdmissionError ? 'refused' : 'failed';
    // Plain words and a way to try again (index.html); the cause, thrown
    // on, reaches the console and the host's error report.
    status.textContent = failureMessage(error, {workspace: workspaceName, loaded: Boolean(runtime)});
    throw error;
  }
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
 * the person first posts or creates something in the room.
 */
function keepIdentity({peer, mount, home, name, runtime, link, naming}) {
  const identity = currentIdentity();
  if (!identity?.keepWithPasskey || !mount?.setHostActions) return null;
  // A device paired to a person whose root is elsewhere has nothing to keep.
  if (peer?.userPublicKey && peer.userPublicKey !== identity.userPublicKey) return null;
  const actions = createIdentityActions({
    identity, name, pairing: pairingOf({peer, identity, runtime, link}), naming: peer?.setDisplayName ? naming : null,
    hosts: () => membershipOrigins(identity.userPublicKey),
    lookups: () => [...membershipOrigins(), ...listedWorkspaces({home}).map(workspace => workspace.host.origin)],
  });
  void actions.attach(mount);
  if (peer?.authoredChanges) {
    // Checked on every peer change, and every few seconds in case a send
    // changed nothing the peer reports.
    let stop = () => {};
    const timer = setInterval(() => check(), 2000);
    const check = () => {
      if (peer.authoredChanges() > 0) { stop(); clearInterval(timer); actions.authored(); }
    };
    stop = peer.subscribe?.(check) ?? stop;
  }
  globalThis.tiloIdentity = actions;
  return actions;
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
