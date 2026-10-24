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
import {doorCopy, failureMessage, inviteLinkTarget, localeMessages, localeMessagesSoon, localized, refusedByHost, startArrival, STOPPED_DRAWING, stoppedInside, visitLine, waitListFailure} from './arrival.mjs';
import {watchForUpdate} from './bundle-update.mjs';
import {boot, grammar} from './entry.mjs';
import {loadRuntime, mountDesktop, prefetchApplication, prepareGpu} from './host.mjs';
import {connectPeer, httpsPost, identityProfile, inspectInvitation, joinWorkspace, profileDevice, withCustody} from './peer.mjs';
import {readBrowserAccounts} from './browser-accounts.mjs';
import {CustodyError, idbStorage, openDeviceCustody} from './device-custody.mjs';
import {createIdentityActions, syncProfile} from './identity-actions.mjs';
import {countryName, deviceLabel, pairingFailure, pairSecret, whenRequested} from './pairing.mjs';
import {qrSvg} from './qr.mjs';
import {readInstallSecret, SET_UP, SETUP_PHASES, setUpFailure, setUpMachine} from './machine-setup.mjs';
import {browserIdentity, browserPerson, currentIdentity, fetchEnvelope, IdentityError, MAX_DISPLAY_NAME_BYTES, signInWithPasskey} from './root-identity.mjs';
import {followRefusal, refusalText, rejoinedName, retryRejoin} from './session-refusal.mjs';
import {rendererUnavailable} from './webgpu-safety.mjs';
import {createWorkspaceActions, INVITE} from './workspace-actions.mjs';
import {
  chooseDisplayName, chooseWorkspace, chosenDisplayName, chosenWorkspace, enteredWorkspaces, forgetGoneRooms, HostAdmissionError,
  hostDeviceOf, hostToReconnect, isPublicHome, joinViaHost, keepJoinedHost, keepOwnedHost, keptMembership, leaveOnWaitList, listedWorkspaces,
  doorPresence, joinWhenSeated, membershipOrigins, nameEntered, OPEN_WORKSPACE, ownedHosts, pairViaHost, defaultDisplayName, redrawnDisplayName, rememberEntered,
  settleFailedRoom, spectate, workspaceActions as workspaceSwitchActions, workspaceChoices,
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

/** How long the host at a remembered room's origin gets to say who answers there. */
const HOST_CHECK_MS = 5_000;

/** Whether the remembered room `known` is gone, asked of its origin after
 * opening it failed (`settleFailedRoom`): `gone`, `unavailable` or `null`. */
async function settleRoom(known, runtime) {
  const hostInfo = async origin => {
    if (!runtime) throw new Error('no runtime to ask the host with');
    const fetcher = (url, init) => globalThis.fetch(url, {...init, signal: AbortSignal.timeout(HOST_CHECK_MS)});
    return JSON.parse(await runtime.hostInfo(httpsPost(origin, fetcher))).hostCert;
  };
  try {
    return await settleFailedRoom(known, {hostInfo});
  } catch (error) {
    console.warn('workspaces: this browser cannot remember them', error);
    return null;
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

/** A pairing card over the page: `data-card` names it, `title` heads it. */
function pairCard(kind, title, role = 'dialog') {
  document.querySelector(`[data-card="${kind}"]`)?.remove();
  const card = document.createElement('div');
  card.className = 'tilo-entry-card tilo-pair';
  card.dataset.card = kind;
  card.setAttribute('role', role);
  card.setAttribute('aria-label', title);
  const heading = document.createElement('h2');
  heading.textContent = title;
  card.append(heading);
  return card;
}

const paragraph = (text, className) => {
  const element = document.createElement('p');
  element.textContent = text;
  if (className) element.className = className;
  return element;
};

/**
 * The sheet that pairs a new device: the link as a QR code for a phone's
 * camera, as text to open or paste, and a Copy button (a click, which the
 * clipboard honours). `copy()` resolves whether the link was copied; where
 * it was not, the link is selected for the person to copy. Closing the
 * sheet leaves the link working: its device is still asked about here.
 */
function pairLinkSheet({link, copy}) {
  const card = pairCard('pair-link', 'Pair a new device');
  const code = document.createElement('img');
  Object.assign(code, {className: 'tilo-pair-qr', alt: 'QR code of the pairing link', width: 240, height: 240,
    src: `data:image/svg+xml,${encodeURIComponent(qrSvg(link))}`});
  const input = document.createElement('input');
  Object.assign(input, {name: 'pairing-link', value: link, readOnly: true, spellcheck: false, autocomplete: 'off'});
  input.setAttribute('aria-label', 'Pairing link');
  const actions = document.createElement('div');
  actions.className = 'tilo-entry-actions';
  const copyButton = document.createElement('button');
  copyButton.type = 'button';
  copyButton.className = 'tilo-entry-primary';
  copyButton.textContent = 'Copy link';
  const close = document.createElement('button');
  close.type = 'button';
  close.textContent = 'Close';
  const copied = () => { copyButton.textContent = 'Copied'; };
  copyButton.addEventListener('click', async () => {
    if (await copy()) copied();
    else { input.focus(); input.select(); }
  });
  input.addEventListener('copy', copied);
  const done = () => card.remove();
  close.addEventListener('click', done);
  card.addEventListener('keydown', event => { if (event.key === 'Escape') done(); });
  actions.append(copyButton, close);
  card.append(code,
    paragraph('On a phone, scan this code with the camera. In a browser, open the link. On another Tilo computer, paste the link into Join a workspace…'),
    input,
    paragraph('This device asks you before anything is let in, and shows the code your new device shows. The link works once, for 10 minutes.', 'tilo-pair-note'),
    actions);
  document.body.append(card);
  copyButton.focus();
  return {close: done};
}

/**
 * The ask about a device that answered this page's pairing link: what the
 * host saw of it (its label, when it asked, from where) and the code it
 * shows, with Approve and Decline. `decide(approved)` answers; the card
 * then says what came of it and goes. Decline has the focus, so a stray
 * Enter lets nothing in.
 */
function pairingAsk(request, decide) {
  const card = pairCard('pair-ask', 'A device asks to be you', 'alertdialog');
  const details = document.createElement('dl');
  details.className = 'tilo-pair-device';
  const detail = (term, value) => {
    const name = document.createElement('dt');
    name.textContent = term;
    const text = document.createElement('dd');
    text.textContent = value;
    details.append(name, text);
    return text;
  };
  detail('Device', request.label || 'A browser');
  const when = detail('Asked', '');
  // In the card's own language, which is English for now.
  const country = countryName(request.country, {locale: 'en'});
  if (country) detail('From', country);
  const tick = () => { when.textContent = whenRequested(request.requestedAtMs, {locale: 'en'}); };
  tick();
  const timer = setInterval(tick, 15_000);
  const error = paragraph('', 'tilo-entry-error');
  error.setAttribute('role', 'alert');
  error.hidden = true;
  const actions = document.createElement('div');
  actions.className = 'tilo-entry-actions';
  const approve = document.createElement('button');
  approve.type = 'button';
  approve.className = 'tilo-entry-primary';
  approve.textContent = `Approve ${request.pairingCode}`;
  const decline = document.createElement('button');
  decline.type = 'button';
  decline.textContent = 'Decline';
  actions.append(approve, decline);
  const close = () => { clearInterval(timer); card.remove(); };
  const answer = async approved => {
    approve.disabled = decline.disabled = true;
    error.hidden = true;
    let result;
    try {
      result = await decide(approved);
    } catch (failure) {
      error.textContent = `That did not reach the host: ${failure?.message ?? failure}. Try again.`;
      error.hidden = false;
      approve.disabled = decline.disabled = false;
      return;
    }
    clearInterval(timer);
    const label = result.label || 'The device';
    card.replaceChildren();
    if (result.approved) {
      card.classList.add('tilo-pair-done');
      const check = document.createElement('span');
      check.className = 'tilo-pair-check';
      check.setAttribute('aria-hidden', 'true');
      card.append(check, paragraph(`${label} is now one of your devices`, 'tilo-pair-said'));
    } else {
      card.append(paragraph(`Declined. ${label} was not let in.`, 'tilo-pair-said'));
    }
    setTimeout(close, 4000);
  };
  approve.addEventListener('click', () => answer(true));
  decline.addEventListener('click', () => answer(false));
  card.append(details, paragraph('Approve only if your new device shows this code:'),
    paragraph(request.pairingCode, 'tilo-pair-digits'), paragraph('Approving lets it act as you.'), error, actions);
  document.body.append(card);
  decline.focus();
  return {close};
}

/** Said on a new device while the person's other device decides on it:
 * the code that device shows beside its ask. */
function pairingWait(code) {
  document.documentElement.dataset.pairingCode = code;
  const card = pairCard('pair-wait', 'Waiting for approval on your other device…', 'status');
  card.append(paragraph('Approve there if it shows this code:'), paragraph(code, 'tilo-pair-digits'),
    paragraph('This page goes on by itself once you approve.', 'tilo-pair-note'));
  document.body.append(card);
  return () => card.remove();
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

/** When a wait-list address was left, as the inviter reads dates. */
const addedAt = atMs => new Intl.DateTimeFormat(undefined, {dateStyle: 'medium', timeStyle: 'short'}).format(new Date(atMs));

/** The people waiting to join, each to let in or turn away, and the
 * addresses on the room's wait list, each to invite (`invite(entry)` makes
 * and shares an invitation for it) or take off the list (`unlist`), as
 * `load()` resolves them (`{people, waitList}`); read again after every
 * decision. Resolves when closed. */
function waitingCard({load, admit, decline, invite, unlist}) {
  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card tilo-waiting';
    form.dataset.card = 'waiting';
    const heading = Object.assign(document.createElement('h2'), {textContent: 'Waiting to join'});
    const list = document.createElement('ul');
    const listed = document.createElement('section');
    listed.dataset.list = 'wait-list';
    const error = document.createElement('p');
    error.className = 'tilo-entry-error';
    error.setAttribute('role', 'alert');
    error.hidden = true;
    const close = Object.assign(document.createElement('button'), {type: 'submit', textContent: 'Close'});
    form.addEventListener('submit', event => { event.preventDefault(); form.remove(); resolve(); });
    const said = refusal => { error.textContent = refusal?.message ?? String(refusal); error.hidden = false; };
    const button = (text, run) => {
      const made = Object.assign(document.createElement('button'), {type: 'button', textContent: text});
      made.addEventListener('click', run);
      return made;
    };
    const show = async () => {
      let people = [];
      let contacts = [];
      try { ({people, waitList: contacts} = await load()); } catch (refusal) { said(refusal); }
      list.replaceChildren(...people.map(person => {
        const item = document.createElement('li');
        const who = Object.assign(document.createElement('span'), {className: 'tilo-waiting-who', textContent: `${person.name} (${person.role})`});
        item.append(who, button('Let in', () => decide(admit, person.requestId)), button('Turn away', () => decide(decline, person.requestId)));
        return item;
      }));
      // Nobody to let in: said only where there is no wait list to show instead.
      list.hidden = !people.length && contacts.length > 0;
      if (!people.length) list.replaceChildren(Object.assign(document.createElement('li'), {className: 'tilo-waiting-none', textContent: 'Nobody is waiting to be let in.'}));
      // The wait list: addresses people left at the door, oldest first.
      listed.replaceChildren();
      if (!contacts.length) return;
      const title = Object.assign(document.createElement('h3'), {textContent: `Wait list · ${contacts.length}`});
      const about = Object.assign(document.createElement('p'), {className: 'tilo-waiting-note',
        textContent: 'They asked to join without an invitation. Invite makes a link for you to email them.'});
      const rows = document.createElement('ul');
      rows.append(...contacts.map(entry => {
        const item = document.createElement('li');
        const who = document.createElement('span');
        who.className = 'tilo-waiting-who';
        who.append(Object.assign(document.createElement('span'), {className: 'tilo-waiting-contact', textContent: entry.contact, title: entry.contact}),
          Object.assign(document.createElement('time'), {dateTime: new Date(entry.addedAtMs).toISOString(), textContent: addedAt(entry.addedAtMs)}));
        item.append(who, button('Invite', () => decide(() => invite(entry), entry.requestId)),
          button('Remove', () => decide(unlist, entry.requestId)));
        return item;
      }));
      listed.append(title, about, rows);
    };
    const decide = async (choice, requestId) => {
      error.hidden = true;
      try { await choice(requestId); } catch (refusal) { said(refusal); }
      await show();
    };
    form.append(heading, list, listed, error, close);
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
  // What the page says when something stops it, in the arrival screen's
  // language once its locale file is in.
  let messages = new Map();
  void localeMessages().then(loaded => { messages = loaded; });
  /** Where an embed's state goes: the site the web host names as its embedder, once the entry is read. */
  let embedder = null;
  const tell = state => { if (embed && embedder && parent !== window) parent.postMessage({type: 'tilo-embed', state}, embedder); };
  let joinNow = false;
  try { joinNow = sessionStorage.getItem(JOIN_NOW) === '1'; sessionStorage.removeItem(JOIN_NOW); } catch { /* No storage: watch first. */ }
  /** How this page entered through a host's door: `joined`, `watching` (until a gesture) or `seatless`. */
  let entered = null;
  let retryAfter = null;
  /** Why the door had no seat for this page (`full`, `too-many`, `unavailable`), when it watches instead. */
  let seatlessBecause = null;
  /** The workspace whose door this page watches through. */
  let visitedAt = null;
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
  /** The peer the host let in, once it did: it knows the room's name as its people last set it. */
  let admitted = null;
  const diagnostics = {
    onHostAction: (id, value) => {
      if (id === SET_UP.id) void setUp?.();
      else if (id === OPEN_IN_TAB) openInTab?.();
      else if (id.startsWith(OPEN_WORKSPACE)) void switchWorkspace?.(id);
      else if (workspaceActions?.handles(id)) void workspaceActions.perform(id);
      else void identityActions?.perform(id, value);
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
        status.textContent = localized(messages, 'arrival-failed-stopped-drawing', STOPPED_DRAWING);
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
    onAdmitted: peer => { admitted = peer; },
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
      visitedAt = workspace;
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
        displayName = chosenDisplayName(identity?.userPublicKey ?? kept?.userPublicKey) ?? rejoinedName() ?? defaultDisplayName(identity?.userPublicKey ?? kept?.userPublicKey);
        const peer = await joinViaHost({workspace, identity, displayName, runtime, onPairingCode: showPairingCode});
        entered = 'joined';
        return peer;
      } catch (error) {
        if (!seatless(error)) throw error;
        entered = 'seatless';
        retryAfter = error.retryAfter;
        seatlessBecause = error.code;
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
      displayName = chosenDisplayName(identity.userPublicKey) ?? defaultDisplayName(identity.userPublicKey);
      if (kept.owner) {
        // On a machine of their own the owner enters any of its rooms
        // (the host admits this device as its owner); the host refuses
        // one that does not exist.
        const lobby = await connectPeer({origin, identity, runtime, displayName, live: false});
        try { await lobby.workspaces.enter(room); } finally { await lobby.dispose?.(); }
      }
      // This tab's own device key: a new one where other tabs hold the
      // others. A room admits each device once, through an invitation (or
      // as its host's owner, above), so in a room joined by invitation a new
      // device of the person cannot get in by itself: said plainly, and the
      // key, which no host has seen, is not kept.
      const custody = await openDeviceCustody({profile: identityProfile(identity, origin), storage: idbStorage()});
      if (custody.created && !kept.owner) {
        await custody.destroy();
        throw new CustodyError('custody-busy', 'A new device of this person needs an invitation to this room');
      }
      const peer = await withCustody(custody, () => connectPeer({origin, room, identity, runtime, displayName, custody}));
      keepJoinedHost({origin, room, userPublicKey: identity.userPublicKey});
      return peer;
    },
    onInvite: () => { void workspaceActions?.perform(INVITE.id); },
    pairDevice: async ({origin, room, userPublicKey, invite, workspace}) => {
      displayName = chosenDisplayName(userPublicKey) ?? defaultDisplayName(userPublicKey);
      let waiting = () => {};
      try {
        return await pairViaHost({origin, room, userPublicKey, invite, workspace, runtime, displayName,
          label: deviceLabel(), onPairingCode: code => { waiting(); waiting = pairingWait(code); }});
      } catch (error) {
        // The host's answer, said as what to do next; anything else as any failure.
        const said = pairingFailure(error);
        if (said) throw new AdmissionError('pairing', said);
        throw error;
      } finally {
        waiting();
      }
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
      // The account names the profile's first device; a tab that took
      // another of its device keys is that device, with its own namespace.
      return withCustody(custody, () => connectPeer({origin, room, userPublicKey, profile, custody,
        device: custody.slot ? profileDevice(profile, globalThis.localStorage, custody.slot) : device,
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
    if (entered) void showVisitState({entered, retryAfter, because: seatlessBecause, workspace: visitedAt, workspaceName, tell, status});
    // Only the desktop lists host actions: a single app's mount refuses them.
    const hostActions = mode === 'desktop' && page.mount?.setHostActions && !embed
      ? hostActionParts(page.mount, ['identity', 'machine', 'workspaces', 'workspace', 'tab']) : null;
    const here = page.router.route.target.scope;
    // The host that answers at the peer's origin now: rooms remembered there
    // under another host went with that host's state.
    const hostDevice = page.peer ? hostDeviceOf(page.peer.host?.hostCert) : null;
    if (hostDevice) remembering(() => forgetGoneRooms({origin: page.peer.origin, hostDevice}));
    if (!embed && !entered && !viaTicket && page.peer && here.kind === 'room') {
      // A room entered as a member: offered again from the person's other workspaces.
      const room = {origin: here.host, room: here.room ?? page.peer.room};
      remembering(() => {
        rememberEntered({...room, hostDevice, name: invitedTo});
        if (!rememberedRoom({kind: 'room', host: room.origin, room: room.room})?.name) unnamed = room;
      });
    }
    if (hostActions) {
      const offered = remembering(() => workspaceChoices({home: page.entry?.home, homeRoom: homeRoomOf(page.entry),
        entered: enteredWorkspaces(), current: here})) ?? [];
      hostActions('workspaces').setHostActions(workspaceSwitchActions(offered, {current: workspaceName}));
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
    // A drawn name someone else here already goes by is drawn again; a
    // name the person chose stays theirs. A page with no host to join has
    // no peer and nobody to share a name with.
    page.peer?.setNameDrawer?.((current, lines) => {
      const key = page.peer.userPublicKey;
      if (chosenDisplayName(key)) return null;
      const redrawn = redrawnDisplayName(key, current, lines);
      if (redrawn) displayName = redrawn;
      return redrawn;
    });
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
    // Whether the home's door lets only its members in (`admission` in its
    // presence): then its bare link admits nobody new, and inviting someone
    // is an invitation of their own.
    let inviteOnlyHome = false;
    const homeDoor = page.entry?.home?.admission;
    if (homeDoor) {
      void fetch(new URL('presence', homeDoor)).then(response => (response.ok ? response.json() : null))
        .then(presence => { inviteOnlyHome = presence?.admission === 'invite'; }, () => {});
    }
    // Only a desktop lists host actions and shows banners.
    workspaceActions = hostActions ? createWorkspaceActions({
      peer: page.peer,
      machines: () => (person() ? ownedHosts(person()) : []),
      lobby: async origin => connectPeer({origin, identity: await rootOfThisPage(page.peer), runtime, displayName, live: false}),
      inviteLink: invitation => {
        const url = roomLink(page.peer.origin, page.peer.room);
        return `${url}${new URL(url).hash.includes('?') ? '&' : '?'}invite=${invitation}`;
      },
      // A public workspace's link is its bare entry: anyone opening it joins
      // through the door, as a guest.
      publicLink: () => (!inviteOnlyHome && remembering(() => isPublicHome({home: page.entry?.home, homeRoom: homeRoomOf(page.entry),
        scope: here.kind === 'room' ? {...here, room: here.room ?? page.peer?.room} : null})) ? page.entry.base : null),
      open: async ({origin, room, name, hostDevice: holder}) => {
        keepJoinedHost({origin, room, userPublicKey: (await rootOfThisPage(page.peer)).userPublicKey});
        remembering(() => rememberEntered({origin, room, name, hostDevice: holder}));
        // Creating a room leaves through the same save-and-close path as switching.
        await openWorkspace({choice: {origin, room, name}, page, status});
      },
      ui: {
        askWorkspaceName: save => askLine({label: 'Name of the new workspace', action: 'Create', save}),
        chooseMachine: origins => card('Create the workspace on', [...origins.map(origin => [new URL(origin).host, origin]), ['Cancel', null]]),
        share: shareCard,
        copy: text => navigator.clipboard.writeText(text).then(() => true, () => false),
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
          if (known) void settleRoom(known, runtime);
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
    // The page's channel state as text (whom each room follows, what waits
    // for keys, what was refused), for tests and tools reading why a
    // record did not arrive. Read only: it hands out no handle on the peer.
    globalThis.tiloChannels = () => page.peer?.channelDiagnostics?.() ?? '';
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
    // Stopped after the host let the person in: opening the desktop failed.
    const inside = stoppedInside(admitted, root.arrival);
    arrive('stopped');
    // Plain words and a way to try again (index.html); the cause, thrown
    // on, reaches the console and the host's error report.
    // The room's name as its people last set it, where the room's desktop
    // reached the page; else the name the page was served or remembered.
    const named = (typeof admitted?.workspaceName === 'string' && admitted.workspaceName) || workspaceName;
    const messages = await localeMessagesSoon();
    const said = failureMessage(error, {workspace: named, loaded: Boolean(runtime), admitted: inside, messages});
    // A refusal is the host's answer, which trying again does not change.
    // Marked together with its words: a page that says it was refused
    // while still reading "Loading Tilo…" tells a reader nothing.
    document.documentElement.dataset.entry = error instanceof AdmissionError || refusedByHost(error) ? 'refused' : 'failed';
    status.textContent = said;
    // Turned away for coming without an invitation: a place to open the
    // one they get.
    if (error?.code === 'invitation-only') offerDoor({messages, named, error, workspace: visitedAt ?? admitting?.entry?.home});
    const known = admitting && remembering(() => rememberedRoom(admitting.scope));
    // A declined or unusable invitation, a damaged link or this browser's
    // device key held by another tab says nothing about the room.
    const local = error?.name === 'CustodyError' || (error instanceof AdmissionError && !['no-access', 'refused', 'host-changed', 'removed'].includes(error.code));
    if (known && !local) {
      // A room this browser was a member of: forgotten when its origin says
      // another host holds it now; else said what is known, and no longer
      // reopened on a bare entry.
      const settled = await settleRoom(known, runtime);
      if (settled === 'gone') {
        status.textContent = `${known.name ?? 'This workspace'} no longer exists: its host was set up again since this browser opened it.`;
      } else if (error instanceof AdmissionError && error.code === 'no-access') {
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
    // Turned away at the door of a room taking people by invitation: its
    // answer, shown above, not a failure of this page to report as an
    // uncaught error (every stranger at such a door would send one).
    if (error?.code === 'invitation-only') return;
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

/** Set for the one reload after signing in at the door, to the person
 * signed in as: refused again, the door says that identity is not in yet. */
const DOOR_SIGNED_IN = 'tilo-door-signed-in';

/**
 * On a page turned away for coming without an invitation, the door
 * (index.html, `#arrival-door`). Where the host keeps a wait list
 * (`error.door`), it apologises that Tilo is invite-only for now and asks
 * for an email to invite (`leaveOnWaitList`); else it names the workspace
 * `named` and asks for the invite link someone sends. Under either: signing
 * in with an identity the person already has (a passkey, then the page
 * loads again and joins as them, and a member gets in), and the field to
 * paste an invite link into, opened in this tab. Their own desktop waits at
 * the foot of the page (`offerOwnDesktop`).
 */
function offerDoor({messages, named, error, workspace}) {
  const door = document.getElementById('arrival-door');
  const form = document.getElementById('arrival-invite');
  const toggle = document.getElementById('arrival-invite-toggle');
  const field = document.getElementById('arrival-invite-link');
  const problem = document.getElementById('arrival-invite-error');
  if (!door || !form || !toggle || !field || !problem) return;
  const say = (key, english, args) => localized(messages, key, english, args);
  const waitList = Boolean(error?.door?.waitList && workspace?.admission);
  const copy = doorCopy({workspace: named, waitList, messages});
  document.getElementById('arrival-door-heading').textContent = copy.heading;
  document.getElementById('arrival-door-body').textContent = copy.body;
  const own = document.getElementById('arrival-own');
  if (own) {
    own.textContent = say('arrival-door-own', 'Or try Tilo on your own');
    door.querySelector('.door-foot')?.append(own);
  }
  if (waitList) offerWaitList({workspace, contact: error.door.contact, messages});
  offerSignIn({workspace, named, messages});
  // The invite link: behind a small toggle beside the wait list; shown
  // where it is the only way in.
  toggle.textContent = say('arrival-invite-toggle', 'Have an invite link?');
  field.placeholder = say('arrival-invite-placeholder', 'Paste an invite link');
  field.setAttribute('aria-label', say('arrival-invite-link', 'Invite link'));
  form.querySelector('button').textContent = say('arrival-invite-open', 'Open');
  form.hidden = waitList;
  toggle.addEventListener('click', () => {
    form.hidden = !form.hidden;
    toggle.setAttribute('aria-expanded', String(!form.hidden));
    if (!form.hidden) field.focus();
  });
  form.addEventListener('submit', event => {
    event.preventDefault();
    const target = inviteLinkTarget(field.value);
    problem.textContent = target ? '' : say('arrival-invite-not-a-link', 'That isn’t an invite link. Paste the whole link you were sent.');
    if (target) loadPage(target);
  });
  document.documentElement.dataset.door = waitList ? 'wait-list' : 'invite';
}

/** The door's wait list: an email field that leaves the address at the
 * host's door, the address confirmed once it took it, and a note on what the
 * address is for and where to be taken off the list (`contact`). */
function offerWaitList({workspace, contact, messages}) {
  const say = (key, english, args) => localized(messages, key, english, args);
  const wait = document.getElementById('arrival-wait');
  const email = document.getElementById('arrival-wait-email');
  const note = document.getElementById('arrival-wait-note');
  const done = document.getElementById('arrival-wait-done');
  const submit = wait?.querySelector('button');
  if (!wait || !email || !note || !done || !submit) return;
  email.placeholder = say('arrival-wait-placeholder', 'you@example.com');
  email.setAttribute('aria-label', say('arrival-wait-email', 'Email'));
  submit.textContent = say('arrival-wait-join', 'Join the wait list');
  const privacy = () => {
    note.classList.remove('door-problem');
    note.removeAttribute('role');
    const used = say('arrival-wait-privacy', 'We only use your email to send you an invite.');
    if (!contact) {
      note.textContent = used;
      return;
    }
    // The contact is a link inside the localized sentence: split at it.
    const marker = '\u0000';
    const [before, after = ''] = say('arrival-wait-removal', 'To be taken off the list, write to { $contact }.', {contact: marker}).split(marker);
    const page = contact.startsWith('https://');
    const link = Object.assign(document.createElement('a'), {
      textContent: page ? contact.slice('https://'.length) : contact,
      href: page ? contact : `mailto:${contact}`, rel: 'noopener',
    });
    if (page) link.target = '_blank';
    note.replaceChildren(`${used} ${before}`, link, after);
  };
  privacy();
  email.addEventListener('input', () => {
    if (email.getAttribute('aria-invalid') !== 'true') return;
    email.removeAttribute('aria-invalid');
    privacy();
  });
  wait.addEventListener('submit', async event => {
    event.preventDefault();
    if (submit.disabled) return;
    if (!email.value.trim()) {
      email.focus();
      return;
    }
    submit.disabled = true;
    try {
      const kept = await leaveOnWaitList({workspace, email: email.value});
      done.textContent = say('arrival-wait-done', 'You’re on the list. We’ll email you at { $email }.', {email: kept});
      document.documentElement.dataset.waitListed = '';
      privacy();
    } catch (refusal) {
      console.warn('wait list:', refusal?.message ?? refusal);
      note.textContent = waitListFailure(refusal?.code, messages);
      note.classList.add('door-problem');
      note.setAttribute('role', 'alert');
      email.setAttribute('aria-invalid', 'true');
      email.focus();
    } finally {
      submit.disabled = false;
    }
  });
}

/**
 * Signing in at the door with an identity the person already has: their
 * passkey opens it (its envelope from the hosts that keep it), and the page
 * loads again to join as them. A member gets in; anyone else is told this
 * identity is not in `named` yet, with the wait list still there.
 */
function offerSignIn({workspace, named, messages}) {
  const say = (key, english, args) => localized(messages, key, english, args);
  const row = document.getElementById('arrival-sign-in');
  const button = document.getElementById('arrival-sign-in-button');
  const note = document.getElementById('arrival-sign-in-note');
  if (!row || !button || !note) return;
  document.getElementById('arrival-sign-in-ask').textContent = say('arrival-sign-in-ask', 'Already have a Tilo identity?');
  button.textContent = say('arrival-sign-in', 'Sign in');
  // A browser without passkeys cannot open an identity kept with one.
  if (typeof globalThis.PublicKeyCredential !== 'function' || !navigator.credentials?.get) {
    row.hidden = true;
    return;
  }
  let signedIn = null;
  try {
    signedIn = sessionStorage.getItem(DOOR_SIGNED_IN);
    sessionStorage.removeItem(DOOR_SIGNED_IN);
  } catch { /* No storage: nothing to say about a sign-in. */ }
  if (signedIn && signedIn === currentIdentity()?.userPublicKey) {
    note.textContent = named
      ? say('arrival-sign-in-not-member', 'You’re signed in, but this identity isn’t in { $workspace } yet. Join the wait list, or ask someone in it for an invite.', {workspace: named})
      : say('arrival-sign-in-not-member-unnamed', 'You’re signed in, but this identity isn’t in this workspace yet. Ask someone in it for an invite.');
  }
  button.addEventListener('click', async () => {
    button.disabled = true;
    note.textContent = '';
    note.classList.remove('door-problem');
    try {
      const origins = [...new Set([workspace?.host?.origin, ...membershipOrigins(),
        ...listedWorkspaces({home: workspace}).map(listed => listed.host.origin)].filter(Boolean))];
      const identity = await signInWithPasskey({fetchEnvelope: root => fetchEnvelope(root, origins)});
      try { sessionStorage.setItem(DOOR_SIGNED_IN, identity.userPublicKey); } catch { /* Said less, nothing lost. */ }
      location.reload();
    } catch (error) {
      console.warn('signing in at the door:', error);
      button.disabled = false;
      // Closing the passkey prompt is the person's own answer.
      if (error?.code === 'passkey-cancelled' || ['NotAllowedError', 'AbortError'].includes(error?.name)) return;
      note.classList.add('door-problem');
      note.textContent = ['no-envelope', 'not-tilo', 'no-wrap'].includes(error?.code)
        ? say('arrival-sign-in-unknown', 'That passkey doesn’t open a Tilo identity this site knows. Use one you made on Tilo, or open Tilo where you already use it.')
        : say('arrival-sign-in-failed', 'Signing in didn’t work in this browser. Try again, or open Tilo where you already use it.');
    }
  });
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
 * What a visitor let in through a host's door sees beyond the workspace,
 * said as the door's presence has it (`visitLine`): as a watching embed, the
 * invitation to join on the first gesture (captured, so the spectator
 * surface never sees it), unless only invitees get in or nobody joins for
 * now; without a seat, that they watch live and are let in when one frees
 * up (asked after the host's Retry-After, with jitter so a full room is not
 * asked by everyone at once).
 */
async function showVisitState({entered, retryAfter, because = null, workspace, workspaceName, tell, status}) {
  if (entered === 'joined') { tell('joined'); return; }
  tell('watching');
  const embed = entered === 'watching';
  const offerJoin = () => {
    status.textContent = 'Click or type to join';
    const gesture = event => {
      for (const type of ['pointerdown', 'keydown']) removeEventListener(type, gesture, true);
      event.preventDefault();
      event.stopPropagation();
      status.textContent = 'Joining…';
      joinByReload();
    };
    for (const type of ['pointerdown', 'keydown']) addEventListener(type, gesture, true);
  };
  const [presence, messages] = await Promise.all([doorPresence(workspace), localeMessagesSoon()]);
  const {line, join} = visitLine({entered, because, presence, workspace: workspaceName, messages});
  if (join === 'gesture') { offerJoin(); return; }
  status.textContent = line;
  if (join === 'seat') joinWhenSeated({workspace, retryAfter, join: embed ? offerJoin : joinByReload});
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
    pause: inputPause(), searchChord: mac ? '⌘K' : 'Ctrl+K', showPairLink: pairLinkSheet, askPairing: pairingAsk,
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
 * link that opens this workspace there as a new device of this person,
 * answering `invite`, and the host's requests waiting for this root to
 * approve them.
 */
function pairingOf({peer, identity, runtime, link}) {
  if (!peer?.origin || !peer.room || !runtime?.pendingPairings || !link) return null;
  const post = httpsPost(peer.origin);
  return {
    link: invite => {
      const url = link.format({mode: 'desktop', target: {scope: {kind: 'room', host: peer.origin, room: peer.room}}});
      return `${url}${new URL(url).hash.includes('?') ? '&' : '?'}pair=${pairSecret(identity.userPublicKey, invite)}`;
    },
    pending: async () => JSON.parse(await runtime.pendingPairings(post, identity)),
    answer: (digest, approved) => runtime.answerPairing(post, identity, digest, approved),
  };
}
