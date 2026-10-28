/**
 * The wait list at the door of a room that takes people by invitation
 * (index.html, `#arrival-wait` and the lines around it), as this browser
 * stands with it:
 *
 * - `form`: nothing left here. An email field and what the address is for.
 * - `inbox`: the address was left at a door that mails (`mail`), and waits
 *   to be confirmed: "Check your inbox", with a way to have the link sent
 *   again and a way to take the address off.
 * - `listed`: the address is on the list (confirmed, or left at a door that
 *   sends no mail, which says so), with the way off.
 * - `invited`: someone invited the address; the invitation is in its inbox
 *   (or comes from whoever invited it, where the door sends no mail).
 * - `left`: it was just taken off: the form again, saying so.
 * - `expired`: a confirmation link no longer worked: the form again, saying
 *   so.
 *
 * Taking it off needs no inbox: the door handed this browser a secret when
 * the address was left (`leaveOnWaitList`), which `leaveWaitList` sends
 * back; every mail's removal link carries one too. The links mailed land on
 * this door (`arrival`): a confirmation link confirms and shows `listed`, a
 * removal link takes the address off and shows `left`. Where the host names
 * a contact (`TILO_ROOMS_CONTACT`), a line under it says where questions and
 * removal requests go; without one, nothing.
 */
import {DESKTOP_LANGUAGE, localized, waitListFailure} from './arrival.mjs';
import {confirmWaitList, leaveOnWaitList, leaveWaitList, waitListing, waitListStanding} from './workspaces.mjs';

/** Where the contact goes in its line, which is split there to link it. */
const CONTACT_AT = '\u0000';

/** The states in which the address is on the list, with the way off. */
const ON_THE_LIST = new Set(['inbox', 'listed', 'invited']);

/**
 * The words of the door's wait list in `state` (see above), for the
 * address `email` (maybe unknown), at a door that mails or not (`mail`),
 * in `messages`' language, where this browser can take it off
 * (`leavable`, else the mails' removal links do): `done` (the line in place of the form, or
 * `null` while the form shows), `again` (`{prompt, button}` to have the
 * link sent again, or `null`), `change` and `leave` (the way off, or
 * `null`), `note` (under the field or the line, or `null`) and `contact`
 * (`{before, contact, after}` around the contact `contact` names, or
 * `null`).
 */
export function waitListWords({state = 'form', email = '', mail = false, leavable = true, contact = null, messages = null} = {}) {
  const say = (key, english, args) => localized(messages, key, english, args);
  const listed = ON_THE_LIST.has(state);
  const off = listed && leavable;
  let done = null;
  if (state === 'inbox') done = say('arrival-wait-inbox', 'Check your inbox: we sent a link to { $email } to confirm.', {email});
  if (state === 'listed') {
    done = email ? say('arrival-wait-listed', 'You’re on the list ({ $email })', {email})
      : say('arrival-wait-confirmed', 'You’re on the list.');
  }
  if (state === 'invited') {
    done = mail ? say('arrival-wait-invited', 'You’re invited: the link is in your inbox.')
      : say('arrival-wait-invited-no-mail', 'You’re invited: whoever invited you sends you the link.');
  }
  let note = mail ? say('arrival-wait-privacy-mail', 'We email you a link to confirm, then your invite. Nothing else.')
    : say('arrival-wait-privacy', 'We only use your email to send you an invite.');
  if (state === 'left') note = say('arrival-wait-left', 'You’re off the list.');
  if (state === 'expired') note = say('arrival-wait-expired', 'That link no longer works. Leave your address again.');
  if (listed) note = state === 'listed' && !mail ? say('arrival-wait-no-mail', 'No email is sent from here: whoever invites you sends you the link.') : null;
  let line = null;
  if (contact) {
    const [before, after = ''] = say('arrival-wait-contact', 'Questions or removal: { $contact }', {contact: CONTACT_AT}).split(CONTACT_AT);
    line = {before, contact, after};
  }
  return {
    done,
    again: state === 'inbox' && email
      ? {prompt: say('arrival-wait-again-prompt', 'Didn’t get it?'), button: say('arrival-wait-again', 'Send it again')} : null,
    change: off ? say('arrival-wait-change', 'Changed your mind?') : null,
    leave: off ? say('arrival-wait-leave', 'Take me off the list') : null,
    note,
    contact: line,
  };
}

/** The state an entry's standing at the door (`waitListStanding`) shows. */
const STANDING = {pending: 'inbox', listed: 'listed', invited: 'invited', gone: 'form'};

/**
 * Wire the door's wait list for `workspace`: `elements` are the form
 * (`form`, `email`, `submit`), the line in its place (`done`), the way to
 * have the link sent again (`againLine`, `againPrompt`, `again`, a
 * button), the way off (`change`, `leave`, a button), the note (`note`)
 * and the contact's line (`contact`). `root` (the page's element) carries
 * `data-wait-list` (the state) and `data-wait-listed` while the address is
 * on the list. `mail`: the door mails. `arrival` is what a mailed link
 * brought: `{confirm}` or `{remove}`, a secret. `join`, `leave`,
 * `listing`, `confirm` and `standing` reach the host (`leaveOnWaitList`,
 * `leaveWaitList`, `waitListing`, `confirmWaitList`,
 * `waitListStanding`). Returns `state()` and `settled`, which resolves once
 * the door knows where the entry stands.
 */
export function mountWaitList({elements, root, workspace, contact = null, mail = false, arrival = null, messages = null,
  document = globalThis.document, join = leaveOnWaitList, leave = leaveWaitList, listing = waitListing,
  confirm = confirmWaitList, standing = waitListStanding}) {
  const {form, email, submit, done, againLine, againPrompt, again, change, leave: leaveButton, note, contact: contactLine} = elements;
  const say = (key, english, args) => localized(messages, key, english, args);
  let state = 'form';
  let address = '';
  const problem = text => {
    note.textContent = text;
    note.classList.add('door-problem');
    note.setAttribute('role', 'alert');
  };
  const show = (next, known = address) => {
    state = next;
    address = ON_THE_LIST.has(state) ? known : '';
    const words = waitListWords({state, email: address, mail, leavable: Boolean(listing(workspace)), contact, messages});
    root.dataset.waitList = state;
    if (words.done) root.dataset.waitListed = '';
    else delete root.dataset.waitListed;
    done.textContent = words.done ?? '';
    againLine.hidden = !words.again;
    againPrompt.textContent = words.again?.prompt ?? '';
    again.textContent = words.again?.button ?? '';
    change.textContent = words.change ?? '';
    leaveButton.textContent = words.leave ?? '';
    note.classList.remove('door-problem');
    note.removeAttribute('role');
    note.textContent = words.note ?? '';
    if (state === 'left' || state === 'expired') note.setAttribute('role', 'status');
    if (!words.contact) {
      contactLine.replaceChildren();
      return;
    }
    const page = words.contact.contact.startsWith('https://');
    const link = Object.assign(document.createElement('a'), {
      textContent: page ? words.contact.contact.slice('https://'.length) : words.contact.contact,
      href: page ? words.contact.contact : `mailto:${words.contact.contact}`, rel: 'noopener',
    });
    if (page) link.target = '_blank';
    contactLine.replaceChildren(words.contact.before, link, words.contact.after);
  };
  const failed = refusal => {
    console.warn('wait list:', refusal?.message ?? refusal);
    problem(refusal?.code === 'too-many'
      ? say('arrival-wait-leave-too-many', 'Too many tries from your network just now. Try again later.')
      : waitListFailure(undefined, messages));
  };
  email.placeholder = say('arrival-wait-placeholder', 'you@example.com');
  email.setAttribute('aria-label', say('arrival-wait-email', 'Email'));
  submit.textContent = say('arrival-wait-join', 'Join the wait list');
  const kept = listing(workspace);
  // What this browser knows at once; where the entry stands is asked next.
  show(kept ? (kept.pending ? 'inbox' : 'listed') : 'form', kept?.email ?? '');
  const settle = async () => {
    if (arrival?.confirm) {
      const confirmed = await confirm({workspace, token: arrival.confirm});
      show(confirmed ? 'listed' : 'expired', listing(workspace)?.email ?? '');
      return;
    }
    if (arrival?.remove) {
      await leave({workspace, secret: arrival.remove});
      show('left');
      return;
    }
    if (!kept) return;
    const stands = await standing({workspace});
    // Unless the visitor moved on meanwhile.
    if (stands && state === (kept.pending ? 'inbox' : 'listed')) show(STANDING[stands] ?? state, kept.email);
  };
  const settled = settle().catch(failed);
  const leaveIt = async () => {
    const value = email.value.trim() || address;
    if (!value) {
      email.focus();
      return;
    }
    submit.disabled = true;
    again.disabled = true;
    try {
      // Its mail is in the language this page is in.
      const left = await join({workspace, email: value, language: DESKTOP_LANGUAGE});
      email.value = '';
      return left;
    } finally {
      submit.disabled = false;
      again.disabled = false;
    }
  };
  email.addEventListener('input', () => {
    if (email.getAttribute('aria-invalid') !== 'true') return;
    email.removeAttribute('aria-invalid');
    show('form');
  });
  form.addEventListener('submit', async event => {
    event.preventDefault();
    if (submit.disabled) return;
    if (!email.value.trim()) {
      email.focus();
      return;
    }
    try {
      const left = await leaveIt();
      show(left.mail ? 'inbox' : 'listed', left.email);
      leaveButton.focus?.();
    } catch (refusal) {
      console.warn('wait list:', refusal?.message ?? refusal);
      problem(waitListFailure(refusal?.code, messages));
      email.setAttribute('aria-invalid', 'true');
      email.focus();
    }
  });
  again.addEventListener('click', async () => {
    if (again.disabled || !address) return;
    try {
      const left = await leaveIt();
      show(left.mail ? 'inbox' : 'listed', left.email);
      note.textContent = say('arrival-wait-again-sent', 'Asked again. We send at most one link an hour, so check your spam folder too.');
      note.setAttribute('role', 'status');
    } catch (refusal) {
      failed(refusal);
    }
  });
  leaveButton.addEventListener('click', async () => {
    if (leaveButton.disabled) return;
    leaveButton.disabled = true;
    try {
      await leave({workspace});
      show('left');
      email.focus();
    } catch (refusal) {
      failed(refusal);
    } finally {
      leaveButton.disabled = false;
    }
  });
  return {state: () => state, settled};
}
