/**
 * The entry pages' wiring: the real page host and admission doors behind
 * `entry.mjs`, and the few controls a link needs (an invitation to accept,
 * an account to choose, the other mode). Diagnostic query switches stay
 * readable (`?workspace=`, `?workspace=`, `?invite`), since they say nothing
 * about what the link opens.
 *
 * `?embed=1` is the page inside another site (the homepage's live
 * preview): it shows nothing of the workspace until the visitor's first
 * click, tap or key press, then reloads and joins as this browser's own
 * person, as the site itself would. A person who already joined that host
 * comes back at once. The embedding page, if it is the web host's embedder
 * (`entry.json` `embedder`), hears `{type: 'tilo-embed', state}`: `idle`
 * until the page joined (`joined`), and again after a failure, when the site
 * shows its static fallback. A full workspace answers the door with
 * `503 Retry-After`, and the page waits on the reconnect screen for a seat
 * like anyone else.
 */
import {AdmissionError} from './admission.mjs';
import {doorCopy, failureMessage, inviteLinkTarget, localeMessages, localeMessagesSoon, localized, refusedByHost, startArrival, STOPPED_DRAWING, stoppedInside} from './arrival.mjs';
import {servedVersion, watchForUpdate} from './bundle-update.mjs';
import {mountWaitList} from './door-wait-list.mjs';
import {boot, grammar} from './entry.mjs';
import {applicationName, loadRuntime, mountDesktop, prefetchApplication, prefetchOfferedApplications, prepareGpu, reportPageErrors} from './host.mjs';
import {connectPeer, httpsPost, identityProfile, inspectInvitation, joinWorkspace, openInvitation, profileDevice, withCustody} from './peer.mjs';
import {readBrowserAccounts} from './browser-accounts.mjs';
import {CustodyError, idbStorage, openDeviceCustody} from './device-custody.mjs';
import {createIdentityActions, SIGN_IN, syncProfile} from './identity-actions.mjs';
import {firstRun, MCP_URL, SEEN} from './first-run.mjs';
import {pickImageFile, squarePhoto} from './profile-photos.mjs';
import {deviceLabel, pairingFailure, pairSecret, whenRequested} from './pairing.mjs';
import {readInstallSecret, SET_UP, SETUP_PHASES, setUpFailure, setUpMachine} from './machine-setup.mjs';
import {browserIdentity, browserPerson, currentIdentity, fetchEnvelope, IdentityError, MAX_DISPLAY_NAME_BYTES, signInWithPasskey} from './identity.mjs';
import {createReconnect, domReconnectView} from './reconnect.mjs';
import {followConnection} from './connection-status.mjs';
import {followRefusal, hostUnreachable, reachedHost, refusalText, rejoinedName, retryUnreachable} from './session-refusal.mjs';
import {rendererUnavailable} from './webgpu-safety.mjs';
import {createWorkspaceActions, invitedLine} from './workspace-actions.mjs';
import {sameEntry} from './entered-workspaces.mjs';
import {
  chooseDisplayName, chooseWorkspace, chosenDisplayName, chosenWorkspace, enteredWorkspaces, forgetGoneWorkspaces,
  heldWorkspaces, hostDeviceOf, hostToReconnect, joinGranted, joinViaHost, keepJoinedHost, keepOwnedHost,
  keptMembership, listedWorkspaces,
  membershipOrigins, nameEntered, OPEN_WORKSPACE, ownDesktopOpen, ownedHosts, pairViaHost, defaultDisplayName, redrawnDisplayName, rememberEntered,
  settleFailedWorkspace, workspaceActions as workspaceSwitchActions, workspaceChoices,
} from './workspaces.mjs';

/** Set for the one reload that joins an embed's visiting page as the person. */
const JOIN_NOW = 'tilo-embed-join';

const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');

/** The host's certificate; a workspace that just started refuses briefly. */
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

/** How long the host at a remembered workspace's origin gets to say who answers there. */
const HOST_CHECK_MS = 5_000;

/** Whether the remembered workspace `known` is gone, asked of its origin after
 * opening it failed (`settleFailedWorkspace`): `gone`, `unavailable` or `null`. */
async function settleWorkspace(known, runtime) {
  const hostInfo = async origin => {
    if (!runtime) throw new Error('no runtime to ask the host with');
    const fetcher = (url, init) => globalThis.fetch(url, {...init, signal: AbortSignal.timeout(HOST_CHECK_MS)});
    return JSON.parse(await runtime.hostInfo(httpsPost(origin, fetcher))).hostCert;
  };
  try {
    return await settleFailedWorkspace(runtime, known, {hostInfo});
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

/** What each role lets someone do, in the words the desktop's role chooser
 * uses (`workspace-settings-role-<role>-preview`), so an invitation says what
 * it gives, not only its name. */
const ROLE_WORDS = {
  owner: 'you can manage people, permissions and settings',
  moderator: 'you can edit shared work and remove other people’s messages',
  editor: 'you can view, comment on and edit shared work',
  commenter: 'you can add comments, but not edit shared work',
  viewer: 'you can view shared work, but not comment or edit it',
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

/** What an invitation offers, before the person joins: who it is from, the
 * workspace it is for and how many people are in it, and what the role lets
 * them do. `inviterColour` is the presence colour of who made it, where the
 * page has one for them. Resolves to whether they join. */
function invitationCard(offer, inviterColour) {
  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card tilo-invite';
    form.dataset.card = 'invite';
    const inviter = offer.inviter?.name || 'Someone';
    const who = document.createElement('div');
    who.className = 'tilo-invite-who';
    // The picture a person sets is not the workspace's to hand to someone
    // who is not in it yet; their initial is, as the desktop draws who it
    // has no picture of — in the colour presence gives them everywhere else
    // the desktop shows them.
    const face = document.createElement('span');
    face.className = 'tilo-invite-face';
    face.setAttribute('aria-hidden', 'true');
    face.textContent = [...inviter][0]?.toUpperCase() ?? '?';
    if (typeof inviterColour === 'string' && /^#[0-9a-f]{6}$/i.test(inviterColour)) {
      face.style.background = inviterColour;
      face.style.color = readableOn(inviterColour);
    }
    const name = document.createElement('span');
    name.className = 'tilo-invite-name';
    name.textContent = inviter;
    who.append(face, name);
    const lead = document.createElement('p');
    lead.className = 'tilo-invite-lead';
    lead.textContent = 'invites you to';
    const workspace = document.createElement('p');
    workspace.className = 'tilo-invite-workspace';
    workspace.textContent = offer.workspaceName;
    const place = document.createElement('div');
    place.className = 'tilo-invite-place';
    place.append(workspace);
    if (Number.isInteger(offer.members) && offer.members > 0) {
      const people = document.createElement('p');
      people.className = 'tilo-invite-people';
      people.textContent = offer.members === 1 ? '1 person' : `${offer.members} people`;
      place.append(people);
    }
    const role = typeof offer.role === 'string' && offer.role ? offer.role : 'member';
    const roleLine = document.createElement('p');
    roleLine.className = 'tilo-invite-role';
    const roleName = document.createElement('strong');
    roleName.textContent = role[0].toUpperCase() + role.slice(1);
    roleLine.append(roleName, ` — ${ROLE_WORDS[role] ?? 'you join the workspace'}.`);
    const actions = document.createElement('div');
    actions.className = 'tilo-entry-actions';
    for (const [label, join] of [['Join', true], ['Not now', false]]) {
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = label;
      if (join) button.className = 'tilo-entry-primary';
      button.addEventListener('click', () => { form.remove(); resolve(join); });
      actions.append(button);
    }
    form.append(who, lead, place, roleLine, actions);
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
 * The QR code of `link`, as the image the sheet shows: drawn by the runtime
 * (`qrSvg`), which the sheet's own page has in by the time a person asks to
 * pair a device.
 */
function pairCode(link) {
  const code = document.createElement('img');
  Object.assign(code, {className: 'tilo-pair-qr', alt: 'QR code of the pairing link', width: 240, height: 240});
  void (async () => {
    try {
      const runtime = await loadRuntime();
      code.src = `data:image/svg+xml,${encodeURIComponent(runtime.qrSvg(link))}`;
    } catch (error) {
      console.warn('pairing: this link cannot be drawn as a code', error);
    }
  })();
  return code;
}

/**
 * The sheet that pairs a new device: the link as a QR code for a phone's
 * camera, as text to open or paste, and a Copy button (a click, which the
 * clipboard honours). `copy()` resolves whether the link was copied; where
 * it was not, the link is selected for the person to copy. Closing the
 * sheet leaves the link working: its device is still asked about here.
 */
function pairLinkSheet({link, copy}) {
  const card = pairCard('pair-link', 'Pair a new device');
  const code = pairCode(link);
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

/** The addresses on the workspace's wait list, each to invite (`invite(entry)`
 * makes and shares an invitation for it) or take off the list (`unlist`), as
 * `load()` resolves them; read again after every decision. Resolves when
 * closed. */
function waitingCard({load, invite, unlist}) {
  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card tilo-waiting';
    form.dataset.card = 'waiting';
    const heading = Object.assign(document.createElement('h2'), {textContent: 'Waiting to join'});
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
      let contacts = [];
      try { contacts = await load(); } catch (refusal) { said(refusal); }
      // The wait list: addresses people left at the door, oldest first.
      listed.replaceChildren();
      if (!contacts.length) {
        listed.append(Object.assign(document.createElement('p'), {className: 'tilo-waiting-none',
          textContent: 'Nobody is waiting to join.'}));
        return;
      }
      const title = Object.assign(document.createElement('h3'), {textContent: `Wait list · ${contacts.length}`});
      const about = Object.assign(document.createElement('p'), {className: 'tilo-waiting-note',
        textContent: 'They asked to join without an invitation. Invite sends them a link to join; they leave the list once they join.'});
      const rows = document.createElement('ul');
      rows.append(...contacts.map(entry => {
        const item = document.createElement('li');
        const who = document.createElement('span');
        who.className = 'tilo-waiting-who';
        who.append(Object.assign(document.createElement('span'), {className: 'tilo-waiting-contact', textContent: entry.contact, title: entry.contact}),
          Object.assign(document.createElement('time'), {dateTime: new Date(entry.addedAtMs).toISOString(), textContent: addedAt(entry.addedAtMs)}));
        // Invited already: said, so nobody invites them twice.
        if (entry.invitedAtMs != null) {
          who.append(Object.assign(document.createElement('time'), {className: 'tilo-waiting-invited',
            dateTime: new Date(entry.invitedAtMs).toISOString(), textContent: invitedLine(entry.invitedAtMs)}));
        }
        item.append(who, button(entry.invitedAtMs == null ? 'Invite' : 'Invite again', () => decide(async () => {
          const sent = await invite(entry);
          if (sent?.mailed) mailed(entry.contact, sent.link);
        }, entry.requestId)), button('Remove', () => decide(unlist, entry.requestId)));
        return item;
      }));
      listed.append(title, about, rows);
    };
    // A mailed invitation, said, with its link to copy should the mail not
    // arrive.
    const sent = document.createElement('p');
    sent.className = 'tilo-waiting-sent';
    sent.setAttribute('role', 'status');
    sent.hidden = true;
    const mailed = (contact, link) => {
      const copy = button('Copy link', async () => {
        copy.textContent = await navigator.clipboard.writeText(link).then(() => 'Copied', () => 'Copy failed');
      });
      sent.replaceChildren(`Invitation emailed to ${contact}. `, copy);
      sent.hidden = false;
    };
    const decide = async (choice, requestId) => {
      error.hidden = true;
      sent.hidden = true;
      try { await choice(requestId); } catch (refusal) { said(refusal); }
      await show();
    };
    form.append(heading, listed, sent, error, close);
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

/** The page's uncaught errors, for the door its entry names (`<door>errors`):
 * the site's host logs them, and sizes and rate-limits them there. Five a
 * page load at most, and none of them waits for an answer. `report` is handed
 * to the host as well, which sends a runtime panic this way
 * (`reportPageErrors`). */
function reportErrors() {
  let door = null;
  let sent = 0;
  const report = (kind, message, stack, at = {}) => {
    if (!door || sent++ >= 5) return;
    try {
      navigator.sendBeacon(new URL('errors', door), new Blob([JSON.stringify({
        kind, message: String(message), source: at.source ?? '', line: at.line ?? 0, column: at.column ?? 0,
        stack: stack || '', page: location.pathname, build: PAGE_BUILD,
      })], {type: 'text/plain'}));
    } catch { /* Reporting must never throw. */ }
  };
  addEventListener('error', event => report('error', event.message, event.error?.stack,
    {source: event.filename, line: event.lineno, column: event.colno}));
  addEventListener('unhandledrejection', event => report('unhandledrejection', event.reason?.message ?? event.reason, event.reason?.stack));
  return {at: next => { if (!door && typeof next === 'string' && next) door = next; }, report};
}

export async function startEntryPage({mode}) {
  const surface = document.getElementById('surface');
  const status = document.getElementById('status');
  const query = new URLSearchParams(location.search);
  const embed = query.get('embed') === '1';
  // The bar over one app full tab (app-bar.mjs), fetched beside the runtime
  // and only by that page: the desktop has its own.
  const appBarModule = mode === 'app' && !embed ? import('./app-bar.mjs') : null;
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
    // A page that cannot go on has no workspace to set an agent up in: the
    // first screen says so rather than wait.
    if (step === 'stopped') firstRun?.unavailable({code: 'refused'});
    root.arrival = step;
    if (notes[step] && [loading, opening, drawing].includes(status.textContent)) status.textContent = notes[step];
  };
  // The arrival screen's line and bar follow those steps (arrival.mjs).
  startArrival({mode});
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
  // A panic that named no window is the page's own crash, which the host
  // hears: it reaches the door's log the same way, before the page starts over.
  reportPageErrors((kind, message) => pageErrors.report(kind, message));
  /** Where an embed's state goes: the site the web host names as its embedder, once the entry is read. */
  let embedder = null;
  const tell = state => { if (embed && embedder && parent !== window) parent.postMessage({type: 'tilo-embed', state}, embedder); };
  let joinNow = false;
  try { joinNow = sessionStorage.getItem(JOIN_NOW) === '1'; sessionStorage.removeItem(JOIN_NOW); } catch { /* No storage: no gesture asked yet. */ }
  /** Whether this page got in through a host's door as the browser's own person (rather than by an invitation or an account). */
  let enteredViaDoor = false;
  /** The workspace whose door this page was let in through, or waits at. */
  let visitedAt = null;
  /** Keeping the person this page joined as (`identity-actions.mjs`), once there is one. */
  let identityActions = null;
  /** Setting up a machine (`machine-setup.mjs`), once the page is open. */
  let setUp = null;
  /** Workspaces on the person's machines (`workspace-actions.mjs`), once the page is open. */
  let workspaceActions = null;
  let displayName;
  /** The picture the person chose (a `data:` URL), once their profile is read. */
  let ownPhoto = null;
  /** The workspace this page is getting into, by name, once it knows it. */
  let workspaceName = query.get('workspace') || null;
  /** The workspace the link names and the entry it is served under, once read (`deps.onScope`). */
  let admitting = null;
  /** The name the host signed into an invitation this page redeemed. */
  let invitedTo = null;
  /** Whether this page came in on a guest ticket, which is not remembered as membership. */
  let viaTicket = false;
  /** The workspace in front, remembered as one this browser entered, until the desktop names it. */
  let unnamed = null;
  /** Opening another of the person's workspaces, by host action id, once the page is open. */
  let switchWorkspace = null;
  /** Opening the window in front in a browser tab of its own, once the page is open. */
  let openInTab = null;
  /** The peer the host let in, once it did: it knows the workspace's name as its people last set it. */
  let admitted = null;
  /** Whether the page's store last committed every change: a mount that
   * presented says so with its `persistence`, and a store that stops saving
   * later takes the bar (`connection-status.mjs`). */
  let saved = true;
  /** What the desktop last said of itself (its phase, its error, its
   * store): the bar reads it, so a renderer that stops after the first frame
   * is a state the person sees rather than one that scrolled past. */
  let desktopStatus = null;
  /** Whether this page has no desktop to report on: a browser that cannot
   * draw one says so before it is let in, and its own desktop has no session
   * to be cut off from, so no line about a connection is owed. */
  let noDesktop = false;
  /** The page once it is open: its mount and its peer. The desktop's status
   * arrives while it opens, so the bar reads it from here rather than from
   * the `boot` call's own scope. */
  let opened = null;
  /** The bar's follower of the connection, once it is written below. The
   * desktop reports its first status while the page opens, before then. */
  let bar = null;
  const diagnostics = {
    onHostAction: (id, value) => {
      if (id === SET_UP.id) void setUp?.();
      else if (id === OPEN_IN_TAB) openInTab?.();
      else if (id.startsWith(OPEN_WORKSPACE)) void switchWorkspace?.(id);
      else if (workspaceActions?.handles(id)) void workspaceActions.perform(id);
      else void identityActions?.perform(id, value);
    },
    // Where the person's agents connect over MCP, which their agent door shows.
    mcp: MCP_URL,
    ...(query.get('room') ? {room: query.get('room')} : {}),
    ...(query.get('workspace') ? {workspaceName: query.get('workspace')} : {}),
    // The directory sheet's Join, which its door granted: the page joins with
    // the invitation it answered, then shows the workspace.
    onJoinGranted: joined => { void openGrantedWorkspace(joined); },
    // The render state is for tests and tools (data-phase, data-frames on
    // the root element); the bar only tells the person something they can
    // act on, unless the page was opened with ?debug.
    onStatus: state => {
      if (unnamed && typeof state.workspaceName === 'string' && state.workspaceName) {
        const named = {...unnamed, name: state.workspaceName};
        unnamed = null;
        remembering(() => nameEntered(runtime, named));
      }
      if (!state.phase) return;
      root.phase = state.phase;
      // What graphics start-up waits on while the first frame is late
      // (device, pipelines, surface, first-frame), for tests and tools.
      if (state.gpuStep) root.gpuStep = state.gpuStep;
      // Whether that step stopped moving on: the page offers Try again
      // while it keeps waiting (arrival.mjs).
      if ('gpuStalled' in state || state.phase !== 'gpu-pending') document.documentElement.toggleAttribute('data-gpu-stalled', state.gpuStalled === true);
      if (state.phase === 'presented') {
        arrive('done');
        // The page has drawn: the apps it offers can now fetch their modules
        // in its idle time, a bounded few at a time, so an app the person
        // opens later instantiates from what is already in instead of
        // fetching and compiling it while its window waits to fill in.
        prefetchOfferedApplications();
      }
      else if (state.phase === 'gpu-pending') arrive('draw');
      else if (['failed', 'crashed'].includes(state.phase)) arrive('stopped');
      root.frames = String(state.frames ?? 0);
      // The desktop's own report, for the bar: a renderer or a store that
      // fails after the first frame is a state the person must see, not an
      // event that scrolls past (`connection-status.mjs`).
      desktopStatus = state;
      if (state.persistence) saved = state.persistence !== 'failed';
      // The desktop reports its first status while the page opens, before
      // the follower below is written; it looks once it is.
      bar?.look();
      if (debug) {
        const said = bar.last;
        status.textContent = `${state.phase}${state.error ? `: ${state.error}` : ''} · frames ${state.frames ?? 0}${state.persistence ? ` · ${state.persistence}` : ''}`
          + (said.state === 'online' ? '' : ` · ${said.state}${said.detail ? `: ${said.detail}` : ''}`);
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
  /**
   * The bar's line about this desktop's connection, followed from the
   * session's health and the desktop's own reports rather than from the
   * mount that once presented: a transport that drops, an admission that
   * expires, a store that stops saving and a renderer that stops drawing
   * all keep the desktop mounted, and none of them may read as connected.
   * A page opened with ?debug says the line at once; the person reads it in
   * the bar, which clears once it is well again.
   */
  bar = followConnection({
    // The session's own snapshot, re-read each time: `opened.peer` is the
    // constant peer object, and its health changes under it.
    read: () => ({peer: opened?.peer?.getSnapshot?.() ?? null, desktop: desktopStatus, messages,
      mounted: Boolean(opened?.mount) && !noDesktop, saved}),
    bar: {
      show: reading => {
        document.documentElement.dataset.connection = reading.state;
        // The cause is for the console, once: the bar says what to do.
        if (!reading.cleared) console.warn(`desktop: ${reading.state}`, reading.detail ?? '');
        // The bar is the page's own note while it arrives, so only a line
        // still of its own is replaced: a visit message stays, and so does
        // the note about a desktop that stopped while the session was fine.
        const own = [loading, opening, drawing, STOPPED_DRAWING].includes(status.textContent);
        if (!own && !reading.cleared) return;
        status.textContent = reading.cleared ? '' : reading.sentence;
      },
    },
  });
  let runtime;
  // Whether this browser can draw at all, asked as the page starts: the
  // browser's GPU process takes hundreds of milliseconds to answer on a
  // first visit, and nothing the door needs waits on it. `entry.mjs` asks
  // for the answer before it mounts anything, so a browser that cannot draw
  // still opens no window.
  const renderer = rendererUnavailable();
  /** Said while one of the person's devices decides on this one. */
  const showPairingCode = code => {
    document.documentElement.dataset.pairingCode = code;
    status.textContent = `Pairing code ${code}. On a device where you are already signed in, choose “Pair a new device” and approve ${code}.`;
  };
  const deps = {
    loadRuntime: async () => (runtime = await loadRuntime()),
    rendererUnavailable: () => renderer,
    onAdmitted: peer => { admitted = peer; },
    prefetchApplication,
    // An embed always shows the workspace the embedding site names.
    chosenWorkspace: () => (embed ? null : remembering(() => chosenWorkspace())),
    onScope: next => {
      admitting = next;
      embedder = next.entry?.embedder ?? null;
      // This site's door: where its pages report what they did not catch.
      pageErrors.at(next.entry?.home?.admission);
      const known = remembering(() => rememberedWorkspace(next.scope));
      if (known?.name && !workspaceName) workspaceName = known.name;
    },
    // The person's own desktop has no workspace's peer to certify who writes:
    // the browser's root identity is the person, as everywhere else.
    mountDesktop: async (surface, options) => {
      const identity = options.peer ? null : currentIdentity() ?? await browserIdentity({create: false}).catch(() => null);
      return mountDesktop(surface, identity ? {author: identity.userPublicKey, ...options} : options);
    },
    // The workspace directory its desktop browses (`mountDesktop({directory})`):
    // this site's listings, the person a join is made as, and the workspaces
    // this page holds.
    directory: () => {
      if (!admitting?.entry?.base) return null;
      const scope = admitting.scope ?? null;
      const current = scope?.kind === 'room'
        ? {kind: 'room', host: scope.host, room: scope.room ?? admitted?.workspace ?? null, name: workspaceName} : null;
      const user = currentIdentity()?.userPublicKey;
      return {
        site: admitting.entry.base,
        ...(user ? {person: {userPublicKey: user,
          name: displayName ?? chosenDisplayName(user) ?? defaultDisplayName(user)}} : {}),
        held: heldWorkspaces({entered: enteredWorkspaces(), current}),
      };
    },
    ownDesktopOpen: home => ownDesktopOpen(home),
    // A workspace of this host the browser entered before, such as the
    // person's own: reconnected to, not asked for at the host's door.
    entered: ({origin, room}) => enteredWorkspaces().some(kept => sameEntry(kept, {origin, room})),
    enterViaHost: async ({workspace, room}) => {
      workspaceName = workspace.name || workspaceName;
      visitedAt = workspace;
      // An embed joins nothing before the visitor does something: it takes no
      // seat and makes no person, and its site shows its static fallback
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
      // A page that came back here by itself because the host turned it
      // away (session-refusal.mjs) joins under the name it went by.
      displayName = chosenDisplayName(identity?.userPublicKey ?? kept?.userPublicKey) ?? rejoinedName() ?? defaultDisplayName(identity?.userPublicKey ?? kept?.userPublicKey);
      // A workspace with no seat right now (full, or this address asking too
      // often) is waited out on the reconnect screen, as long as its door
      // asked: the page joins as the same person once there is room.
      const peer = await joinViaHost({workspace, identity, displayName, runtime, onPairingCode: showPairingCode,
        wait: next => reconnect.wait({...next, origin: workspace.host.origin, build: PAGE_BUILD,
          retryAfterMs: Number.isFinite(next.error?.retryAfter) ? next.error.retryAfter * 1000 : 0})});
      reconnect.done();
      enteredViaDoor = true;
      return peer;
    },
    // A peer in the home workspace while the page is in another workspace, which
    // carries the desktop's problem reports to the home's channel. Only for
    // a person the home already let in, as themselves (a paired device's
    // person approves new devices elsewhere): nobody joins the home just to
    // report. It connects with this tab's device key for the home's host (a
    // second one where the workspace's peer holds the first), without calls
    // or a live plane, gives up rather than waiting at the door, and is kept
    // while the page is.
    homePeer: async home => {
      const kept = remembering(() => keptMembership(home));
      if (!kept || kept.paired) return null;
      const identity = currentIdentity() ?? await browserIdentity({create: false}).catch(() => null);
      if (!identity || identity.userPublicKey !== kept.userPublicKey) return null;
      runtime ??= await loadRuntime();
      const name = chosenDisplayName(identity.userPublicKey) ?? defaultDisplayName(identity.userPublicKey);
      return joinViaHost({workspace: home, identity, displayName: name, runtime, live: false, voice: false,
        wait: () => Promise.reject(new Error('the home workspace did not let this device in'))});
    },
    inspectInvitation: (origin, invitation) => inspectInvitation(origin, invitation, {runtime}),
    // A link's `invite` names the invitation its host keeps: the page opens
    // it before anything reads it.
    openInvitation: (origin, secret) => openInvitation(origin, secret, {runtime}),
    // The invite sheet this page's desktop shows, when the page is in a
    // workspace: made and listed through that workspace's host, in links
    // that open this page.
    invite: ({peer, entry}) => peer?.workspace && !embed ? {base: entry.base, origin: peer.origin,
      workspace: peer.workspace, name: displayName ?? '', user: peer.userPublicKey ?? ''} : null,
    confirmInvitation: offer => {
      workspaceName = offer.workspaceName || workspaceName;
      invitedTo = offer.workspaceName || null;
      const inviter = offer.inviter?.userPublicKey;
      return invitationCard(offer,
        typeof inviter === 'string' ? runtime?.presenceColor?.(inviter) : undefined);
    },
    joinWorkspace: async options => {
      const identity = await browserPerson();
      const peer = await joinWorkspace({...options, identity, runtime});
      // Kept, so the next visit reconnects as the person let in.
      keepJoinedHost({origin: options.origin, room: peer.workspace, userPublicKey: identity.userPublicKey});
      return peer;
    },
    reconnect: async ({origin, room}) => {
      // A device paired to a person kept elsewhere goes on as that device,
      // under the certificate it holds: into a workspace its key was
      // admitted to (the person's own, say), never as a new device.
      const paired = remembering(() => keptMembership({host: {origin}}));
      if (paired?.paired) {
        const profile = identityProfile(paired, paired.origin);
        const custody = await openDeviceCustody({profile, storage: idbStorage(), requireExisting: true});
        displayName = chosenDisplayName(paired.userPublicKey) ?? defaultDisplayName(paired.userPublicKey);
        return withCustody(custody, () => connectPeer({origin, workspace: room, userPublicKey: paired.userPublicKey,
          profile, custody, displayName, runtime, requireExistingCertificate: true}));
      }
      const identity = currentIdentity() ?? await browserIdentity({create: false});
      const kept = identity && hostToReconnect(origin, identity.userPublicKey);
      if (!kept) {
        console.warn('reconnect: this browser kept nothing to reconnect to this host with',
          identity ? 'as this person' : 'and holds no person');
        return null;
      }
      displayName = chosenDisplayName(identity.userPublicKey) ?? defaultDisplayName(identity.userPublicKey);
      if (kept.owner) {
        // On a machine of their own the owner enters any of its workspaces
        // (the host admits this device as its owner); the host refuses
        // one that does not exist.
        const lobby = await connectPeer({origin, identity, runtime, displayName, live: false});
        try { await lobby.workspaces.enter(room); } finally { await lobby.dispose?.(); }
      }
      // This tab's own device key: a new one where other tabs hold the
      // others. A workspace admits each device once, through an invitation (or
      // as its host's owner, above), so in a workspace joined by invitation a new
      // device of the person cannot get in by itself: said plainly, and the
      // key, which no host has seen, is not kept.
      const custody = await openDeviceCustody({profile: identityProfile(identity, origin), storage: idbStorage()});
      if (custody.created && !kept.owner) {
        await custody.destroy();
        throw new CustodyError('custody-busy', 'A new device of this person needs an invitation to this workspace');
      }
      const peer = await withCustody(custody, () => connectPeer({origin, workspace: room, identity, runtime, displayName, custody}));
      // What is kept names the workspace the host's door let this browser
      // into (`listedAt`): reconnecting to another of the host's workspaces,
      // such as the person's own, leaves it as it is.
      if (!kept.room) keepJoinedHost({origin, room, userPublicKey: identity.userPublicKey});
      return peer;
    },
    // An embed lists no host actions, so it has nothing to invite with:
    // the desktop offers no Invite there rather than one that does nothing.
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
      accounts.map(account => [account.displayName || account.userPublicKey.slice(0, 12), account]), 'accounts'),
    // A guest ticket: a fresh device of the workspace's root, certified once the
    // web host spent the ticket on this page's pairing code. The root key is
    // read from the host's own certificate, so the link need not carry it.
    redeemTicket: async ({origin, room, ticket, tickets}) => {
      viaTicket = true;
      const {connectRelayPeer} = await import('./relay.mjs');
      const hostCert = await hostCertificate(await loadRuntime(), origin);
      const admit = new URL(`${encodeURIComponent(room)}/admit`, tickets).href;
      return connectRelayPeer({origin, workspace: room, userPublicKey: hex(hostCert.user_public_key), label: 'Guest',
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
      return withCustody(custody, () => connectPeer({origin, workspace: room, userPublicKey, profile, custody,
        device: custody.slot ? profileDevice(profile, globalThis.localStorage, custody.slot) : device,
        runtime, requireExistingCertificate: true}));
    },
  };

  /** The directory sheet's Join: its door granted this page an invitation
   * into a listed workspace, so the page joins with it as it joins one from a
   * link, then shows it. */
  const openGrantedWorkspace = async joined => {
    if (!opened) return;
    try {
      // The same join an invitation link takes, which keeps the membership so
      // the opened page reconnects as this person.
      const {peer, origin, room, name} = await joinGranted(joined, {
        join: options => deps.joinWorkspace(options), displayName: displayName ?? null,
      });
      await peer?.dispose?.();
      await openWorkspace({choice: {origin, room, name}, page: opened, status});
    } catch (error) {
      console.warn('joining a listed workspace:', error);
      const messages = await localeMessagesSoon();
      const said = failureMessage(error, {workspace: joined?.name ?? null, loaded: Boolean(runtime), admitted: true, messages});
      await card(said ?? 'That workspace did not let this page in.', [['Close', null]]);
    }
  };
  try {
    const page = await boot({mode, surface, status, deps, diagnostics, onStep: arrive});
    if (!page) return;
    if (page.rendererUnavailable) {
      // A browser that cannot draw: it got in beside the answer, and stops
      // here with what to change, having opened no window and shown nothing
      // of the workspace. Its seat goes with the page.
      tell('unsupported');
      arrive('stopped');
      document.documentElement.dataset.entry = 'unsupported';
      status.textContent = '';
      rendererCard(page.rendererUnavailable);
      void page.peer?.dispose?.();
      return;
    }
    opened = page;
    const appBar = await appBarModule;
    if (appBar) {
      // One app full tab: the bar says where it is, who is here, and
      // offers Share and the whole desktop.
      const names = new Map();
      appBar.startAppBar({page, workspaceName: () => workspaceName ?? '', appName: app => {
        if (!names.has(app)) {
          names.set(app, app);
          void applicationName(app).then(name => { if (name) names.set(app, name); });
        }
        return names.get(app);
      }});
    }
    // An embed that joined says so: its site takes its fallback away.
    if (enteredViaDoor) tell('joined');
    // Only the desktop lists host actions: a single app's mount refuses them.
    const hostActions = mode === 'desktop' && page.mount?.setHostActions && !embed
      ? hostActionParts(page.mount, ['identity', 'machine', 'workspaces', 'workspace', 'tab']) : null;
    const here = page.router.route.target.scope;
    // The host that answers at the peer's origin now: workspaces remembered there
    // under another host went with that host's state.
    const hostDevice = page.peer ? hostDeviceOf(page.peer.host?.hostCert) : null;
    if (hostDevice) remembering(() => forgetGoneWorkspaces(runtime, {origin: page.peer.origin, hostDevice}));
    // A session's own status (transport, admission, persistence) changes
    // without the desktop unmounting: the page follows it, and clears the
    // line it took once every one of them is well again.
    if (page.peer) page.peer.onStatus?.(() => bar.look());
    else if (!page.mount) noDesktop = true;
    bar.look();
    if (!embed && !enteredViaDoor && !viaTicket && page.peer && here.kind === 'room') {
      // A workspace entered as a member: offered again from the person's other workspaces.
      const entry = {origin: here.host, room: here.room ?? page.peer.workspace};
      remembering(() => {
        rememberEntered(runtime, {...entry, hostDevice, name: invitedTo});
        if (!rememberedWorkspace({kind: 'room', host: entry.origin, room: entry.room})?.name) unnamed = entry;
      });
    }
    if (hostActions) {
      let offered = [];
      const offer = own => {
        offered = remembering(() => workspaceChoices({home: page.entry?.home, homeWorkspace: homeWorkspaceOf(page.entry),
          entered: enteredWorkspaces(), current: here, own})) ?? [];
        hostActions('workspaces').setHostActions(workspaceSwitchActions(offered, {current: workspaceName}));
      };
      offer(false);
      // The way back from a workspace to the person's own desktop, where they
      // may have one: asked of the home's door, so offered once it answers.
      if (!embed && here.kind !== 'own') void ownDesktopAllowed(page.entry?.home).then(open => { if (open) offer(true); });
      switchWorkspace = id => {
        const choice = offered.find(candidate => candidate.id === id);
        return choice ? openWorkspace({choice, page, status}) : undefined;
      };
    }
    // The person's own picture, as their profile keeps it.
    const photos = {
      current: () => ownPhoto,
      choose: async () => {
        const file = await pickImageFile();
        return file ? squarePhoto(file) : null;
      },
      show: url => {
        ownPhoto = url;
        const person = pagePerson(page);
        if (person) void page.mount?.setPersonPhoto?.(person, url);
      },
    };
    // On the person's own desktop no workspace names them: the page does.
    const own = page.peer ? null : pagePerson(page);
    if (own) {
      displayName ??= chosenDisplayName(own) ?? defaultDisplayName(own);
      page.mount?.setOwnPerson?.({user: own, label: displayName});
    }
    const attachIdentity = () => keepIdentity({peer: page.peer, mount: hostActions?.('identity') ?? page.mount, home: page.entry?.home,
      name: () => displayName, runtime, link: page.link, photos, naming: {
        ask: (current, save) => askName(current, save),
        set: chosen => {
          displayName = chooseDisplayName(pagePerson(page), chosen);
          if (page.peer) page.peer.setDisplayName?.(displayName);
          else page.mount?.setOwnPerson?.({user: pagePerson(page), label: displayName});
          return displayName;
        },
      }});
    identityActions = attachIdentity();
    // The first screen (first-run.mjs) sets an agent up through this tab's
    // root and signs in with an agent, both through a login code its host
    // makes, and signs in with a passkey, once the page can.
    if (firstRun) offerFirstRun({page, identityActions, runtime});
    // Leaving the first screen (skipped, or the agent is in) opens the
    // person's own workspace: the one their host keeps for them, where the
    // agent was set up, or else their own desktop where the site gives one.
    // The home workspace let them in already (they stay in it, and find it
    // in the workspace menu), but their own comes first.
    if (firstRun && !embed && here.kind !== 'own') {
      firstRun.whenDone(() => {
        if (hostedOwn) return void openWorkspace({choice: ownChoice(hostedOwn), page, status});
        void ownDesktopAllowed(page.entry?.home).then(open => {
          if (open) void openWorkspace({choice: {id: `${OPEN_WORKSPACE}own`, name: 'Your desktop', own: true}, page, status});
        });
      });
    }
    // A tab an agent just signed in as a device of the person: it paired
    // through the home workspace, and opens the person's own one next.
    if (!embed && page.peer && takeOpenOwn()) {
      void ensureOwnWorkspace(page).then(own => openWorkspace({choice: ownChoice(own), page, status}),
        error => console.warn('signed in: opening your workspace', error));
    }
    // What the kept profile says of linked accounts, or that none is kept:
    // either way a linked account's picture may be worn now, and never
    // over one the person uploaded.
    void followProfile(page, name => { displayName = name; }, kept => {
      ownPhoto = typeof kept.photo === 'string' ? kept.photo : null;
      identityActions?.pictured?.();
      void identityActions?.perform('set-linked', {photoFrom: typeof kept.photo_from === 'string' ? kept.photo_from : null,
        linked: Array.isArray(kept.linked) ? kept.linked : [], keep: false});
    }, () => void identityActions?.perform('set-linked', {photoFrom: null, linked: [], keep: false}));
    // A drawn name someone else here already goes by is drawn again; a
    // name the person chose stays theirs. A page with no host to join has
    // no peer and nobody to share a name with.
    page.peer?.setNameDrawer?.((current, people) => {
      const key = page.peer.userPublicKey;
      if (chosenDisplayName(key)) return null;
      const redrawn = redrawnDisplayName(key, current, people);
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
    // Only a desktop lists host actions and shows banners.
    workspaceActions = hostActions ? createWorkspaceActions({
      peer: page.peer,
      machines: () => (person() ? ownedHosts(person()) : []),
      lobby: async origin => connectPeer({origin, identity: await rootOfThisPage(page.peer), runtime, displayName, live: false}),
      // The invitation's short link, sealed onto this site's shelf, as the
      // sheet makes it: the site keeps ciphertext it cannot read and the key
      // rides in the link's fragment. A shelf that answers with nothing
      // leaves the invitation's own text, which joins just the same.
      inviteLink: async invitation => {
        try {
          return await runtime.sealedInvitationLink(page.entry.base, page.peer.origin, invitation);
        } catch (error) {
          console.warn('invitation not shelved:', error?.message ?? error);
          return invitation;
        }
      },
      open: async ({origin, room, name, hostDevice: holder}) => {
        keepJoinedHost({origin, room, userPublicKey: (await rootOfThisPage(page.peer)).userPublicKey});
        remembering(() => rememberEntered(runtime, {origin, room, name, hostDevice: holder}));
        // Creating a workspace leaves through the same save-and-close path as switching.
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
        canRejoin: () => Boolean(home?.admission) && enteredViaDoor
          && !remembering(() => keptMembership(home))?.paired,
        rejoin: async () => {
          await closeWorkspace(page);
          // The bare entry is the door's: a link naming the workspace the host
          // forgot would be refused. An embed keeps the address it was given.
          if (embed) location.reload(); else loadPage(page.entry.base);
        },
        refuse: async code => {
          tell('unavailable');
          document.documentElement.dataset.refusal = code;
          const known = remembering(() => rememberedWorkspace(here));
          if (known) void settleWorkspace(known, runtime);
          if (embed) return;
          const own = await card(refusalText(code, workspaceName),
            await ownDesktopAllowed(page.entry?.home) ? [['Open your own desktop', true]] : []);
          if (!own) return;
          await closeWorkspace(page);
          loadPage(page.link.format({mode: 'desktop', target: {scope: {kind: 'own'}}}));
        },
      });
    }
    if (page.install && !embed) void setUpFromLink(page.install, setUp);
    // The outcome, for tests and tools driving the page: open, refused or failed.
    document.documentElement.dataset.entry = 'open';
    reachedHost();
    reconnect.done();
    globalThis.tiloMount = page.mount;
    globalThis.tiloRouter = page.router;
    // The page's channel state as text (whom each workspace follows, what waits
    // for keys, what was refused), for tests and tools reading why a
    // record did not arrive. Read only: it hands out no handle on the peer.
    globalThis.tiloChannels = () => page.peer?.channelDiagnostics?.() ?? '';
    // Who is here, as this page's board merged them — each member's
    // `agent`, `label`, `device` and `user` — for tests and tools reading
    // why a member draws as they do. Read only: it hands out no handle on
    // the peer.
    globalThis.tiloPeople = () => JSON.stringify(page.peer?.live?.presenceMembers?.() ?? []);
    // A window that opens after a newer version was deployed finds this
    // version's files gone: offer the reload rather than fail it quietly.
    watchForUpdate({offer: () => offerUpdate({page, status}),
      onChecked: update => { if (update) document.documentElement.dataset.update = update.deployed; }});
    const other = document.getElementById('other-mode');
    if (other) {
      // Written when followed, so it carries whatever is in front by then.
      const refresh = () => {
        other.href = mode === 'app'
          ? (appBar ? page.link.format(appBar.desktopRoute(page.router.route.target)) : page.router.url('desktop'))
          : page.router.url('app');
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
    // A host that did not answer before the page got in (it is starting,
    // or restarting) is asked again on the reconnect screen, with backoff,
    // the tries counted across the reloads: nothing the host refused.
    if (!admitted && hostUnreachable(error)) {
      console.warn('admission: the host did not answer yet, trying again', error?.message);
      const {attempt, since} = retryUnreachable();
      const origin = visitedAt?.host?.origin ?? admitting?.entry?.home?.host?.origin ?? admitting?.scope?.host ?? location.origin;
      await reconnect.wait({error, attempt, since, origin, build: PAGE_BUILD,
        retryAfterMs: Number.isFinite(error?.retryAfter) ? error.retryAfter * 1000 : 0});
      location.reload();
      return;
    }
    if (embed) tell('idle');
    // Stopped after the host let the person in: opening the desktop failed.
    const inside = stoppedInside(admitted, root.arrival);
    // Plain words and a way to try again (index.html); the cause, thrown
    // on, reaches the console and the host's error report.
    // The workspace's name as its people last set it, where the workspace's desktop
    // reached the page; else the name the page was served or remembered.
    const named = (typeof admitted?.workspaceName === 'string' && admitted.workspaceName) || workspaceName;
    const messages = await localeMessagesSoon();
    const said = failureMessage(error, {workspace: named, loaded: Boolean(runtime), admitted: inside, messages});
    // A refusal is the host's answer, which trying again does not change.
    // Marked together with its words: a page that says it was refused
    // while still reading "Loading Tilo…" tells a reader nothing. Stopped
    // only then too: stopped with no answer yet reads as a failure, which
    // shows Try again and gives it the keyboard before a refusal hides it.
    arrive('stopped');
    document.documentElement.dataset.entry = error instanceof AdmissionError || refusedByHost(error) ? 'refused' : 'failed';
    status.textContent = said;
    // Turned away for coming without an invitation: a place to open the
    // one they get.
    if (error?.code === 'invitation-only') offerDoor({messages, named, error, workspace: visitedAt ?? admitting?.entry?.home});
    const known = admitting && remembering(() => rememberedWorkspace(admitting.scope));
    // A declined or unusable invitation, a damaged link or this browser's
    // device key held by another tab says nothing about the workspace.
    const local = error?.name === 'CustodyError' || (error instanceof AdmissionError && !['no-access', 'refused', 'host-changed', 'removed'].includes(error.code));
    if (known && !local) {
      // A workspace this browser was a member of: forgotten when its origin says
      // another host holds it now; else said what is known, and no longer
      // reopened on a bare entry.
      const settled = await settleWorkspace(known, runtime);
      if (settled === 'gone') {
        status.textContent = `${known.name ?? 'This workspace'} no longer exists: its host was set up again since this browser opened it.`;
      } else if (error instanceof AdmissionError && error.code === 'no-access') {
        status.textContent = `${known.name ?? 'This workspace'} did not let this browser in. You may have been removed from it, or it no longer exists.`;
      }
    }
    // The door of a workspace taking people by invitation is a page of its own,
    // with its own ways in: nothing else is offered over it.
    if (!embed && mode === 'desktop' && admitting && error?.code !== 'invitation-only') {
      const refused = document.documentElement.dataset.entry === 'refused';
      void offerOtherWorkspaces({...admitting, runtime}).then(offered => {
        // Nothing to try again and nowhere else of theirs to go: the
        // person's own desktop, where they have one.
        if (!offered && refused) void offerOwnDesktop({...admitting, runtime});
      });
    }
    // Turned away at the door of a workspace taking people by invitation: its
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

/** The remembered workspace `scope` names, if this browser entered it as a member. */
function rememberedWorkspace(scope) {
  if (scope?.kind !== 'room' || !scope.room) return null;
  const origin = new URL(scope.host).origin;
  return enteredWorkspaces().find(entry => entry.origin === origin && entry.room === scope.room) ?? null;
}

/** The workspace this browser was admitted to through the home workspace's door. */
function homeWorkspaceOf(entry) {
  return entry?.home ? keptMembership(entry.home)?.room ?? null : null;
}

/** The address that opens `choice` (`workspaceChoices`): the bare entry for the home. */
function choiceUrl(choice, entry, link) {
  if (choice.home) return entry.base;
  if (choice.own) return link.format({mode: 'desktop', target: {scope: {kind: 'own'}}});
  return link.format({mode: 'desktop', target: {scope: {kind: 'room', host: choice.origin, room: choice.room}}});
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

/**
 * Switch to another of the person's workspaces: save what is pending in the
 * workspace in front (staying there when that fails), close its desktop and its
 * peer, then load the other workspace's page, which admits the person afresh.
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
  // The own desktop is no workspace a bare entry could reopen: the choice kept stays.
  if (!choice.own) remembering(() => chooseWorkspace(choice.home ? null : choice));
  status.textContent = `Opening ${choice.name}…`;
  await closeWorkspace(page);
  loadPage(url);
}

/** Stop following links, then close the workspace's desktop and its peer, saving
 * what they hold; the person decides whether to leave without what cannot
 * be saved. */
async function closeWorkspace(page) {
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
  await closeWorkspace(page);
  location.reload();
}

/** Whether this page may offer the person's own desktop under `home`
 * (`ownDesktopOpen`), asked once per page. */
let ownDesktopGate = null;
const ownDesktopAllowed = home => (ownDesktopGate ??= ownDesktopOpen(home).catch(() => false));

/** The bundle version this page runs, for the reconnect screen's details. */
const PAGE_BUILD = servedVersion(import.meta.url)?.version ?? 'development';

/** Set for the one reload after signing in at the door, to the person
 * signed in as: refused again, the door says that identity is not in yet. */
const DOOR_SIGNED_IN = 'tilo-door-signed-in';

/**
 * On a page turned away for coming without an invitation, the door
 * (index.html, `#arrival-door`). Where the host keeps a wait list
 * (`error.door`), it apologises that Tilo is invite-only for now and asks
 * for an email to invite (`offerWaitList`); else it names the workspace
 * `named` and asks for the invite link someone sends. Under either: signing
 * in with an identity the person already has (a passkey, then the page
 * loads again and joins as them, and a member gets in), and the field to
 * paste an invite link into, opened in this tab. Where the person has a
 * desktop of their own without an invitation (`ownDesktopAllowed`), it waits
 * at the foot of the page (`offerOwnDesktop`); while the home takes people
 * by invitation only, nothing does.
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
    void ownDesktopAllowed(workspace).then(open => {
      if (!open) return;
      own.textContent = say('arrival-door-own', 'Or try Tilo on your own');
      door.querySelector('.door-foot')?.append(own);
    });
  }
  if (waitList) offerWaitList({workspace, contact: error.door.contact, mail: error.door.mail === true, messages});
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
  // An invite link opened in this tab while it shows the door (followed
  // from a mail, or typed into the address bar) changes only the fragment:
  // the page loads again to follow it.
  window.addEventListener('hashchange', () => {
    if (inviteLinkTarget(location.href)) location.reload();
  });
  document.documentElement.dataset.door = waitList ? 'wait-list' : 'invite';
}

/** The door's wait list (door-wait-list.mjs): an email field that leaves
 * the address at the host's door, where it stands once it took it, this
 * browser's way to take it off again, and where questions go (`contact`).
 * A door that mails (`mail`) sends links back to this page: one that
 * confirms the address (`?wait-list-confirm=`) or takes it off
 * (`?wait-list-remove=`), done here and taken out of the address bar. */
function offerWaitList({workspace, contact, mail, messages}) {
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
    arrival: arrival.confirm || arrival.remove ? arrival : null, messages});
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
 * the person's own desktop under this entry, where the person has one
 * (`ownDesktopAllowed`). */
async function offerOwnDesktop({entry, runtime}) {
  const button = document.getElementById('arrival-own');
  if (!button || !await ownDesktopAllowed(entry?.home)) return;
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
  const choices = (remembering(() => workspaceChoices({home: entry.home, homeWorkspace: homeWorkspaceOf(entry),
    entered: enteredWorkspaces(), current: scope})) ?? []).filter(choice => choice.home || link);
  if (!choices.length) return false;
  const choice = await card('Open another of your workspaces',
    [...choices.map(option => [option.unavailable ? `${option.name} (did not open last time)` : option.name, option]), ['Not now', null]]);
  if (!choice) return true;
  // The own desktop is no workspace a bare entry could reopen: the choice kept stays.
  if (!choice.own) remembering(() => chooseWorkspace(choice.home ? null : choice));
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
async function followProfile(page, onName, onKept = () => {}, onNone = () => {}) {
  const userPublicKey = pagePerson(page);
  if (!userPublicKey || (page.peer && !page.peer.setDisplayName)) return;
  const origins = [page.peer?.origin, ...membershipOrigins(userPublicKey),
    ...listedWorkspaces({home: page.entry?.home}).map(workspace => workspace.host.origin)].filter(Boolean);
  try {
    await syncProfile({userPublicKey, origins, identity: currentIdentity(), chosen: chosenDisplayName(userPublicKey), onKept, onNone,
      apply: name => {
        const kept = chooseDisplayName(userPublicKey, name);
        if (page.peer) page.peer.setDisplayName(kept);
        else page.mount?.setOwnPerson?.({user: userPublicKey, label: kept});
        onName(kept);
      }});
  } catch (error) {
    console.warn('identity:', error);
  }
}

/**
 * The person this page acts as: the one its workspace's peer is certified as,
 * else on their own desktop the browser's root identity.
 */
function pagePerson(page) {
  return page.peer ? page.peer.userPublicKey ?? null : currentIdentity()?.userPublicKey ?? null;
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
 * The identity actions of the person this page joined as, when this
 * browser holds them (`browserIdentity`): shown in the desktop, offered once
 * the person has done enough in the workspace to be worth keeping, at a pause.
 */
function keepIdentity({peer, mount, home, name, runtime, link, naming, photos}) {
  const identity = currentIdentity();
  if (!identity?.keepWithPasskey || !mount?.setHostActions) return null;
  // A device paired to a person whose root is elsewhere has nothing to keep.
  if (peer?.userPublicKey && peer.userPublicKey !== identity.userPublicKey) return null;
  const mac = /Mac|iPhone|iPad/.test(navigator.platform ?? navigator.userAgent ?? '');
  const actions = createIdentityActions({
    // A peer that cannot set a name of its own (a guest the relay admitted)
    // goes by none; the person's own desktop (no peer) and a workspace they
    // joined do.
    identity, name, pairing: pairingOf({peer, identity, runtime, link}), naming: !peer || peer.setDisplayName ? naming : null,
    photos: !peer || peer.setDisplayName ? photos : null,
    hosts: () => membershipOrigins(identity.userPublicKey),
    lookups: () => [...membershipOrigins(), ...listedWorkspaces({home}).map(workspace => workspace.host.origin)],
    pause: inputPause(), searchChord: mac ? '⌘K' : 'Ctrl+K', showPairLink: pairLinkSheet, askPairing: pairingAsk,
    standIn: standInOf({peer, identity, runtime}), standInStore: globalThis.localStorage,
    profiled: at => peer?.setProfileChanged?.(at),
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
 * What the first screen asks of this page: login codes from the host it
 * joined (for an agent to set Tilo up through this tab's root, or to sign
 * this tab in as a person who has been here), and a passkey sign-in.
 */
function offerFirstRun({page, identityActions, runtime}) {
  const peer = page.peer;
  if (!peer?.origin || !peer.workspace) {
    firstRun.unavailable({code: 'no-workspace'});
    return;
  }
  const codes = loginCodes(runtime, peer.origin);
  if (identityActions) {
    firstRun.offerSetup(async () => {
      // The agent is set up in the person's own workspace on this host,
      // where the host keeps one; a host that keeps none sets it up here.
      if (!hostedOwn) {
        try {
          hostedOwn = await ensureOwnWorkspace(page);
        } catch (error) {
          if (error?.code !== 'unknown_action') throw error;
          console.info('first run: this host keeps no workspaces of people\'s own;', error?.message);
        }
      }
      const {link} = await identityActions.connectAgent({workspace: hostedOwn?.id});
      const made = await codes.create(link);
      // While the person's root is in this tab alone, a stand-in on the
      // host keeps them from being lost with it.
      void identityActions.keepStandIn().catch(error => console.warn('first run: stand-in', error));
      return made;
    });
    firstRun.offerSignIn(() => identityActions.perform(SIGN_IN.id));
  } else firstRun.unavailable({code: 'no-workspace'});
  // Claimed by a device of the person: this tab asks to be certified as a
  // device of theirs with the invite the claim named, as a pairing link
  // does, and their stand-in on the host approves exactly that request.
  firstRun.offerAgentSignIn(async () => ({...await codes.create(null), signIn: ({user, invite}) => {
    const url = page.link.format({mode: 'desktop', target: {scope: {kind: 'room', host: peer.origin, room: peer.workspace}}});
    try { globalThis.localStorage?.setItem(SEEN, '1'); } catch { /* The pairing link skips the first screen anyway. */ }
    try { globalThis.sessionStorage?.setItem(OPEN_OWN, '1'); } catch { /* Then the home workspace stays in front. */ }
    globalThis.location.assign(`${url}${new URL(url).hash.includes('?') ? '&' : '?'}pair=${pairSecret(user, invite)}`);
    globalThis.location.reload();
  }}));
}

/** The person's own workspace on the home host, once the first screen had
 * it made for the agent it set up. */
let hostedOwn = null;
/** Set for the one load after an agent signed this tab in: open the
 * person's own workspace once paired. */
const OPEN_OWN = 'tilo-open-own-workspace';
const takeOpenOwn = () => {
  try {
    const set = globalThis.sessionStorage?.getItem(OPEN_OWN) === '1';
    if (set) globalThis.sessionStorage.removeItem(OPEN_OWN);
    return set;
  } catch { return false; }
};
/** The workspace switcher's choice of the person's own hosted workspace. */
/**
 * The person's own workspace on the host of the page's peer, with this
 * page's device in it (the shared device entry makes or finds it and redeems
 * its invitation), remembered among the workspaces this browser entered so
 * the switcher offers it and opening it reconnects. Resolves to `{id, name,
 * origin}`.
 */
async function ensureOwnWorkspace(page, store = globalThis.localStorage) {
  const peer = page.peer;
  const origin = new URL(peer.origin).origin;
  const own = await peer.enterOwnWorkspace({
    entered: id => enteredWorkspaces(store).some(kept => kept.origin === origin && kept.room === id),
  });
  const name = own.name || null;
  const hostDevice = hostDeviceOf(peer.host?.hostCert);
  if (hostDevice) rememberEntered(await loadRuntime(), {origin, room: own.workspace, hostDevice, name}, store);
  return {id: own.workspace, name, origin};
}

const ownChoice = own => ({id: `${OPEN_WORKSPACE}${own.id}`, name: own.name ?? 'My workspace', origin: own.origin, room: own.id});

/**
 * The person's stand-in on the host `peer` is in, where the runtime can
 * certify one with the root this tab holds and revoke it again. The root
 * revokes it by signing, as it revokes any of the person's devices: the
 * entry is kept with the person and every session of theirs announces it to
 * its host, so the stand-in approves nothing from then on.
 */
function standInOf({peer, identity, runtime}) {
  if (!peer?.origin || typeof runtime?.keepStandIn !== 'function' || typeof identity?.revokeDevice !== 'function') return null;
  const post = httpsPost(peer.origin);
  return {
    keep: () => runtime.keepStandIn(post, identity),
    retire: () => runtime.retireStandIn(post, identity),
    revoke: key => identity.revokeDevice(key),
  };
}

/**
 * The login codes of the host at `origin`, through the one client of them
 * (`createLoginCode`, `followLoginCode` in wasm): `create(link)` makes one
 * naming this tab's pairing link, or nothing to be signed in by a device of
 * the person, resolving to `{code, expiresAt, follow()}`; `follow()` says
 * where it is (the host's `Polled`). A refusal is a `LoginCodeError` whose
 * `code` says its kind (`not-a-device`, `unknown`, `unreachable`, …).
 */
function loginCodes(runtime, origin) {
  const post = httpsPost(origin);
  return {
    async create(link = null) {
      const made = JSON.parse(await runtime.createLoginCode(post, link));
      return {code: made.code, expiresAt: made.expires_at_ms, follow: async () => JSON.parse(await runtime.followLoginCode(post, made.follow))};
    },
  };
}

/**
 * Pairing another device of `identity` through the host `peer` is in: the
 * link that opens this workspace there as a new device of this person,
 * answering `invite`, and the host's requests waiting for this root to
 * approve them.
 */
function pairingOf({peer, identity, runtime, link}) {
  if (!peer?.origin || !peer.workspace || !runtime?.pendingPairings || !link) return null;
  const post = httpsPost(peer.origin);
  return {
    link: (invite, room = peer.workspace) => {
      const url = link.format({mode: 'desktop', target: {scope: {kind: 'room', host: peer.origin, room}}});
      return `${url}${new URL(url).hash.includes('?') ? '&' : '?'}pair=${pairSecret(identity.userPublicKey, invite)}`;
    },
    pending: async () => JSON.parse(await runtime.pendingPairings(post, identity)),
    answer: (digest, approved) => runtime.answerPairing(post, identity, digest, approved),
  };
}
