/**
 * The entry pages' wiring: the real page host and admission entries behind
 * `entry.mjs`, and the few controls a link needs (an invitation to accept,
 * an account to choose, the other mode). Diagnostic query switches stay
 * readable (`?workspace=`, `?workspaceName=`, `?invite`), since they say nothing
 * about what the link opens.
 *
 * `?embed=1` is the page inside another site (the homepage's live
 * preview): it shows nothing of the workspace until the visitor's first
 * click, tap or key press, then reloads and joins as this browser's own
 * person, as the site itself would. A person who already joined that host
 * comes back at once. The embedding page, if it is the web host's embedder
 * (`entry.json` `embedder`), hears `{type: 'tilo-embed', state}`: `idle`
 * until the page joined (`joined`), and again after a failure, when the site
 * shows its static fallback. A full workspace answers the entry with
 * `503 Retry-After`, and the page waits on the reconnect screen until it is admitted
 * like anyone else.
 */
import {AdmissionError} from './admission.mjs';
import {refusalCopy, entryLinkTarget, localeMessages, localeMessagesSoon, localized, refusedByHost, startArrival, stoppedInside, visitorLocale} from './arrival.mjs';
import {servedVersion} from './bundle-update.mjs';
import {mountWaitList} from './wait-list.mjs';
import {boot, grammar} from './entry.mjs';
import {readEntry} from './entry.mjs';
import {loadRuntime} from './host.mjs';
import {connectPeer, httpsPost, identityProfile, inspectInvitation, invitedServer, joinWorkspace, openInvitation, profileDevice, withDeviceKey} from './peer.mjs';
import {readBrowserAccounts} from './browser-accounts.mjs';
import {idbStorage, openDeviceKey} from './device-key.mjs';
import {setupOrigin} from './identity-actions.mjs';
import {showRecoveryWords} from './recovery-words.mjs';
import {firstScreen, openSignIn, setupDone} from './first-screen.mjs';
import {fetchPersonProfile} from './profile-photos.mjs';
import {deviceLabel, ofThisSite, PAIRING_SIGNING_IN, PAIRING_WAIT, PAIRING_WHOSE, pairingFailure, pairingStatus, pairingWhere, pairingWhose} from './pairing.mjs';
import {browserIdentity, browserPerson, currentIdentity, keepRecoveryWordsShown, keptIdentity, recoveryWords, recoveryWordsShown} from './identity.mjs';
import {createReconnect, describeError, domReconnectView} from './reconnect.mjs';
import {domFailureView, failureCause, failureDetails, failureScreen, namedOrigin, unanswered} from './failure-screen.mjs';
import {hostUnreachable, refusalText, rejoinedName, retryUnreachable} from './session-refusal.mjs';
import {deviceRemoved} from './session-refusal.mjs';
import {sameEntry} from './entered-workspaces.mjs';
import {isInvitedServer, invitedOrigin, nameInvitedServer, siteNames, siteServer} from './site-host.mjs';
import {chooseWorkspace, chosenDisplayName, chosenWorkspace, entryWorkspace, enteredWorkspaces, hostDeviceOf, joinViaHost, keepJoinedHost, keptMembership, listedWorkspaces, publicListings, ownDesktopOpen, pairViaHost, rememberEntered, settleFailedWorkspace, unmarkOwnPairing, workspaceAt, workspaceChoices} from './workspaces.mjs';
import {initial, shownName} from './person.mjs';
import {hex} from './hex.mjs';
import {memberText} from './json-text.mjs';

/** Set for the one reload that joins an embed's visiting page as the person. */
const JOIN_NOW = 'tilo-embed-join';

/** The host's certificate, as the page's site names it (`site-host.mjs`) or else as the host
 * says; a workspace that just started refuses briefly. */
async function hostCertificate(runtime, origin, {timeoutMs = 15_000} = {}) {
  const named = siteServer(origin);
  if (named) return named.info.hostCert;
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

/** How long the host at a remembered workspace's origin gets to say who answers there. */
const HOST_CHECK_MS = 5_000;

/** Whether the remembered workspace `known` is gone, asked of its origin after
 * opening it failed (`settleFailedWorkspace`): `deleted`, `gone`, `unavailable`
 * or `null`. */
async function settleWorkspace(known, runtime) {
  const post = origin => {
    if (!runtime) throw new Error('no runtime to ask the host with');
    return httpsPost(origin, (url, init) => globalThis.fetch(url, {...init, signal: AbortSignal.timeout(HOST_CHECK_MS)}));
  };
  const hostInfo = async origin => JSON.parse(await runtime.hostInfo(post(origin))).hostCert;
  const deleted = async (origin, workspace) => runtime.workspaceDeleted(post(origin), workspace);
  try {
    return await settleFailedWorkspace(runtime, known, {hostInfo, deleted});
  } catch (error) {
    console.warn('workspaces: this browser cannot remember them', error);
    return null;
  }
}

/** A question over the page with its answers as buttons; resolves to the
 * value of the one taken. `name` marks the card (`data-card`), for tests and
 * tools driving the page to tell which question it asks. */
function card(text, choices, name) {
  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card';
    if (name) form.dataset.card = name;
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

/** What each role lets someone do, in the desktop role chooser's words
 * (`workspace-settings-role-<role>-preview`), so an invitation says what it
 * gives, not only its name. */
const ROLE_WORDS = {
  owner: 'Can manage people, permissions, and workspace settings.',
  administrator: 'Can do everything here: edit shared work, invite and remove people, make other administrators, and change the workspace’s name and settings.',
  moderator: 'Can edit shared work and remove other people’s messages.',
  editor: 'Can view, comment on, and edit shared work.',
  commenter: 'Can view shared work and add comments, but cannot edit it.',
  viewer: 'Can view shared work, but cannot comment or edit it.',
};

/** The colour an initial is drawn in on `background`, `#rrggbb`: black or
 * white, whichever has more contrast, by WCAG relative luminance. */
function readableOn(background) {
  const channels = [1, 3, 5].map(at => parseInt(background.slice(at, at + 2), 16) / 255)
    .map(channel => channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4);
  const luminance = 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
  // Where black and white read alike (contrast 4.58:1 each way).
  return luminance > 0.179 ? '#101114' : '#ffffff';
}

const element = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

/** The name the invitation card shows for who sent it: the one the workspace
 * signed into the invitation, which is its name for them; else the one their
 * profile keeps, which the machine serving the workspace at `origin` answers
 * by their key, as the desktop names someone not here to say theirs. Empty
 * where nobody named them yet: the card says the person is invited in plain
 * words, never a name it made up. A key of theirs is never a name: it says
 * what it is, not who they are. */
async function inviterName(inviter, origin) {
  const named = shownName(inviter?.name);
  if (named) return named;
  const user = inviter?.userPublicKey;
  if (typeof user !== 'string') return '';
  const profile = typeof origin === 'string' && origin
    ? await fetchPersonProfile(origin, user).catch(error => {
      console.warn('the inviter’s profile was not read', error?.message ?? error);
      return null;
    })
    : null;
  return shownName(profile?.display_name);
}

/** What an invitation offers, before the person joins: the workspace it is
 * for, who it is from and how many people are in it, and what the role lets
 * them do. `inviterColour` is the presence colour of who made it, where the
 * page has one for them. Resolves to whether they join. An invitation sent
 * off the wait list shows no card: the person asked to get in, and joins at
 * once (admission.mjs). */
function invitationCard(offer, inviterColour) {
  return new Promise(resolve => {
    const form = element('form', 'tilo-entry-card tilo-invite');
    form.dataset.card = 'invite';
    form.setAttribute('role', 'dialog');
    form.setAttribute('aria-labelledby', 'tilo-invite-workspace');
    // One sentence, read top to bottom: who asked, then the workspace as
    // the card's heading.
    const from = element('p', 'tilo-invite-from');
    const inviter = shownName(offer.inviter?.name);
    if (inviter) {
      // The picture a person sets is not the workspace's to hand to someone
      // who is not in it yet; their initial is, as the desktop draws who it
      // has no picture of, in the colour presence gives them everywhere
      // else the desktop shows them.
      const face = element('span', 'tilo-invite-face', initial(inviter));
      face.setAttribute('aria-hidden', 'true');
      if (typeof inviterColour === 'string' && /^#[0-9a-f]{6}$/i.test(inviterColour)) {
        face.style.background = inviterColour;
        face.style.color = readableOn(inviterColour);
      }
      const says = element('span', 'tilo-invite-says');
      says.append(element('strong', 'tilo-invite-name', inviter), ' invited you to');
      from.append(face, says);
    } else {
      from.append('You’re invited to');
    }
    // An invitation read on its own (a server reached only over iroh) does not carry the
    // workspace's name: the server says it once the person is in.
    const workspace = element('h2', 'tilo-invite-workspace', offer.workspaceName || (inviter ? 'their workspace' : 'a workspace'));
    workspace.id = 'tilo-invite-workspace';
    const head = element('div', 'tilo-invite-head');
    head.append(from, workspace);
    if (Number.isInteger(offer.members) && offer.members > 0) {
      head.append(element('p', 'tilo-invite-people', offer.members === 1 ? '1 person' : `${offer.members} people`));
    }
    form.append(head);
    // The role as the desktop's role chooser shows one: its name, and
    // beneath it what it lets them do.
    const role = typeof offer.role === 'string' ? offer.role : '';
    if (ROLE_WORDS[role]) {
      const panel = element('div', 'tilo-invite-role');
      panel.append(element('p', 'tilo-invite-role-name', role[0].toUpperCase() + role.slice(1)),
        element('p', 'tilo-invite-role-words', ROLE_WORDS[role]));
      form.append(panel);
    }
    form.append(answers(form, resolve, 'Join'));
    document.body.append(form);
  });
}

/** The invitation card's two answers: `join`, named for what it does, and
 * Not now. Either closes the card. */
function answers(form, resolve, join) {
  const actions = element('div', 'tilo-entry-actions');
  for (const [label, joins] of [[join, true], ['Not now', false]]) {
    const button = element('button', joins ? 'tilo-entry-primary' : '', label);
    button.type = 'button';
    button.addEventListener('click', () => { form.remove(); resolve(joins); });
    actions.append(button);
  }
  return actions;
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
 * Asked on a device before it asks a host to make it a person's: whose
 * device it becomes (`whose`, the sentence of `pairingWhose`), which server
 * adds it (`where`, the sentence of `pairingWhere`), what follows from
 * that, what it means for a browser that is somebody already (`already`),
 * and the two ways on. Stop has the focus, so a stray Enter pairs nothing.
 * Resolves to whether the person went on.
 */
function pairingAgree(whose, where, {already = false} = {}) {
  return new Promise(resolve => {
    const card = pairCard('pair-whose', PAIRING_WHOSE.heading, 'alertdialog');
    const actions = document.createElement('div');
    actions.className = 'tilo-entry-actions';
    const go = document.createElement('button');
    go.type = 'button';
    go.className = 'tilo-entry-primary';
    go.textContent = PAIRING_WHOSE.go;
    const stop = document.createElement('button');
    stop.type = 'button';
    stop.textContent = PAIRING_WHOSE.stop;
    const answer = agreed => { card.remove(); resolve(agreed); };
    go.addEventListener('click', () => answer(true));
    stop.addEventListener('click', () => answer(false));
    actions.append(go, stop);
    card.append(paragraph(whose, 'tilo-pair-whose'), paragraph(where, 'tilo-pair-whose'), paragraph(PAIRING_WHOSE.consequence),
      ...(already ? [paragraph(PAIRING_WHOSE.already)] : []), actions);
    document.body.append(card);
    stop.focus();
  });
}

/** Said on a new device while the person's other device decides on it:
 * which device that is, the code it shows beside its ask, whose device this
 * one becomes and which server adds it (`whose` and `where`, as it said
 * before it asked), and what to do when nothing asks (`PAIRING_WAIT`). Its
 * Stop is the way out (`onStop`). */
function pairingWait(code, whose, where, onStop) {
  document.documentElement.dataset.pairingCode = code;
  const card = pairCard('pair-wait', PAIRING_WAIT.heading, 'status');
  const stop = document.createElement('button');
  stop.type = 'button';
  stop.textContent = PAIRING_WHOSE.stop;
  stop.addEventListener('click', () => onStop());
  card.append(paragraph(PAIRING_WAIT.where), paragraph(code, 'tilo-pair-digits'),
    ...[whose, where].filter(Boolean).map(said => paragraph(said, 'tilo-pair-whose')),
    paragraph(PAIRING_WAIT.then, 'tilo-pair-note'), paragraph(PAIRING_WAIT.otherwise, 'tilo-pair-note'), stop);
  document.body.append(card);
  return () => card.remove();
}

/** Said on a tab its person signed in with their agent while it enters as
 * theirs: nothing asks the person anything, since their own computer
 * signed it in already, so the tab shows no code and points at no other
 * device. */
function pairingSigningIn() {
  const card = pairCard('pair-own', PAIRING_SIGNING_IN, 'status');
  document.body.append(card);
  return () => card.remove();
}

/** Leave a pairing that waits: the address loses its link and the page
 * loads again as the bare site. The host hears the page's request go with
 * it, so nobody can approve this device any more. */
function leavePairing() {
  const bare = new URL(location.href);
  bare.hash = '';
  bare.search = '';
  location.assign(bare.href);
  location.reload();
}

/** The page's uncaught errors, for the entry its entry names (`<entry>errors`):
 * the site's host logs them, and sizes and rate-limits them there. Five a
 * page load at most, and none of them waits for an answer. */
/** The most a report's body holds: the entry reads no more
 * (`MAX_PAGE_ERROR_BYTES` in tilo-server-protocol). */
export const MAX_REPORT_BYTES = 4096;

/** A page error report's body, cut to fit `MAX_REPORT_BYTES`: the stack
 * goes first, then the message, so the cause the page showed always
 * arrives. */
export function reportBody(fields) {
  const report = {...fields, message: String(fields.message ?? ''), stack: String(fields.stack ?? '')};
  const size = () => new TextEncoder().encode(JSON.stringify(report)).length;
  for (const field of ['stack', 'message', 'source', 'page']) {
    while (size() > MAX_REPORT_BYTES && report[field]) {
      report[field] = report[field].slice(0, Math.floor(report[field].length / 2));
    }
  }
  return JSON.stringify(report);
}

/** Send `body` to `url`: a request that outlives the page, which every
 * browser sends. Not a beacon first: LibreWolf and hardened Firefox keep
 * `navigator.sendBeacon` with beacons turned off (`beacon.enabled`), where
 * it answers that it sent and sends nothing. A beacon only where the page
 * has no fetch. */
export function sendReport(url, body, {navigator = globalThis.navigator, fetch = globalThis.fetch} = {}) {
  try {
    if (typeof fetch === 'function') {
      void Promise.resolve(fetch(url, {method: 'POST', body, keepalive: true, mode: 'no-cors', credentials: 'omit',
        headers: {'content-type': 'text/plain'}})).catch(() => {});
      return;
    }
  } catch { /* Refused at once: a beacon below. */ }
  try {
    navigator?.sendBeacon?.(url, new Blob([body], {type: 'text/plain'}));
  } catch { /* Reporting must never throw. */ }
}

function reportErrors() {
  let entry = null;
  let sent = 0;
  /** Errors the page already reported with their cause (`failed`), which
   * reach the window again only because the page throws them on. */
  const told = new WeakSet();
  const report = (kind, message, stack, at = {}) => {
    if (!entry || sent++ >= 5) return;
    try {
      sendReport(new URL('errors', entry), reportBody({
        kind, message, source: at.source ?? '', line: at.line ?? 0, column: at.column ?? 0,
        stack: stack || '', page: location.pathname, build: PAGE_BUILD,
      }));
    } catch { /* Reporting must never throw. */ }
  };
  const fresh = error => !(error && typeof error === 'object' && told.has(error));
  addEventListener('error', event => { if (fresh(event.error)) report('error', event.message, event.error?.stack,
    {source: event.filename, line: event.lineno, column: event.colno}); });
  addEventListener('unhandledrejection', event => { if (fresh(event.reason)) report('unhandledrejection', event.reason?.message ?? event.reason, event.reason?.stack); });
  return {
    at: next => { if (!entry && typeof next === 'string' && next) entry = next; },
    report,
    reported: error => { if (error && typeof error === 'object') told.add(error); },
  };
}

/**
 * Right after this browser's person is first let in anywhere: their 24 words
 * are the only way back into Tilo if they lose every device, so they are
 * shown once, before anything else opens, until the person confirms they kept
 * them. The words are derived from the root here and never leave this
 * browser. A browser that keeps no person (a tab paired to someone's, a guest)
 * holds no root to write down, and shows none.
 */
async function showWordsOnce(identity) {
  if (!identity?.recoveryWords) return;
  if (await recoveryWordsShown().catch(() => false)) return;
  await showRecoveryWords(recoveryWords(identity));
  await keepRecoveryWordsShown().catch(() => {});
}

export async function startEntryPage({mode}) {
  const status = document.getElementById('status');
  const query = new URLSearchParams(location.search);
  const embed = query.get('embed') === '1';
  const loading = status.textContent;
  // The step of getting in the page shows (`data-arrival` on the root, see
  // index.html): `load`ing Tilo, `join`ing through a host's entry, `open`ing
  // the workspace's page, `stopped` when it cannot go on. Each step says itself in the
  // status line, unless something else (admission, a visit) is said there;
  // the arrival screen's line shows that note (arrival.mjs).
  const root = document.documentElement.dataset;
  const opening = mode === 'app' ? 'Opening the app…' : 'Opening the workspace…';
  const notes = {open: opening};
  const arrive = step => {
    if (root.arrival === 'done' && step !== 'stopped') return;
    // Let in after all (an identity this browser kept): the workspace opens over the site's entry.
    if (step === 'open' && 'front' in root) {
      for (const name of ['front', 'refusal', 'waitListed']) delete root[name];
    }
    // A page that cannot go on has no workspace to set an agent up in: the
    // first screen says so rather than wait.
    if (step === 'stopped') firstScreen?.unavailable({code: 'stopped'});
    root.arrival = step;
    if (notes[step] && [loading, opening].includes(status.textContent)) status.textContent = notes[step];
  };
  // The arrival screen's line and bar follow those steps (arrival.mjs).
  startArrival();
  // Someone new at the site itself, whose home keeps a wait list: its
  // entry, at once, while the runtime loads behind it for signing in.
  if (mode === 'desktop' && !embed) void offerFront();
  // What the page says when something stops it, in the arrival screen's
  // language once its locale file is in.
  let messages = new Map();
  void localeMessages().then(loaded => { messages = loaded; });
  /** The reconnect screen (`reconnect.mjs`), while the host does not let
   * the page in yet: one for every reason the page waits. */
  const reconnect = createReconnect({view: domReconnectView({messages: () => messages, workspace: () => workspaceName,
    onTryNow: () => reconnect.tryNow()})});
  /** What this page did not catch, for the site's host to log. */
  const pageErrors = reportErrors();
  /** The page's one failure screen (failure-screen.mjs). */
  let failureReported = false;
  const failureView = domFailureView({messages: () => messages, onRetry: () => location.reload()});
  /** The host the page was reaching when `error` stopped it: the one the
   * error names, else the workspace's. */
  const failingHost = error => (typeof error?.origin === 'string' && error.origin) || namedOrigin(error) || visitedAt?.host?.origin
    || (admitting?.scope?.kind === 'workspace' ? admitting.scope.host : null) || admitting?.entry?.home?.host?.origin || null;
  /**
   * Stop on the failure screen: `error`'s cause in plain words, the action
   * that gets the person in, their other workspaces, and the details. The
   * site's host hears the cause too (`<entry>errors`).
   */
  const showFailure = async ({error, settled = null, admitted: inside = false, drawn = false, step = null, workspace = null, messages: words = messages}) => {
    const host = failingHost(error);
    const cause = failureCause(error, {settled, admitted: inside, drawn, online: navigator.onLine !== false,
      siteAnswers: unanswered(error) ? await siteAnswers() : true, hostIsSite: host === location.origin});
    const screen = failureScreen(cause, {workspace, host, step, error, messages: words});
    const others = embed || mode !== 'desktop' || !admitting?.entry ? [] : await otherWorkspaces({...admitting, runtime});
    failureView.show({screen, others, details: failureDetails({cause, host, step, error, build: PAGE_BUILD})});
    // Offline, the page tries again by itself once the browser is back.
    if (cause === 'offline') addEventListener('online', () => location.reload(), {once: true});
    // Once a page: a desktop that stops draws its failure, and the mount
    // that throws for the same cause redraws it.
    if (!failureReported) pageErrors.report('failed', `${cause}: ${describeError(error)}`, error?.stack, {source: host ?? ''});
    failureReported = true;
  };
  /** Where an embed's state goes: the site the web host names as its embedder, once the entry is read. */
  let embedder = null;
  const tell = state => { if (embed && embedder && parent !== window) parent.postMessage({type: 'tilo-embed', state}, embedder); };
  let joinNow = false;
  try { joinNow = sessionStorage.getItem(JOIN_NOW) === '1'; sessionStorage.removeItem(JOIN_NOW); } catch { /* No storage: no gesture asked yet. */ }
  /** Whether this page got in through a host's entry as the browser's own person (rather than by an invitation or an account). */
  let enteredViaEntry = false;
  /** Whether a guest ticket spent on this page's pairing code let it in: a visitor of the workspace, not a member. */
  let guestTicket = false;
  /** The workspace whose entry this page was let in through, or waits at. */
  let visitedAt = null;
  let displayName;
  /** The workspace this page is getting into, by name, once it knows it. */
  let workspaceName = query.get('workspaceName') || null;
  /** The workspace the link names and the entry it is served under, once read (`deps.onScope`). */
  let admitting = null;
  /** The person's own workspace, made as a wait-list invitation let them in (`deps.ownWorkspace`). */
  let ownFirst = null;
  /** The peer the host let in, once it did: it knows the workspace's name as its people last set it. */
  let admitted = null;
  /** Each server's public directory, as `deps.listings` read it, by origin. */
  const listingsRead = new Map();
  let runtime;
  /** Said while one of the person's devices decides on this one. */
  const showPairingCode = code => {
    document.documentElement.dataset.pairingCode = code;
    status.textContent = pairingStatus(code);
  };
  // A browser an earlier page paired to its own person is its own person
  // again before anything asks who it is here (`unmarkOwnPairing`). Asked now
  // and awaited with the runtime, which admission waits for anyway, so the
  // page starts no later for it.
  const ownAgain = unmarkOwnPairing(keptIdentity).catch(error => {
    console.warn('pairing: this browser stays marked as paired to its own person', error);
    return [];
  });
  /**
   * `pair(userPublicKey, signedIn)` for a sign-in with the person's agent
   * (`signInAs`): this tab becomes a device of `userPublicKey` through the
   * host of `listed` (the workspace it is in or entering), with the
   * certificate their device signed it in with and the key it asked with
   * (`signedIn`). It enters on the page it is on, so the one load the
   * sign-in takes is the workspace it opens; `null` where that page knows no
   * host to enter through.
   */
  function pairSignIn(listed) {
    if (!listed?.host?.origin) return null;
    return (userPublicKey, signedIn) => deps.pairDevice({origin: listed.host.origin,
      workspace: listed.workspace ?? null, userPublicKey, signedIn, listed});
  }
  /**
   * Hold the page the entry would have handed over (`entry.mjs`,
   * `holdFirstScreen`) while a first visit's first screen is in play: release
   * the screen now that the site let the person in (`reveal`), offer the codes
   * its buttons copy, and, when the person leaves it (skipped, or their agent
   * is in), open the workspace that is theirs — never the shared workspace
   * the entry admitted this browser to behind the screen. Resolves to whether
   * the entry was held.
   */
  function holdFirstScreen(page) {
    if (!firstScreen || embed) return false;
    const peer = page.peer;
    firstScreen.reveal();
    const identity = currentIdentity();
    const origin = setupOrigin({peer, home: page.entry?.home});
    const listed = peer ? workspaceAt(listedWorkspaces({home: page.entry?.home}), peer.origin) : null;
    if (identity && peer?.workspace && origin && peer.userPublicKey === identity.userPublicKey) {
      offerFirstScreenCodes({origin, identity, runtime, link: grammar(runtime, page.entry), home: homeAdmissionWorkspace(page.entry),
        pair: pairSignIn(listed)});
    } else {
      firstScreen.unavailable({code: 'no-codes'});
    }
    firstScreen.whenDone(async ({signedIn}) => {
      // A sign-in loads the page as the person (`signInAs`): nothing of this
      // tab's is opened.
      if (signedIn) return setupDone();
      const own = peer ? await askOwnWorkspace(peer) : null;
      setupDone();
      if (own) {
        // Chosen before the setup is over, so a bare entry opens this
        // workspace from now on rather than the site's home.
        remembering(() => chooseWorkspace({origin: own.origin, workspace: own.id}));
        rememberPageWorkspace({workspace: own.id, name: own.name});
        globalThis.location.assign(`${page.entry.base}w/${own.id}`);
        return;
      }
      globalThis.location.assign(page.url);
    });
    return true;
  }
  /**
   * Keep the workspace `peer` was admitted to as one this browser entered as a
   * member: an invitation it redeemed, an account it holds or a device it
   * paired. A bare entry reopens it, and admission comes back to it as the
   * person this browser is rather than asking someone signed in nowhere to
   * sign in again. A guest a site's entry or a ticket let in is no member of
   * the workspace, and is offered back by nothing.
   */
  function rememberEnteredWorkspace(peer) {
    if (embed || enteredViaEntry || guestTicket || !peer?.origin || !peer?.workspace) return;
    const hostDevice = hostDeviceOf(peer.host?.hostCert);
    if (!hostDevice) return;
    remembering(() => rememberEntered(runtime, {origin: peer.origin, workspace: peer.workspace, hostDevice,
      name: workspaceName ?? peer.workspaceName ?? null}));
  }
  /** The runtime with the peer's code loaded: what reads an invitation, asks a
   * server what it offers, and joins (`../src/split.rs`). Loading it is the
   * page entering a workspace, never its first screen. */
  const peerRuntime = async () => (runtime = await loadRuntime('peer'));
  const deps = {
    loadRuntime: async () => {
      runtime = await loadRuntime();
      await ownAgain;
      return runtime;
    },
    // Whether someone is signed in on this browser: an identity unlocked, one
    // it keeps, or a person whose device it became on the link's host (signed
    // in with their agent). Asked before any join, so a link nobody here can
    // open asks them to sign in rather than make a stranger of them.
    holdsPerson: async ({origin} = {}) => Boolean(currentIdentity()
      || (origin && remembering(() => keptMembership({host: {origin}}))?.paired)
      || await keptIdentity().catch(() => null)),
    // The public workspaces a server lists (its directory), read once a page:
    // whether a workspace's own entry lets anyone in, and the wait list one
    // keeps, before anything joins.
    listings: origin => {
      const at = new URL(origin).origin;
      if (!listingsRead.has(at)) listingsRead.set(at, publicListings(at));
      return listingsRead.get(at);
    },
    onAdmitted: async peer => {
      admitted = peer;
      // Their own workspace opened in place of the invitation's: named as itself.
      if (ownFirst && peer?.workspace === ownFirst.id) workspaceName = ownFirst.name ?? workspaceName;
      letIn(peer);
      // Right after they are let in: the 24 words that are the only way back
      // to this person, shown once before the workspace opens.
      await showWordsOnce(currentIdentity());
    },
    // The web version is the page the server renders, and the entry hands the
    // browser to it once it is a device of the workspace. It tells the page
    // which workspace that is — the name the host answered admission with,
    // which no address carries — and a site that frames this page hears what
    // it hears from a page it mounted.
    onPageFirst: peer => {
      rememberPageWorkspace({workspace: peer?.workspace ?? null, name: workspaceName ?? peer?.workspaceName ?? null});
      // This browser is in the workspace now: kept as one it entered, so the
      // next visit comes back to it as this person.
      rememberEnteredWorkspace(peer);
      if (embed && enteredViaEntry) tell('joined');
    },
    // A first visit sets itself up before it is in any workspace: the entry
    // keeps the browser on its first screen, holding the page it would have
    // handed over (`url`), until the person is done or skips, then opens the
    // person's own workspace — or, where the host keeps none, the page it
    // held. `page` is `{url, workspace, entry, peer}`.
    holdFirstScreen: page => holdFirstScreen(page),
    // An embed always shows the workspace the embedding site names.
    chosenWorkspace: () => (embed ? null : remembering(() => chosenWorkspace())),
    onScope: next => {
      admitting = next;
      embedder = next.entry?.embedder ?? null;
      // This site's entry: where its pages report what they did not catch.
      pageErrors.at(next.entry?.home?.admission);
      const known = remembering(() => rememberedWorkspace(next.scope));
      if (known?.name && !workspaceName) workspaceName = known.name;
    },
    ownDesktopOpen: home => ownDesktopOpen(home),
    // A workspace of this host the browser entered before, such as the
    // person's own: reconnected to, not asked for at the host's entry.
    entered: ({origin, workspace}) => enteredWorkspaces().some(kept => sameEntry(kept, {origin, workspace})),
    enterViaHost: async ({workspace}) => {
      workspaceName = workspace.name || workspaceName;
      visitedAt = workspace;
      // An embed joins nothing before the visitor does something: it takes no
      // admission and makes no person, and its site shows its static fallback
      // until the first click, tap or key press (`{idle: true}`: nothing
      // mounts). A browser that already joined this host comes in at once.
      if (embed && !joinNow && !keptMembership(workspace)) {
        tell('idle');
        document.documentElement.dataset.entry = 'idle';
        joinOnFirstGesture(status);
        return {idle: true};
      }
      // A device paired to a person kept elsewhere reconnects as them,
      // and never makes a person of its own.
      const kept = keptMembership(workspace);
      const identity = kept?.paired ? undefined : await browserPerson();
      // The first screen's codes need only the person and their host, not
      // the workspace's session: made now, while the entry lets them in. A
      // first visit the first paint held (`firstScreen.held`) has none made
      // until it is in: a site taking people by invitation only may send it
      // to its wait list, and no code is owed to a visitor it turns away.
      if (firstScreen && !firstScreen.held && identity && !embed) {
        offerFirstScreenCodes({origin: workspace.host.origin, identity, runtime, link: admitting?.link,
          home: homeAdmissionWorkspace(admitting?.entry), pair: pairSignIn(workspace)});
      }
      // A page that came back here by itself because the host turned it
      // away (session-refusal.mjs) joins under the name it went by.
      displayName = chosenDisplayName(identity?.userPublicKey ?? kept?.userPublicKey) ?? rejoinedName();
      // A workspace with no capacity right now (full, or this address asking too
      // often) is waited out on the reconnect screen, as long as its entry
      // asked: the page joins as the same person once there is workspace.
      const peer = await joinViaHost({workspace, identity, displayName, runtime, onPairingCode: showPairingCode,
        wait: next => reconnect.wait({...next, origin: workspace.host.origin, build: PAGE_BUILD,
          retryAfterMs: Number.isFinite(next.error?.retryAfter) ? next.error.retryAfter * 1000 : 0})});
      reconnect.done();
      enteredViaEntry = true;
      return peer;
    },
    // A peer in the home workspace while the page is in another workspace, which
    // carries the desktop's problem reports to the home's channel. Only for
    // a person the home already let in, as themselves (a paired device's
    // person approves new devices elsewhere): nobody joins the home just to
    // report. It connects with this tab's device key for the home's host (a
    // second one where the workspace's peer holds the first), without calls
    // or a live session, gives up rather than waiting at the entry, and is kept
    // while the page is.
    homePeer: async home => {
      const kept = remembering(() => keptMembership(home));
      if (!kept || kept.paired) return null;
      const identity = currentIdentity() ?? await browserIdentity({create: false}).catch(() => null);
      if (!identity || identity.userPublicKey !== kept.userPublicKey) return null;
      runtime ??= await loadRuntime();
      const name = chosenDisplayName(identity.userPublicKey);
      return joinViaHost({workspace: home, identity, displayName: name, runtime: await peerRuntime(), live: false, voice: false,
        wait: () => Promise.reject(new Error('the home workspace did not let this device in'))});
    },
    inspectInvitation: async (origin, invitation) => inspectInvitation(origin, invitation, {runtime: await peerRuntime()}),
    // An invitation a link names no other server beside, whose server the page's own site does
    // not name either, is that server's own word on where it is: a server reached only over iroh
    // (behind a NAT, through a packet relay) is dialled as the invitation says, never asked over
    // HTTP, and what it offers is read from the invitation.
    invitedServer: async (invitation, origin) => {
      if (new URL(origin).origin !== globalThis.location.origin && !siteServer(origin)) return null;
      runtime = await peerRuntime();
      let invited;
      try {
        invited = await invitedServer(invitation, {runtime});
      } catch (error) {
        console.warn('admission: the invitation names no server to dial', error?.message ?? error);
        return null;
      }
      if (!invited) return null;
      // The server the site names (its home workspace's host), however the invitation names it:
      // dialled at the site's origin for it, so every link this person hands out from then on
      // names a host any browser reaches.
      const named = siteNames(invited.info);
      // Asked there what the invitation offers, as a short link's is: the server
      // answers the workspace's name, its people and whether it sent the invitation off its
      // wait list, which the invitation alone does not say.
      if (named) return named === new URL(origin).origin ? null : {origin: named, offer: null};
      const at = invitedOrigin(invited.endpoint);
      if (!at) return null;
      nameInvitedServer(at, invited.info);
      return {origin: at, offer: invited.offer};
    },
    // A link's `invite` names the invitation its workspace keeps at its host:
    // the page asks for it before anything reads it.
    openInvitation: async (origin, workspace, secret) => openInvitation(origin, workspace, secret, {runtime: await peerRuntime()}),
    // The invite sheet this page's desktop shows, when the page is in a
    // workspace: made and listed through that workspace's host, in links
    // that open this page.
    invite: ({peer, entry}) => peer?.workspace && !embed ? {base: entry.base, origin: peer.origin,
      workspace: peer.workspace, name: displayName ?? '', user: peer.userPublicKey ?? '',
      host: hex(peer.host?.hostCert?.user_public_key ?? [])} : null,
    confirmInvitation: (offer, origin) => {
      workspaceName = offer.workspaceName || workspaceName;
      const inviter = offer.inviter?.userPublicKey;
      const colour = typeof inviter === 'string' ? runtime?.presenceColor?.(inviter) : undefined;
      // The workspace signs the name it knows for who asked. When it knows
      // none, the card waits for the name their profile keeps.
      const signed = shownName(offer.inviter?.name);
      // A server reached only over iroh has no web origin to ask their profile of.
      if (!offer.inviter || signed || isInvitedServer(origin)) return invitationCard(offer, colour);
      return inviterName(offer.inviter, origin).then(name =>
        invitationCard({...offer, inviter: {...offer.inviter, name}}, colour));
    },
    joinWorkspace: async options => {
      const identity = await browserPerson();
      // A person joins under the name they chose here, else under none: they
      // show as a person without a name until they give one.
      displayName = options.displayName || chosenDisplayName(identity?.userPublicKey);
      const peer = await joinWorkspace({...options, identity, runtime, displayName});
      // Kept, so the next visit reconnects as the person let in.
      keepJoinedHost({origin: options.origin, workspace: peer.workspace, userPublicKey: identity.userPublicKey});
      return peer;
    },
    reconnect: async ({origin, workspace}) => {
      // A device paired to a person kept elsewhere goes on as that device,
      // under the certificate it holds: into a workspace its key was
      // admitted to (the person's own, say), never as a new device.
      const paired = remembering(() => keptMembership({host: {origin}}));
      if (paired?.paired) {
        const profile = identityProfile(paired, paired.origin);
        const deviceKey = await openDeviceKey({profile, storage: idbStorage(), requireExisting: true});
        displayName = chosenDisplayName(paired.userPublicKey);
        return withDeviceKey(deviceKey, () => connectPeer({origin, workspace, userPublicKey: paired.userPublicKey,
          profile, deviceKey, displayName, runtime, requireExistingCertificate: true}));
      }
      const identity = currentIdentity() ?? await browserIdentity({create: false});
      if (!identity) {
        console.warn('reconnect: this browser holds no person to reconnect to this host as');
        return null;
      }
      // A person signed in here comes back as themself, whether or not this
      // browser kept this host (signed in just now, from a link to it): the
      // host says whether they are in the workspace.
      displayName = chosenDisplayName(identity.userPublicKey);
      // This tab's own device key, which may be one the workspace never
      // admitted: a new key where other tabs hold the others, or one this
      // browser's other pages took for this host while the person entered
      // the workspace on another. Whether the person is in the workspace is
      // the host's to say, by their root, not this browser's: the device
      // enters through the workspace's entry on its session, which lets a
      // device it admitted straight back in and has any other device of a
      // member sign its own row there, the owner's on a machine of their own
      // too.
      const deviceKey = await openDeviceKey({profile: identityProfile(identity, origin), storage: idbStorage()});
      const entry = new URL(`directory/${workspace}/`, `${origin}/`).href;
      const entered = await withDeviceKey(deviceKey, () => joinWorkspace({origin, entry, identity, runtime, displayName, deviceKey}));
      keepJoinedHost({origin, workspace, userPublicKey: identity.userPublicKey});
      return entered;
    },
    // An embed lists no host actions, so it has nothing to invite with:
    // the desktop offers no Invite there rather than one that does nothing.
    pairDevice: async ({origin, workspace, userPublicKey, ticket = null, signedIn = null, listed}) => {
      displayName = chosenDisplayName(userPublicKey);
      let waiting = () => {};
      // Whose device this becomes and which server adds it, as its person
      // was told before anything was asked, and whether they stopped while
      // it waited.
      let whose = '';
      let where = '';
      let stopped = false;
      // A tab its person's computer signed in (`signInAs`) is theirs: they
      // asked for it on this page, so they are not asked whose device this
      // becomes.
      const own = Boolean(signedIn);
      try {
        // The person this browser already is, where it keeps one: the one
        // this tab opened, else the one it keeps (a tab that opens a link
        // has opened nobody yet). It is told so, never paired to them.
        const holds = currentIdentity()?.userPublicKey ?? (await keptIdentity().catch(() => null))?.rootPublicKey ?? null;
        return await pairViaHost({origin, workspace, userPublicKey, ticket, signedIn, listed, runtime, displayName, holds,
          label: deviceLabel(),
          // The link wrote the server it names. Only one this site names as
          // its own (its `entry.json`, read before anything is asked) is
          // asked who the link's person is; any other names nobody.
          ofSite: ofThisSite(origin, {page: location.href, entry: admitting?.entry}),
          agree: who => {
            if (own) return true;
            whose = pairingWhose(who.name, who);
            where = pairingWhere(who.origin, who);
            return pairingAgree(whose, where, {already: Boolean(who.holds)});
          },
          stopped: () => stopped,
          onPairingCode: code => {
            waiting();
            waiting = own ? pairingSigningIn() : pairingWait(code, whose, where, () => { stopped = true; leavePairing(); });
          }});
      } catch (error) {
        // The host's answer, or this browser's own, said as what to do next; anything else as any failure.
        const said = pairingFailure(error);
        if (said) throw new AdmissionError('pairing', said);
        throw error;
      } finally {
        waiting();
      }
    },
    readAccounts: async origin => readBrowserAccounts({origin, runtime: await loadRuntime('accounts'), currentDevice: profile => profileDevice(profile)}),
    chooseAccount: accounts => card('Open this link as',
      accounts.map(account => [account.displayName || account.userPublicKey.slice(0, 12), account]), 'accounts'),
    // A guest ticket: a fresh device of the workspace's root, signed in once the
    // web host spent the ticket on this page's sign-in ticket. The root key is
    // read from the host's own certificate, so the link need not carry it.
    // The visitor is a person without a name until they give one; their
    // device goes by what it is, never as their name.
    redeemTicket: async ({origin, workspace, ticket, tickets}) => {
      guestTicket = true;
      const {connectServerPeer} = await import('./server.mjs');
      const hostCert = await hostCertificate(await loadRuntime(), origin);
      const admit = new URL(`${encodeURIComponent(workspace)}/admit`, tickets).href;
      return connectServerPeer({origin, workspace, userPublicKey: hex(hostCert.user_public_key), label: deviceLabel(),
        onSignIn: async signIn => {
          const response = await fetch(admit, {method: 'POST', headers: {'content-type': 'application/json'},
            body: JSON.stringify({ticket, signIn})});
          if (!response.ok) {
            console.warn('guest link:', (await response.json().catch(() => ({}))).error ?? response.status);
            status.textContent = 'This guest link didn’t let this browser in. Ask whoever sent it for a new one.';
          }
        }});
    },
    connectMember: async ({origin, workspace, account}) => {
      const {userPublicKey, profile, device} = account;
      const deviceKey = await openDeviceKey({profile, storage: idbStorage(), requireExisting: true});
      // The account names the profile's first device; a tab that took
      // another of its device keys is that device, with its own namespace.
      return withDeviceKey(deviceKey, () => connectPeer({origin, workspace, userPublicKey, profile, deviceKey,
        device: deviceKey.slot ? profileDevice(profile, globalThis.localStorage, deviceKey.slot) : device,
        runtime, requireExistingCertificate: true}));
    },
  };
  /**
   * The person's own workspace, for someone a wait-list invitation just let
   * into `peer`'s workspace (admission.mjs): the server makes it with this
   * device in it, and the page enters it in place of the one the invitation
   * let them into, which stays among theirs. `null` where the server keeps
   * none for them: they stay where they were let in.
   */
  deps.ownWorkspace = async peer => {
    try {
      ownFirst = await ensureOwnWorkspace(peer);
      // What a bare entry reopens from now on, kept as soon as it exists: someone who closes the
      // tab while it opens comes back to it, not to the wait list.
      remembering(() => chooseWorkspace({origin: ownFirst.origin, workspace: ownFirst.id}));
      return {workspace: ownFirst.id, name: ownFirst.name};
    } catch (error) {
      console.warn('wait-list invitation: your own workspace was not made', error?.code ?? error?.message ?? error);
      return null;
    }
  };

  try {
    await boot({mode, status, deps, onStep: arrive});
  } catch (error) {
    // A host that did not answer before the page got in (it is starting,
    // or restarting) is asked again on the reconnect screen, with backoff,
    // the tries counted across the reloads: nothing the host refused.
    if (!admitted && hostUnreachable(error)) {
      console.warn('admission: the host did not answer yet, trying again', error?.message);
      const {attempt, since} = retryUnreachable();
      const origin = failingHost(error) ?? location.origin;
      await reconnect.wait({error, attempt, since, origin, build: PAGE_BUILD,
        retryAfterMs: Number.isFinite(error?.retryAfter) ? error.retryAfter * 1000 : 0});
      location.reload();
      return;
    }
    if (embed) tell('idle');
    // Stopped after the host let the person in: opening the desktop failed.
    const inside = stoppedInside(admitted, root.arrival);
    // How far the page got, for the failure screen to say where it stopped.
    const step = root.arrival;
    // The workspace's name as its people last set it, where the workspace's desktop
    // reached the page; else the name the page was served or remembered.
    const named = (typeof admitted?.workspaceName === 'string' && admitted.workspaceName) || workspaceName;
    const messages = await localeMessagesSoon();
    // This browser was removed from its person's devices: its server's own word at the
    // handshake, which nothing here undoes. Said in the words an open page says it in.
    if (deviceRemoved(error)) {
      error = Object.assign(new AdmissionError('device-revoked', refusalText('device-revoked', named || null)), {cause: error});
      document.documentElement.dataset.refusal = 'device-revoked';
    }
    // A refusal is the host's answer, which trying again does not change.
    arrive('stopped');
    document.documentElement.dataset.entry = error instanceof AdmissionError || refusedByHost(error) ? 'refused' : 'failed';
    // Turned away for coming without an invitation: the entry is a page of
    // its own, with its own ways in, and nothing else is shown over it. No
    // desktop speaks behind it, so it speaks the visitor's own language.
    // Nobody signed in here, at a link to a workspace no entry lets a
    // stranger into: the same page, asking them to sign in, and the link
    // opens as them once they did (`sign-in` in admission.mjs).
    if (error?.code === 'sign-in' && admitting?.scope?.kind === 'workspace') {
      const language = visitorLocale();
      const messages = await localeMessagesSoon(1500, globalThis, language);
      const {host, workspace} = admitting.scope;
      // Its own name where this browser kept one, never the site's home's.
      const linked = {name: error.listing?.name ?? remembering(() => rememberedWorkspace(admitting.scope))?.name ?? null,
        host: {origin: host}, workspace, admission: error.entry ?? undefined};
      // Someone with no Tilo identity is offered the wait list, as the
      // site's front page offers it: the workspace's own, else the home's.
      const waitList = await signInWaitList({listing: error.listing, host, workspace, home: admitting.entry?.home, listings: deps.listings});
      offerRefusal({messages, language, named: linked.name, error, workspace: linked, runtime, link: admitting.link,
        home: homeAdmissionWorkspace(admitting.entry), pair: pairSignIn(linked), signIn: true, waitList});
      status.textContent = `${document.getElementById('arrival-refusal-heading')?.textContent ?? ''}. ${document.getElementById('arrival-refusal-body')?.textContent ?? ''}`;
      return;
    }
    if (error?.code === 'invitation-only') {
      const language = visitorLocale();
      const messages = await localeMessagesSoon(1500, globalThis, language);
      const screen = failureScreen('invitation-only', {workspace: named, messages});
      status.textContent = `${screen.heading}. ${screen.body}`;
      // The workspace whose entry turned this page away, and the site's home
      // it is: a sign-in with the person's agent pairs here and opens a
      // workspace of theirs, never the home it was not let into.
      const turnedAway = visitedAt ?? admitting?.entry?.home ?? null;
      offerRefusal({messages, language, named, error, workspace: turnedAway, runtime, link: admitting?.link,
        home: homeAdmissionWorkspace(admitting?.entry) ?? entryWorkspace(turnedAway), pair: pairSignIn(turnedAway)});
      // Its answer, not a failure of this page to report as an uncaught
      // error (every stranger at such an entry would send one).
      return;
    }
    const known = admitting && remembering(() => rememberedWorkspace(admitting.scope));
    // A declined or unusable invitation, a damaged link or this browser's
    // device key held by another tab says nothing about the workspace.
    const local = error?.name === 'DeviceKeyError' || (error instanceof AdmissionError && !['no-access', 'refused', 'host-changed', 'removed'].includes(error.code));
    // A workspace this browser was a member of: forgotten when its origin says
    // another host holds it now, and no longer reopened on a bare entry.
    const settled = known && !local ? await settleWorkspace(known, runtime) : null;
    await showFailure({error, settled, admitted: inside, step, workspace: named ?? known?.name ?? null, messages});
    // The cause reaches the console and, once, the site's host (above):
    // thrown on, it is not reported a second time as uncaught.
    pageErrors.reported(error);
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

/** Keep the workspace and the name the page the entry hands the browser to
 * shows (`page.mjs`), which no address carries. */
function rememberPageWorkspace({workspace, name}) {
  try {
    sessionStorage.setItem('tilo-page-workspace', JSON.stringify({workspace: workspace ?? null, name: name ?? null}));
  } catch { /* No storage: the page names no workspace. */ }
}

/** The remembered workspace `scope` names, if this browser entered it as a member. */
function rememberedWorkspace(scope) {
  if (scope?.kind !== 'workspace' || !scope.workspace) return null;
  const origin = new URL(scope.host).origin;
  return enteredWorkspaces().find(entry => entry.origin === origin && entry.workspace === scope.workspace) ?? null;
}

/** The workspace the site's own entry names, as its admission address spells it
 * (`<server>/peer/v1/directory/<workspace>/`): the site's home. */
/** The wait list a sign-in screen at `workspace` on `host` offers someone
 * with no Tilo identity: the one the workspace's own `listing` names, else
 * the one the site's `home` workspace keeps, as its server's directory
 * (`listings`) lists it; `{workspace, contact, mail}`, `workspace` the
 * listing whose entry keeps it, or `null` where neither keeps one. */
async function signInWaitList({listing, host, workspace, home, listings}) {
  if (listing?.waitList) return {workspace: {host: {origin: host}, workspace, admission: listing.entry, name: listing.name}, ...listing.waitList};
  const origin = home?.host?.origin;
  const id = homeAdmissionWorkspace({home});
  if (!origin || !id) return null;
  const kept = await Promise.resolve().then(() => listings(origin)).then(all => all.find(listed => listed.workspace === id), () => null);
  return kept?.waitList ? {workspace: {...home, workspace: id, admission: kept.entry}, ...kept.waitList} : null;
}

function homeAdmissionWorkspace(entry) {
  const admission = entry?.home?.admission;
  const match = typeof admission === 'string' ? admission.match(/\/directory\/([^/]+)\/?$/) : null;
  return match ? match[1] : null;
}

/** The site's home workspace: the one its sign-up address names, else the one
 * this browser was first let into on the home's host. The workspace kept for
 * that host is not the home when an invitation into another of its workspaces
 * was the person's way in. */
function homeWorkspaceOf(entry) {
  if (!entry?.home) return null;
  return homeAdmissionWorkspace(entry) ?? keptMembership(entry.home)?.workspace ?? null;
}

/** The address that opens `choice` (`workspaceChoices`): the bare entry for the home. */
function choiceUrl(choice, entry, link) {
  if (choice.home) return entry.base;
  if (choice.own) return link.format({mode: 'desktop', target: {scope: {kind: 'own'}}});
  return link.format({mode: 'desktop', target: {scope: {kind: 'workspace', host: choice.origin, workspace: choice.workspace}}});
}

/**
 * Load `url` as a new page. Only the fragment differs between workspaces under
 * one entry, so that is written as a new history entry and the page loads
 * again: every workspace is admitted by a page of its own.
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

/** The bundle version this page runs, for the reconnect screen's details. */
const PAGE_BUILD = servedVersion(import.meta.url)?.version ?? 'development';

/**
 * On a page turned away for coming without an invitation, the entry
 * (index.html, `#arrival-refusal`). Where the host keeps a wait list
 * (`error.entry`), it apologises that Tilo is invite-only for now and asks
 * for an email to invite (`offerWaitList`); else it names the workspace
 * `named` and asks for the invite link someone sends. Under either: signing
 * in with an identity the person already has (their agent, then the page
 * loads again and joins as them, and a member gets in), and the field to
 * paste an invite or pairing link into, opened in this tab.
 */
function offerRefusal({messages, language, named, error, workspace, runtime, link, home = null, pair = null, signIn = false, waitList: offered = null}) {
  const entry = document.getElementById('arrival-refusal');
  // The first screen of a first visit is not for someone the entry turns
  // away: it asks them to set an agent up in a workspace they are not in. It
  // leaves now, not after its fade, and whatever the page drew of it while it
  // loaded goes with it: the entry is what this visit is. Its setup code is
  // no longer watched for an agent either: this person approves no device
  // at a host that turned them away, and asking would only be refused.
  firstScreen?.close();
  document.documentElement.removeAttribute('data-first-screen');
  const form = document.getElementById('arrival-invite');
  const toggle = document.getElementById('arrival-invite-toggle');
  const field = document.getElementById('arrival-invite-link');
  const problem = document.getElementById('arrival-invite-error');
  if (!entry || !form || !toggle || !field || !problem) return;
  const say = (key, english, args) => localized(messages, key, english, args);
  // The wait list the entry's refusal named, or the one a sign-in screen offers.
  const kept = offered ?? (error?.entry?.waitList && workspace?.admission
    ? {workspace, contact: error.entry.contact, mail: error.entry.mail === true} : null);
  const waitList = Boolean(kept);
  // The site's entry the page showed at once (`offerFront`) is this one: it
  // keeps its words and its wait list, and gains the ways to sign in.
  const shown = waitList && !signIn && 'front' in document.documentElement.dataset;
  const copy = refusalCopy({workspace: named, waitList, signIn, messages});
  if (!shown) {
    document.getElementById('arrival-refusal-heading').textContent = copy.heading;
    document.getElementById('arrival-refusal-body').textContent = copy.body;
  }
  if (kept && !shown) offerWaitList({workspace: kept.workspace, contact: kept.contact, mail: kept.mail === true, messages, language});
  // Signing in from a link comes back to the link; at the site's entry, to a workspace of their own.
  offerSignIn({workspace, messages, runtime, link, home, pair, stay: signIn});
  // The link that lets someone in, an invitation or a pairing from a device
  // of theirs: behind a small toggle beside the wait list; shown where it is
  // the only way in.
  toggle.textContent = say('arrival-invite-toggle', 'Have an invite or pairing link?');
  field.placeholder = say('arrival-invite-placeholder', 'Paste an invite or pairing link');
  field.setAttribute('aria-label', say('arrival-invite-link', 'Invite or pairing link'));
  form.querySelector('button').textContent = say('arrival-invite-open', 'Open');
  form.hidden = waitList;
  toggle.addEventListener('click', () => {
    form.hidden = !form.hidden;
    toggle.setAttribute('aria-expanded', String(!form.hidden));
    if (!form.hidden) field.focus();
  });
  form.addEventListener('submit', event => {
    event.preventDefault();
    const target = entryLinkTarget(field.value);
    problem.textContent = target ? '' : say('arrival-invite-not-a-link', 'That isn’t an invite or pairing link. Paste the whole link.');
    if (target) loadPage(target);
  });
  document.documentElement.dataset.refusal = waitList ? 'wait-list' : 'invite';
}

/**
 * A first visit to the site itself (`data-first-screen-hold`: this browser
 * kept no membership, no workspace and no setup, and the address names no
 * workspace), at a site whose home workspace takes people by invitation and
 * keeps a wait list: the site's entry (`#arrival-refusal`), what Tilo is and
 * the wait list, as soon as the site's `entry.json` and its server's
 * directory answer, instead of after the runtime loaded and the home's
 * entry refused it. The page goes on loading behind it: once the entry
 * refuses it, `offerRefusal` adds the ways to sign in (`data-front` keeps them
 * hidden until then); a browser let in after all opens its desktop over it.
 * Resolves to whether it showed.
 */
async function offerFront() {
  const root = document.documentElement;
  if (!('firstScreenHold' in root.dataset)) return false;
  try {
    const {home} = await readEntry({mode: 'desktop', location});
    if (!home?.admission) return false;
    const listing = (await publicListings(home.host.origin))
      .find(listed => home.admission.includes(`/directory/${encodeURIComponent(listed.workspace)}/`));
    if (!listing?.invitationOnly || !listing.waitList) return false;
    const language = visitorLocale();
    const messages = await localeMessagesSoon(1000, globalThis, language);
    // The runtime got there first: the entry it showed stands.
    if (root.dataset.arrival === 'stopped' || root.dataset.arrival === 'done') return false;
    const copy = refusalCopy({waitList: true, messages});
    document.getElementById('arrival-refusal-heading').textContent = copy.heading;
    document.getElementById('arrival-refusal-body').textContent = copy.body;
    document.getElementById('status').textContent = `${copy.heading}. ${copy.body}`;
    root.dataset.refusal = 'wait-list';
    root.dataset.front = '';
    offerWaitList({workspace: home, contact: listing.waitList.contact, mail: listing.waitList.mail, messages, language});
    return true;
  } catch (error) {
    // The site's entry comes as before, once the runtime asked it.
    console.warn('the site’s entry was not read ahead of the runtime:', error?.message ?? error);
    return false;
  }
}

/** The entry's wait list (wait-list.mjs): an email field that leaves
 * the address at the host's entry, where it stands once it took it, this
 * browser's way to take it off again, and where questions go (`contact`).
 * An entry that mails (`mail`) sends links back to this page: one that
 * confirms the address (`?wait-list-confirm=`) or takes it off
 * (`?wait-list-remove=`), done here and taken out of the address bar. */
function offerWaitList({workspace, contact, mail, messages, language}) {
  const form = document.getElementById('arrival-wait');
  const elements = {form, submit: form?.querySelector('button')};
  for (const [name, id] of [['email', 'arrival-wait-email'], ['done', 'arrival-wait-done'], ['againLine', 'arrival-wait-again-line'],
    ['againPrompt', 'arrival-wait-again-prompt'], ['again', 'arrival-wait-again'], ['change', 'arrival-wait-change'],
    ['leave', 'arrival-wait-leave'], ['note', 'arrival-wait-note'], ['contact', 'arrival-wait-contact']]) {
    elements[name] = document.getElementById(id);
  }
  if (Object.values(elements).some(element => !element)) return;
  const url = new URL(location.href);
  const arrival = {confirm: url.searchParams.get('wait-list-confirm'), remove: url.searchParams.get('wait-list-remove')};
  if (arrival.confirm || arrival.remove) {
    url.searchParams.delete('wait-list-confirm');
    url.searchParams.delete('wait-list-remove');
    history.replaceState(history.state, '', url);
  }
  mountWaitList({elements, root: document.documentElement, workspace, contact, mail,
    arrival: arrival.confirm || arrival.remove ? arrival : null, messages, language});
}

/**
 * Signing in at the entry with an identity the person already has: a computer
 * of theirs vouches for this tab, which pairs here as their new device and
 * then opens a workspace of their own rather than the home workspace's shared
 * desktop. A member gets in; the wait list still stands for anyone else.
 */
export function offerSignIn({workspace, messages, runtime, link, home = null, pair = null, stay = false}) {
  const say = (key, english, args) => localized(messages, key, english, args);
  const row = document.getElementById('arrival-sign-in');
  const agent = document.getElementById('arrival-sign-in-agent');
  if (!row || !agent) return;
  document.getElementById('arrival-sign-in-ask').textContent = say('arrival-sign-in-ask', 'Already have a Tilo identity?');
  agent.textContent = say('arrival-sign-in-agent', 'Sign in with your agent');
  // With their agent: a computer of theirs vouches for this tab, which
  // pairs here as their new device and then opens their own workspace, past
  // the wait list.
  const origin = workspace?.host?.origin;
  const target = entryWorkspace(workspace);
  const signInWithAgent = origin && target && link && pair;
  agent.hidden = !signInWithAgent;
  row.hidden = !signInWithAgent;
  if (!signInWithAgent) return;
  const tickets = ticketCodes({origin: new URL(origin).origin});
  const signIn = claimed => signInAs(claimed, {origin, home, stay}, link, pair);
  agent.addEventListener('click', () => {
    const screen = openSignIn(document, {onBack: () => agent.focus()});
    screen?.offerAgentSignIn(async () => ({...await tickets.createSignIn(), signIn}));
  });
}

/** The person's other workspaces, as the failure screen lists them after
 * a failure to open `scope` (`failure-screen.mjs`): their home and the
 * workspaces this browser entered. */
async function otherWorkspaces({scope, entry, runtime}) {
  const link = runtime ? grammar(runtime, entry) : null;
  const choices = remembering(() => workspaceChoices({home: entry.home, homeWorkspace: homeWorkspaceOf(entry),
    entered: enteredWorkspaces(), current: scope, own: false})) ?? [];
  return choices.filter(choice => choice.home || choice.own || link).map(choice => ({
    // Named as the workspace switcher names it: it is no workspace.
    label: choice.own ? 'Your desktop' : choice.name,
    own: Boolean(choice.own),
    note: choice.unavailable ? 'Didn’t open last time' : null,
    open: () => {
      // The own desktop is no workspace a bare entry could reopen: the choice kept stays.
      if (!choice.own) remembering(() => chooseWorkspace(choice.home ? null : choice));
      loadPage(choice.own && !link ? new URL('#/me', entry.base).href : choiceUrl(choice, entry, link));
    },
  }));
}

/** Whether this site still answers, once the page stopped: a host that does
 * not answer while the site does is that host's problem, not the
 * connection's. */
async function siteAnswers(url = location.href) {
  try {
    await fetch(url, {method: 'HEAD', cache: 'no-store', credentials: 'omit', signal: AbortSignal.timeout(4000)});
    return true;
  } catch {
    return false;
  }
}

/** Reload this page to join the workspace it shows, as this browser's own person. */
function joinByReload() {
  try { sessionStorage.setItem(JOIN_NOW, '1'); } catch { /* A kept membership decides then. */ }
  location.reload();
}

/**
 * Ask the visitor to join: the embed's first click, tap or key press joins
 * as this browser's own person, on the page that loads for it. The gesture is
 * captured, so the visiting surface never sees it.
 */
function joinOnFirstGesture(status) {
  status.textContent = 'Click or type to join';
  const gesture = event => {
    for (const type of ['pointerdown', 'keydown']) removeEventListener(type, gesture, true);
    event.preventDefault();
    event.stopPropagation();
    status.textContent = 'Joining…';
    joinByReload();
  };
  for (const type of ['pointerdown', 'keydown']) addEventListener(type, gesture, true);
}

/**
 * The person's own workspace on the host of `peer`, asked for once
 * (`ensureOwnWorkspace`): `{id, name, origin}`, or null where the host keeps
 * none (agents are set up in `setUpIn`, the workspace this page is in) or did
 * not answer, in which case the next ask asks again.
 */
function askOwnWorkspace(peer) {
  ownWorkspaceAsked ??= ensureOwnWorkspace(peer).catch(error => {
    if (error?.code === 'unknown_action') {
      console.info('first screen: this host keeps no workspaces of people\'s own;', error?.message);
      setUpIn = peer.workspace;
    } else {
      console.warn('first screen: your own workspace', error);
      ownWorkspaceAsked = null;
    }
    return null;
  });
  return ownWorkspaceAsked;
}

/** Whether the first screen has its tickets offered. */
let firstScreenCodesOffered = false;
/** The workspace an agent is set up in when the host keeps none of the
 * person's own: the one this page is in. */
let setUpIn = null;
/** Resolves to the page's peer once it is in its workspace. */
let letIn;
const pageLetIn = new Promise(resolve => { letIn = resolve; });
/** How long signing in with an agent waits for this tab to be in its workspace. */
const LET_IN_MS = 30_000;

/**
 * The tickets a person's agent sets Tilo up with, for this tab's root on `origin`: `make()` hands
 * back the ticket the agent runs `tilo login` with, and the setup keeps it (`resume(saved)`
 * follows it again after a reload). Its `ours(saved)` says whether a kept ticket is this person's,
 * on this host.
 *
 * Setting up is a ticket this tab's host serves for the person's own workspace on that host,
 * which the person's agent's computer logs in with: the tab approves that computer with its root.
 * The ticket names the workspace when one is named (the one in front, where the person connects
 * an agent from their card and sees it working), else the person's own workspace on that host, as
 * the host answers it once this tab is let in (`askOwnWorkspace`): made then on a first visit,
 * under an id the host chooses, so the ticket names a workspace the host knows.
 */
function agentSetupCodes({origin, identity, runtime, link, workspace = null}) {
  const host = new URL(origin).origin;
  const tickets = ticketCodes({origin: host, identity});
  // What the setup keeps of a ticket names whose it is: a ticket kept for another person or host
  // is not this tab's to follow.
  const ours = saved => saved?.person === identity.userPublicKey && saved?.host === host;
  const make = () => makeCode().catch(error => { throw hostNamed(error, host); });
  const makeCode = async () => {
    const own = workspace ? {id: workspace, name: null} : await setupPersonWorkspace();
    const made = await tickets.create({workspace: own.id, name: own.name});
    return {...made, saved: {...made.saved, person: identity.userPublicKey, host}};
  };
  const resume = saved => (ours(saved) ? tickets.resume(saved) : null);
  return {tickets, ours, make, resume, warm: () => tickets.warm()};
}

/**
 * The first screen's tickets, from the host at `origin` (for an agent to set Tilo up through this
 * tab's root, or to sign this tab in as a person who has been here), offered once: the tab needs
 * only its person (`identity`) and that host for them, not a session in any workspace. The ticket
 * itself is [`agentSetupCodes`]'s. `pair` makes this tab a device of a person through that host
 * (the one whose device approved the ticket), and `home` is the host's shared home workspace,
 * which a sign-in does not open. The setup ticket names the person's own workspace, which the
 * host makes when it is asked for it, so it is made only once the person asks to set up
 * (`ahead: false`).
 */
function offerFirstScreenCodes({origin, identity, runtime, link, home = null, pair = null}) {
  if (firstScreenCodesOffered || !firstScreen || !origin || !runtime) return;
  firstScreenCodesOffered = true;
  const host = new URL(origin).origin;
  const setup = agentSetupCodes({origin: host, identity, runtime, link});
  firstScreen.offerSetup(setup.make, setup.resume, {ahead: false});
  // The press that makes a code asks the host over one link: dial it once the
  // screen is idle, so that press waits only for the answer.
  setup.warm();
  // Signing this tab in is the person's computer's to approve, and this tab's to pair once it
  // learns whom: the ticket names no workspace, so the host makes none for it.
  if (!pair) return;
  const {tickets, ours} = setup;
  const signIn = claimed => signInAs(claimed, {origin: host, home}, link, pair);
  firstScreen.offerAgentSignIn(async () => {
    const made = await tickets.createSignIn();
    return {...made, saved: {...made.saved, person: identity.userPublicKey, host}, signIn};
  }, saved => (ours(saved) ? {...tickets.resume(saved), signIn} : null));
}

/**
 * Sign this tab in as the person whose device signed in its sign-in ticket
 * (`{user, signedIn}`): the tab enters here with the certificate it was
 * signed in with (`pair`), as a new device of theirs, and then loads, once, the workspace it
 * opens: the one the person last used on the host at `origin`, else their own
 * (`signInWorkspace`). `home` is that host's shared home workspace, which is
 * where people meet, not where someone starts. With `stay` (signed in from a
 * link to a workspace) the tab loads that link again instead.
 */
export async function signInAs({user, signedIn}, {origin, home = null, stay = false}, link, pair) {
  setupDone();
  const peer = await pair(user, signedIn);
  // Signed in from a link: the link opens again, now as them.
  if (stay) {
    await peer.dispose?.();
    globalThis.location.reload();
    return;
  }
  let destination;
  try {
    destination = await signInWorkspace(peer, {origin, home});
  } finally {
    await peer.dispose?.();
  }
  const url = (await link).format({mode: 'desktop', target: {scope: {kind: 'workspace', host: origin, workspace: destination}}});
  // The workspace the tab opens, then one load of it: the page is admitted
  // as this device into the workspace it shows, so it names that one before
  // it loads, rather than drawing the workspace it paired through first.
  globalThis.history.replaceState(globalThis.history.state, '', url);
  globalThis.location.reload();
}

/**
 * The workspace a tab an agent signed in loads, once it is a device of the
 * person: the one they last used on the host at `origin` (the most recent
 * this browser entered there, never the host's shared home workspace `home`
 * nor one that refused it last time), else the person's own there, which the
 * host makes or finds. Resolves to the workspace's id.
 */
async function signInWorkspace(peer, {origin, home = null}) {
  const at = new URL(origin).origin;
  const last = remembering(() => enteredWorkspaces()
    .find(kept => kept.origin === at && !kept.unavailable && kept.workspace !== home)) ?? null;
  if (last) return last.workspace;
  const own = await ensureOwnWorkspace(peer);
  return own.id;
}

/**
 * The workspace an agent set up from this tab goes into: the person's own on
 * the host this tab is let into, as the host answers it, or the one this tab
 * is in where the host keeps none. Waits for the tab to be let in.
 */
async function setupWorkspace() {
  return (await setupPersonWorkspace()).id;
}

/**
 * The person's own workspace on the host this tab is let into, as the host answers it:
 * `{id, name}`. Waits for the tab to be let in.
 */
async function setupPersonWorkspace() {
  const peer = await setupPeer();
  if (!peer?.workspace) throw Object.assign(new Error('this tab is not in its workspace yet'), {code: 'not-let-in'});
  const own = await askOwnWorkspace(peer);
  if (own) return own;
  if (setUpIn) return {id: setUpIn, name: null};
  throw Object.assign(new Error('your workspace could not be made; try again'), {code: 'no-workspace'});
}

/** This tab's peer, once it is in its workspace, or `null` after the wait. */
async function setupPeer() {
  return Promise.race([pageLetIn, new Promise(resolve => setTimeout(resolve, LET_IN_MS, null))]);
}

/** The person's own workspace on the home host, or null, once asked for. */
let ownWorkspaceAsked = null;
/** The workspace switcher's choice of the person's own hosted workspace. */
/**
 * The person's own workspace on the host of `peer`, with this page's device
 * in it (the shared device entry makes or finds it and redeems its
 * invitation), remembered among the workspaces this browser entered so the
 * switcher offers it and opening it reconnects. The ask names no workspace:
 * the host answers the person's own, made under an id of its choosing the
 * first time. Resolves to `{id, name, origin}`.
 */
async function ensureOwnWorkspace(peer, store = globalThis.localStorage) {
  const origin = new URL(peer.origin).origin;
  const own = await peer.enterOwnWorkspace({
    entered: id => enteredWorkspaces(store).some(kept => kept.origin === origin && kept.workspace === id),
  });
  const name = own.name || null;
  const hostDevice = hostDeviceOf(peer.host?.hostCert);
  if (hostDevice) rememberEntered(await loadRuntime(), {origin, workspace: own.workspace, hostDevice, name}, store);
  return {id: own.workspace, name, origin};
}

/** The ticket actions `tilo.device` answers a page with on its host (`ticketAction`). */
const TICKET_ISSUE = 'issue';
const TICKET_FOLLOW = 'follow';
const TICKET_ANSWER = 'answer';
/** How long a ticket works, as the host serves it (`TICKET_LIFETIME_NS`). */
const TICKET_LIFE_MS = 10 * 60_000;

/**
 * The tickets this tab's host serves it for its agent: `create({workspace, name})` asks for one
 * the agent's computer logs in with, `createSignIn()` one that signs this tab in as the person
 * whose computer approves it. Each answers the way the first screen follows a code
 * (`{code, expiresAt, follow(), saved}`), and `resume(saved)` follows one kept before a reload
 * again. A ticket is the longer text the prompt carries; the [id] under it is what this page asks
 * the host by.
 *
 * The page asks for its tickets and follows them with the ticket actions its host answers
 * (`ticketAction`: `issue`, `follow`, `answer`), the same `tilo.device` actions the computer
 * calls there. The host serves a setup's ticket on its iroh endpoint, and the tab approves the
 * computer that shows up with it by signing, with the root it holds, the certificate the host
 * built and the question with its own device key (`peer.approveAsked`). A sign-in's ticket names
 * the key this tab asks with, and is the person's computer's to approve: the tab reads whose
 * device it became (`claimed`) and the certificate their root signed for that key when it follows
 * the ticket.
 */
function ticketCodes({origin, identity}) {
  const post = httpsPost(origin);
  // The key a sign-in asks with, before the tab knows whose device it becomes: kept under a
  // profile of its own until the person's device signed it in, then moved to theirs
  // (`adoptDeviceKey`).
  const signInProfile = `tilo-sign-in@${new URL(origin).host}`;
  let signInKey = null;
  const signingIn = async () => {
    signInKey ??= openDeviceKey({profile: signInProfile, storage: idbStorage()})
      .then(async key => ({key, wasm: key.kind === 'sealed-seed' ? await key.unseal() : key}));
    return signInKey;
  };
  // The answer as the host wrote it: a certificate in it has times more than a number holds.
  const callText = async (action, body, deviceKey = undefined) => {
    // A ticket is issued and followed over the host's action link, whose code
    // is the tickets module's (`../src/split.rs`).
    const runtime = await loadRuntime('tickets');
    const text = typeof body === 'string' ? body : JSON.stringify(body);
    const reply = await runtime.ticketAction(post, action, text, deviceKey).catch(error => {
      throw hostNamed(Object.assign(new Error(String(error?.message ?? error)), {code: 'unreachable'}), origin);
    });
    const answer = JSON.parse(reply);
    if (typeof answer?.refused === 'string') {
      throw hostNamed(Object.assign(new Error(answer.refused || 'the host refused this ticket'),
        {name: 'TicketError', code: 'refused'}), origin);
    }
    return memberText(reply, 'value') ?? 'null';
  };
  const call = async (action, body, deviceKey = undefined) => JSON.parse(await callText(action, body, deviceKey));
  const kept = (ticket, id, direction) => ({code: ticket, expiresAt: Date.now() + TICKET_LIFE_MS, follow: id, id, direction});
  const answerer = saved => {
    let answering = false;
    return {code: saved.code, expiresAt: saved.expiresAt, saved: {...saved},
      follow: async () => {
        // A sign-in is followed by the key that asked, which alone reads its certificate.
        const asking = saved.direction === 'sign-in' ? await signingIn() : null;
        const statusText = await callText(TICKET_FOLLOW, {id: saved.follow}, asking?.wasm);
        const status = JSON.parse(statusText) ?? {};
        const agent = status.agent ?? null;
        let claimed = status.claimed ?? null;
        if (saved.direction === 'sign-in') {
          // The person's computer signed this tab in: the screen hands whom it became a device
          // of, with the certificate and the key it was signed in with.
          if (status.certified) {
            const signedIn = {certified: `{"cert":${status.certified},"hostCert":${status.hostCert}}`,
              key: {profile: signInProfile, slot: asking.key.slot}};
            await asking.key.close();
            signInKey = null;
            claimed = claimed ? {...claimed, signedIn} : null;
            return {resolved: true, expired: false, steps: ['approved'], agent, claimed};
          }
          if (status.declined) {
            return {resolved: true, expired: false, steps: ['failed'], agent, detail: status.declined, claimed: null};
          }
          // Whom it names is said once the tab is signed in: then it enters as them.
          return {resolved: Boolean(claimed), expired: Date.now() > saved.expiresAt, steps: [], agent, claimed: null};
        }
        if (status.declined) {
          return {resolved: true, expired: false, steps: ['failed'], agent, detail: status.declined};
        }
        if (status.asked && !answering) {
          answering = true;
          try {
            const peer = await setupPeer();
            const person = currentIdentity();
            if (!peer?.approveAsked || !person) throw new Error('this tab cannot approve your agent');
            // Signed over the question as the host asked it, and handed back as signed.
            const approved = await peer.approveAsked(person, memberText(statusText, 'asked'));
            await call(TICKET_ANSWER, `{"id":${JSON.stringify(saved.follow)},"approved":${approved}}`);
          } catch (error) {
            console.warn('first screen: approving your agent', error);
            await call(TICKET_ANSWER, {id: saved.follow, declined: String(error?.message ?? error)})
              .catch(() => {});
          } finally {
            answering = false;
          }
        }
        // The steps are the ones the computer reported: it logged in, added Tilo's tools, and
        // entered. Approval is not a step the screen claims on its own.
        const steps = status.steps ?? [];
        return {resolved: steps.length > 0 || Boolean(status.asked), expired: Date.now() > saved.expiresAt,
          steps, agent, detail: status.detail ?? status.declined ?? null};
      }};
  };
  return {
    async create(workspace, name) {
      const minted = await call(TICKET_ISSUE, {user: identity.userPublicKey, workspace, name: name ?? null,
        site: globalThis.location?.origin ?? null});
      return answerer(kept(minted.ticket, minted.id, 'login'));
    },
    async createSignIn() {
      const {wasm} = await signingIn();
      const minted = await call(TICKET_ISSUE, {direction: 'sign-in', label: deviceLabel(),
        site: globalThis.location?.origin ?? null}, wasm);
      return answerer(kept(minted.ticket, minted.id, 'sign-in'));
    },
    resume(saved) {
      return answerer(saved);
    },
    /** Fetch and compile the tickets module and dial the link a press will ask
     * over, so the press waits only for the answer: once the runtime is in and
     * the page is idle, never beside the start module's own download. Quiet
     * when the host is not reached; the press dials again (`../src/peer.rs`). */
    warm() {
      const idle = () => {
        void loadRuntime('tickets').then(runtime => runtime.warmTicketAction?.(post)).catch(() => {});
      };
      if (typeof requestIdleCallback === 'function') requestIdleCallback(idle, {timeout: 5000});
      else setTimeout(idle, 5000);
    },
  };
}

/** `error`, naming the host at `origin` it came from (`origin`, as
 * `first-screen.mjs` `setupFailure` reads it) unless it names one already: the
 * codes' host is not the page's own (tilo.sh's come from eu1.tilo.sh), and
 * the screen says which one did not answer. */
function hostNamed(error, origin) {
  if (error && typeof error === 'object' && !error.origin) {
    try { error.origin = new URL(origin).origin; } catch { /* Then the screen names the site. */ }
  }
  return error;
}

