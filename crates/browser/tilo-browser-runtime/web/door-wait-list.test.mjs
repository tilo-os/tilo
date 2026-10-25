import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {parseMessages} from './arrival.mjs';
import {mountWaitList, waitListWords} from './door-wait-list.mjs';
import {HostAdmissionError} from './workspaces.mjs';

const locale = name => parseMessages(readFileSync(new URL(`locales/${name}.ftl`, import.meta.url), 'utf8'));
const tick = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };

class Element {
  constructor(tag = 'p') {
    this.tag = tag;
    this.children = [];
    this.attributes = new Map();
    this.listeners = new Map();
    this.dataset = {};
    this.value = '';
    this.disabled = false;
    this.hidden = false;
    this.focused = 0;
    const classes = new Set();
    this.classList = {add: name => classes.add(name), remove: name => classes.delete(name), contains: name => classes.has(name)};
  }
  get textContent() { return this.children.map(child => (typeof child === 'string' ? child : child.textContent)).join(''); }
  set textContent(text) { this.children = text ? [String(text)] : []; }
  replaceChildren(...nodes) { this.children = nodes.filter(node => node !== ''); }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  removeAttribute(name) { this.attributes.delete(name); }
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  focus() { this.focused++; }
  async fire(type) { await this.listeners.get(type)?.({preventDefault() {}}); await tick(); }
}

const workspace = {name: 'Lounge', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
const NAMES = ['form', 'email', 'submit', 'done', 'againLine', 'againPrompt', 'again', 'change', 'leave', 'note', 'contact'];

/** The door's wait list, mounted with `kept` left on an earlier visit (or
 * not), at a door that mails or not, after a mailed link (`arrival`) or
 * not, the host answering as given. */
function door({kept = null, contact = null, mail = false, arrival = null, messages = null, join, leave = async () => {},
  confirm = async () => true, standing = async () => null} = {}) {
  const elements = Object.fromEntries(NAMES.map(name => [name, new Element()]));
  const root = {dataset: {}};
  const calls = [];
  let listing = kept;
  const document = {createElement: tag => new Element(tag)};
  const mounted = mountWaitList({elements, root, workspace, contact, mail, arrival, messages, document,
    listing: asked => { assert.equal(asked, workspace); return listing; },
    join: async request => {
      calls.push(['join', request.email]);
      const left = await join(request);
      listing = {email: left.email, pending: left.mail};
      return left;
    },
    leave: async request => {
      calls.push(['leave', request.secret ?? 'kept']);
      await leave(request);
      listing = null;
    },
    confirm: async request => { calls.push(['confirm', request.token]); return confirm(request); },
    standing: async request => { calls.push(['standing', request.workspace.name]); return standing(request); }});
  const shown = () => ({
    state: root.dataset.waitList, listed: 'waitListed' in root.dataset, done: elements.done.textContent,
    again: elements.againLine.hidden ? '' : `${elements.againPrompt.textContent} ${elements.again.textContent}`,
    off: `${elements.change.textContent} ${elements.leave.textContent}`.trim(), note: elements.note.textContent,
    alert: elements.note.getAttribute('role') === 'alert', contact: elements.contact.textContent,
  });
  return {elements, root, calls, mounted, shown};
}

test('a first visit shows the form, saying what the address is for', async () => {
  const {shown, elements, mounted, calls} = door();
  await mounted.settled;
  assert.deepEqual(shown(), {state: 'form', listed: false, done: '', again: '', off: '',
    note: 'We only use your email to send you an invite.', alert: false, contact: ''});
  assert.equal(mounted.state(), 'form');
  assert.equal(elements.submit.textContent, 'Join the wait list');
  assert.equal(elements.email.placeholder, 'you@example.com');
  assert.deepEqual(calls, [], 'nothing kept here: the door is not asked');
  assert.equal(door({mail: true}).shown().note, 'We email you a link to confirm, then your invite. Nothing else.');
});

test('joining at a door that mails says to check the inbox, with a resend and a way off', async () => {
  const {elements, shown, calls} = door({mail: true, join: async ({email}) => ({email: email.trim(), mail: true})});
  elements.email.value = ' Ada@Example.org ';
  await elements.form.fire('submit');
  assert.deepEqual(calls, [['join', 'Ada@Example.org']]);
  assert.deepEqual(shown(), {state: 'inbox', listed: true, done: 'Check your inbox: we sent a link to Ada@Example.org to confirm.',
    again: 'Didn’t get it? Send it again', off: 'Changed your mind? Take me off the list', note: '', alert: false, contact: ''});
  assert.equal(elements.email.value, '', 'the field is not left holding the address');

  // Asked again: the address is left once more, and the door says how
  // often a link goes out.
  await elements.again.fire('click');
  assert.deepEqual(calls.at(-1), ['join', 'Ada@Example.org']);
  assert.equal(shown().state, 'inbox');
  assert.equal(shown().note, 'Asked again. We send at most one link an hour, so check your spam folder too.');
});

test('joining at a door that sends no mail lists the address and says plainly no mail comes', async () => {
  const {elements, shown} = door({join: async ({email}) => ({email: email.trim(), mail: false})});
  elements.email.value = 'ada@example.org';
  await elements.form.fire('submit');
  assert.deepEqual(shown(), {state: 'listed', listed: true, done: 'You’re on the list (ada@example.org)', again: '',
    off: 'Changed your mind? Take me off the list',
    note: 'No email is sent from here: whoever invites you sends you the link.', alert: false, contact: ''});
});

test('a browser that left its address before is told where it stands instead of the form', async () => {
  for (const [stands, state, done] of [
    ['pending', 'inbox', 'Check your inbox: we sent a link to ada@example.org to confirm.'],
    ['listed', 'listed', 'You’re on the list (ada@example.org)'],
    ['invited', 'invited', 'You’re invited: the link is in your inbox.'],
    [null, 'inbox', 'Check your inbox: we sent a link to ada@example.org to confirm.'],
  ]) {
    const {shown, mounted, calls} = door({mail: true, kept: {email: 'ada@example.org', pending: true}, standing: async () => stands});
    assert.equal(shown().state, 'inbox', 'at once, from what this browser kept');
    await mounted.settled;
    assert.deepEqual(calls, [['standing', 'Lounge']]);
    assert.equal(shown().state, state, String(stands));
    assert.equal(shown().done, done);
    assert.equal(shown().off, 'Changed your mind? Take me off the list');
  }
  // Gone (joined, or taken off elsewhere): the form.
  const gone = door({kept: {email: 'ada@example.org', pending: false}, standing: async () => 'gone'});
  await gone.mounted.settled;
  assert.equal(gone.shown().state, 'form');
  // Invited where the door sends no mail: the invitation comes from the inviter.
  const plain = door({kept: {email: 'ada@example.org', pending: false}, standing: async () => 'invited'});
  await plain.mounted.settled;
  assert.equal(plain.shown().done, 'You’re invited: whoever invited you sends you the link.');
});

test('a confirmation link lands on the door, listed; one no longer waiting says so over the form', async () => {
  const token = 'c1'.repeat(32);
  const kept = {email: 'ada@example.org', pending: true};
  const confirmed = door({mail: true, kept, arrival: {confirm: token}});
  await confirmed.mounted.settled;
  assert.deepEqual(confirmed.calls, [['confirm', token]]);
  assert.equal(confirmed.shown().state, 'listed');
  assert.equal(confirmed.shown().done, 'You’re on the list (ada@example.org)');
  assert.equal(confirmed.shown().note, '');
  // Opened in another browser: listed, without the address, and without a
  // way off (the mail's own removal link is that).
  const elsewhere = door({mail: true, arrival: {confirm: token}});
  await elsewhere.mounted.settled;
  assert.deepEqual([elsewhere.shown().done, elsewhere.shown().off], ['You’re on the list.', '']);
  const late = door({mail: true, arrival: {confirm: token}, confirm: async () => false});
  await late.mounted.settled;
  assert.equal(late.shown().state, 'expired');
  assert.equal(late.shown().note, 'That link no longer works. Leave your address again.');
});

test('a removal link takes the address off and lands on the form, saying so', async () => {
  const secret = 'd2'.repeat(32);
  const {mounted, calls, shown} = door({mail: true, arrival: {remove: secret}});
  await mounted.settled;
  assert.deepEqual(calls, [['leave', secret]]);
  assert.deepEqual(shown(), {state: 'left', listed: false, done: '', again: '', off: '', note: 'You’re off the list.', alert: false, contact: ''});
});

test('taking the address off shows the form again, saying it is off', async () => {
  const {elements, shown, calls, mounted} = door({kept: {email: 'ada@example.org', pending: false}});
  await mounted.settled;
  await elements.leave.fire('click');
  assert.deepEqual(calls.at(-1), ['leave', 'kept']);
  assert.deepEqual(shown(), {state: 'left', listed: false, done: '', again: '', off: '', note: 'You’re off the list.', alert: false, contact: ''});
  assert.equal(elements.note.getAttribute('role'), 'status');
  assert.ok(elements.email.focused > 0, 'the field is ready for another address');
});

test('a removal the host could not do says so and keeps the address listed', async () => {
  let refusal = new HostAdmissionError('unavailable', 'the room is restarting');
  const {elements, shown, mounted} = door({kept: {email: 'ada@example.org', pending: false}, leave: async () => { throw refusal; }});
  await mounted.settled;
  await elements.leave.fire('click');
  assert.equal(shown().state, 'listed');
  assert.ok(shown().listed && shown().alert);
  assert.match(shown().note, /didn’t go through/);
  refusal = new HostAdmissionError('too-many', 'slow down', 60);
  await elements.leave.fire('click');
  assert.match(shown().note, /Too many tries/);
  assert.equal(elements.leave.disabled, false);
});

test('an address the door refused is said beside the field, which then reads as typed again', async () => {
  const {elements, shown} = door({join: async () => { throw new HostAdmissionError('invalid-email', 'no'); }});
  elements.email.value = 'not an address';
  await elements.form.fire('submit');
  assert.equal(shown().state, 'form');
  assert.ok(shown().alert);
  assert.match(shown().note, /doesn’t look like an email/);
  assert.equal(elements.email.getAttribute('aria-invalid'), 'true');
  await elements.email.fire('input');
  assert.equal(shown().note, 'We only use your email to send you an invite.');
  assert.equal(elements.email.getAttribute('aria-invalid'), null);
});

test('the contact the host names is a line of its own, linked, in every state; none, no line', async () => {
  for (const [contact, href, text] of [['hello@example.org', 'mailto:hello@example.org', 'hello@example.org'],
    ['https://example.org/privacy', 'https://example.org/privacy', 'example.org/privacy']]) {
    const {elements, shown, mounted} = door({contact, kept: {email: 'ada@example.org', pending: false}});
    await mounted.settled;
    assert.equal(shown().contact, `Questions or removal: ${text}`);
    const link = elements.contact.children.find(child => typeof child !== 'string');
    assert.equal(link.href, href);
    await elements.leave.fire('click');
    assert.equal(shown().contact, `Questions or removal: ${text}`);
  }
  assert.equal(door().shown().contact, '');
});

test('the door’s wait-list words in each state, in German too', () => {
  const de = locale('de-DE');
  assert.equal(waitListWords({state: 'form'}).done, null);
  assert.equal(waitListWords({state: 'listed', email: 'a@b.org', mail: true, messages: de}).done, 'Du stehst auf der Liste (a@b.org)');
  assert.equal(waitListWords({state: 'inbox', email: 'a@b.org', messages: de}).done,
    'Schau in dein Postfach: Wir haben einen Link an a@b.org geschickt, mit dem du bestätigst.');
  assert.deepEqual(waitListWords({state: 'inbox', email: 'a@b.org', messages: de}).again, {prompt: 'Nichts angekommen?', button: 'Noch mal senden'});
  assert.equal(waitListWords({state: 'invited', mail: true, messages: de}).leave, 'Nimm mich von der Liste');
  assert.equal(waitListWords({state: 'left', messages: de}).note, 'Du stehst nicht mehr auf der Liste.');
  assert.deepEqual(waitListWords({contact: 'x@y.org', messages: de}).contact, {before: 'Fragen oder Austragen: ', contact: 'x@y.org', after: ''});
  assert.equal(waitListWords({state: 'inbox', email: 'a@b.org'}).note, null);
});
