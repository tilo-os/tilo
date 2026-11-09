import test from 'node:test';
import assert from 'node:assert/strict';
import {ANYONE_LINK_SECONDS, MAX_FACES, desktopRoute, drawPeople, initial, peopleHere, shareSheet} from './app-bar.mjs';

const tick = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };

class Element {
  constructor(tag) {
    this.tag = tag;
    this.children = [];
    this.attributes = new Map();
    this.listeners = new Map();
    this.style = {};
    this.hidden = false;
    this.disabled = false;
    this.value = '';
    this.className = '';
    this.focused = 0;
  }
  get textContent() { return this._text ?? this.children.map(child => child.textContent).join(''); }
  set textContent(text) { this._text = String(text); this.children = []; }
  append(...nodes) { for (const node of nodes) { node.parent = this; this.children.push(node); } }
  after(node) { this.parent?.append(node); }
  replaceChildren(...nodes) { this.children = []; this.append(...nodes); }
  remove() { if (this.parent) this.parent.children = this.parent.children.filter(child => child !== this); this.parent = null; }
  contains(node) { return node === this || this.children.some(child => child.contains(node)); }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  focus() { this.focused++; }
  select() {}
  find(predicate) { return predicate(this) ? this : this.children.map(child => child.find(predicate)).find(Boolean); }
  async click() { await this.listeners.get('click')?.({}); await tick(); }
}

function fakeDocument() {
  const listeners = new Map();
  const body = new Element('body');
  return {
    body,
    createElement: tag => new Element(tag),
    addEventListener: (type, listener) => listeners.set(type, listener),
    removeEventListener: type => listeners.delete(type),
    press: key => listeners.get('keydown')?.({key, preventDefault() {}}),
  };
}

const here = {app: 'so.tilo.notes', record: '0123456789abcdef'};

test('the people here are each person once, those in this document first, never this page’s own person', () => {
  const members = [
    {publisher: 'd1', user: 'ada', label: 'Ada', colour: '#ff0000', focus: {app: 'so.tilo.chat', record: 'ffff'}},
    {publisher: 'd2', user: 'ada', label: 'Ada', colour: '#ff0000', focus: here},
    {publisher: 'd3', user: 'me', label: 'Me', focus: here},
    {publisher: 'd4', user: 'bo', label: 'Bo', focus: null},
    {publisher: 'd5', label: 'Scribe', agent: true, focus: here},
  ];
  const people = peopleHere(members, {self: 'me', here});
  assert.deepEqual(people.map(person => [person.label, person.here, person.agent]),
    [['Ada', true, false], ['Scribe', true, true], ['Bo', false, false]]);
  assert.deepEqual(peopleHere(null), []);
  assert.equal(initial('ada'), 'A');
  assert.equal(initial(''), '?');
});

test('the bar draws a few faces and counts the rest, and hides when nobody else is here', () => {
  const document = fakeDocument();
  const list = new Element('span');
  const people = ['Ada', 'Bo', 'Cy', 'Di', 'Ed'].map((label, at) => ({key: label, label, colour: '#2a3fd1', agent: at === 1, here: at < 2}));
  drawPeople(list, people, document);
  assert.equal(list.children.length, MAX_FACES + 1);
  assert.equal(list.children.at(-1).textContent, '+2');
  assert.match(list.children[1].className, /bar-agent/);
  assert.match(list.children[2].className, /bar-elsewhere/);
  assert.equal(list.children[0].style.color, '#ffffff');
  assert.equal(list.hidden, false);
  drawPeople(list, [], document);
  assert.equal(list.hidden, true);
});

test('“Open in Tilo” opens the same place on the desktop, a document of its own as its app', () => {
  const scope = {kind: 'room', host: 'https://rooms.example', room: 'w'};
  const primary = {scope, app: 'so.tilo.chat', record: '', resource: {kind: 'chat.message', id: 'chat.message:1'}};
  assert.deepEqual(desktopRoute(primary), {mode: 'desktop', target: primary});
  assert.deepEqual(desktopRoute({scope, app: 'so.tilo.word-processor', record: 'uuid:1', view: 'outline'}),
    {mode: 'desktop', target: {scope, app: 'so.tilo.word-processor'}});
});

test('Share copies the link for the workspace’s people, or makes one for anyone with the role chosen', async () => {
  const document = fakeDocument();
  const anchor = new Element('button');
  const written = [];
  globalThis.navigator ??= {};
  Object.defineProperty(globalThis.navigator, 'clipboard', {configurable: true, value: {writeText: async text => { written.push(text); }}});
  const asked = [];
  const closed = shareSheet({anchor, document, workspaceName: 'Reading Group',
    link: () => 'https://tilo.sh/app/#/@h/w/1/so.tilo.notes/-',
    anyoneLink: async role => { asked.push(role); return `https://tilo.sh/app/#/@h/w/1/so.tilo.notes/-?invite=${role}`; }});
  const sheet = document.body.children[0];
  assert.equal(anchor.getAttribute('aria-expanded'), 'true');
  assert.ok(sheet.find(node => node.textContent === 'People in Reading Group can open this link.'));
  const buttons = [];
  sheet.find(node => { if (node.tag === 'button') buttons.push(node); return false; });
  const [members, anyone] = buttons;
  await members.click();
  assert.equal(members.textContent, 'Copied');
  sheet.find(node => node.tag === 'select').value = 'viewer';
  await anyone.click();
  assert.deepEqual(asked, ['viewer']);
  assert.deepEqual(written, ['https://tilo.sh/app/#/@h/w/1/so.tilo.notes/-', 'https://tilo.sh/app/#/@h/w/1/so.tilo.notes/-?invite=viewer']);
  assert.ok(sheet.find(node => node.textContent === `Copied. It lets anyone in for ${ANYONE_LINK_SECONDS / 86_400} days.`));
  document.press('Escape');
  await closed;
  assert.equal(document.body.children.length, 0);
  assert.equal(anchor.getAttribute('aria-expanded'), 'false');
});

test('a refused link for anyone says the host’s words and keeps the sheet', async () => {
  const document = fakeDocument();
  const anchor = new Element('button');
  void shareSheet({anchor, document, workspaceName: '', link: () => '',
    anyoneLink: async () => { throw new Error('Your role here does not let you invite people'); }});
  const sheet = document.body.children[0];
  assert.ok(sheet.find(node => node.textContent === 'People in this workspace can open this link.'));
  const buttons = [];
  sheet.find(node => { if (node.tag === 'button') buttons.push(node); return false; });
  await buttons[1].click();
  assert.ok(sheet.find(node => node.textContent === 'Your role here does not let you invite people'));
  assert.equal(buttons[1].disabled, false);
  document.press('Escape');
});
