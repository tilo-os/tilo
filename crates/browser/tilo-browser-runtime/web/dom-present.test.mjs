import test from 'node:test';
import assert from 'node:assert/strict';
import {createDomPresenter} from './dom-present.mjs';
import {semanticBridge} from './semantics.mjs';

// Just the DOM the presenter and the semantic mirror touch.
class Node {
  constructor(tag) {
    this.localName = tag; this.children = []; this.parentNode = null; this.attributes = new Map();
    this.dataset = {}; this.listeners = new Map(); this.textContent = ''; this.scrollTop = 0;
    const props = new Map();
    this.style = {props, cssText: '', setProperty: (name, value) => props.set(name, value)};
    for (const name of ['position', 'left', 'top', 'width', 'height', 'opacity']) {
      Object.defineProperty(this.style, name, {get: () => props.get(name), set: value => props.set(name, value)});
    }
  }
  get firstChild() { return this.children[0] ?? null; }
  get nextSibling() { const siblings = this.parentNode?.children; return siblings?.[siblings.indexOf(this) + 1] ?? null; }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  removeAttribute(name) { this.attributes.delete(name); }
  addEventListener(type, listener) { this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]); }
  dispatch(type) { for (const listener of this.listeners.get(type) ?? []) listener({type, target: this}); }
  remove() { if (this.parentNode) this.parentNode.children = this.parentNode.children.filter(c => c !== this); this.parentNode = null; }
  insertBefore(child, before) {
    child.remove();
    const at = before ? this.children.indexOf(before) : -1;
    if (at < 0) this.children.push(child); else this.children.splice(at, 0, child);
    child.parentNode = this;
  }
  append(...children) { for (const child of children) this.insertBefore(child, null); }
  prepend(child) { this.insertBefore(child, this.firstChild); }
  contains(node) { return node === this || this.children.some(child => child.contains(node)); }
  replaceChildren() { for (const child of this.children) child.parentNode = null; this.children = []; }
  getBoundingClientRect() { return {left: 0, top: 0, right: 1e4, bottom: 1e4}; }
  closest() { return null; }
  get scrollHeight() { return 1e4; }
  get clientHeight() { return 100; }
}
globalThis.document = {
  createElement: tag => new Node(tag), createElementNS: (_, tag) => new Node(tag),
  fonts: {add() {}}, activeElement: null, body: new Node('body'),
};
globalThis.FontFace = class { load() { return Promise.resolve(); } };

const all = node => [node, ...node.children.flatMap(all)];
const meta = (extra = {}) => JSON.stringify({background: '#fff', ...extra});
const frame = (presenter, ops, extra) => presenter.present(meta(extra), JSON.stringify(ops), []);
const box = (x, y) => ['r', x, y, 10, 10, 0, 'red', '', 0];
const text = (words, x, y) => ['t', words, x, y, 12, 'face', '', 'black', 0];

test('a widget keeps its element by key, and a move is one write', () => {
  const layer = new Node('div');
  const presenter = createDomPresenter(layer);
  const widget = (key, x) => [['k', key, x, 0, 1], box(x, 0), text('hi', x + 2, 10), ['ke']];
  frame(presenter, [...widget('a', 0), ...widget('b', 100)]);
  const [a, b] = layer.children;
  assert.deepEqual(layer.children.map(n => n.children.length), [2, 2]);
  // An op inserted into `a` renumbers nothing in `b`; `b` moved.
  frame(presenter, [['k', 'a', 0, 0, 1], box(0, 20), box(0, 0), text('hi', 2, 10), ['ke'], ...widget('b', 140)]);
  assert.equal(layer.children[0], a);
  assert.equal(layer.children[1], b);
  assert.equal(presenter.stats.created, 1, 'only the inserted box is new');
  assert.equal(b.style.props.get('left'), '140px');
  assert.equal(b.children[0].style.props.get('left'), '0px', 'its children stand where they stood in it');
  // A widget gone takes its element with it; one that comes back by key is
  // the same element only while it is drawn.
  frame(presenter, widget('b', 140));
  assert.deepEqual(layer.children, [b]);
});

test('painted text is hidden from assistive technology', () => {
  const layer = new Node('div');
  const presenter = createDomPresenter(layer);
  frame(presenter, [['k', 'a', 0, 0, 1], text('Hello', 0, 10), ['ke']]);
  const span = all(layer).find(n => n.localName === 'span');
  assert.equal(span.textContent, 'Hello');
  const flow = layer.children[0].children[0];
  assert.ok(all(flow).includes(span));
  assert.equal(flow.getAttribute('aria-hidden'), 'true');
});

test('the lines of one block are one text flow, each run where the toolkit set it', () => {
  const layer = new Node('div');
  const presenter = createDomPresenter(layer);
  // Two lines, the first of two runs (a bold word after a plain one), then a
  // caret box, then a label further up: a flow of its own.
  const ops = [['k', 'a', 0, 0, 1],
    text('The quick ', 10, 20), text('brown', 70, 20), text('fox jumps', 10, 40),
    box(0, 0), text('label', 10, 5), ['ke']];
  frame(presenter, ops);
  const widget = layer.children[0];
  const flows = widget.children.filter(n => n.localName === 'div' && n.children[0]?.localName === 'div');
  assert.equal(flows.length, 2, 'the label after the box is a flow of its own');
  const [flow] = flows;
  assert.deepEqual(flow.children.map(line => line.children.map(span => span.textContent)), [['The quick ', 'brown'], ['fox jumps']]);
  const [first, second] = flow.children;
  assert.equal(first.style.props.get('height'), '20px', 'a line is as tall as the step to the next');
  assert.equal(first.children[0].style.props.get('width'), '60px', 'a run followed on its line is as wide as the step to the next run');
  assert.equal(first.children[1].style.props.get('margin-left'), '0px');
  assert.equal(second.children[0].style.props.get('margin-left'), '0px');
  // Typing into the first line rewrites its text and nothing is created.
  frame(presenter, [['k', 'a', 0, 0, 1],
    text('The quicker ', 10, 20), text('brown', 80, 20), text('fox jumps', 10, 40),
    box(0, 0), text('label', 10, 5), ['ke']]);
  assert.equal(presenter.stats.created, 0);
  assert.equal(first.children[0].textContent, 'The quicker ');
  assert.equal(layer.children[0].children.find(n => n === flow), flow);
});

test('a scrolling widget\'s viewport is a native scroll container', () => {
  const layer = new Node('div');
  const scrolled = [];
  const presenter = createDomPresenter(layer, {onScroll: (key, offset) => scrolled.push([key, offset])});
  const ops = offset => [['k', 's', 0, 0, 1], ['c', 0, 50, 200, 100, 0, 1], box(0, 60 - offset), ['e'], ['ke']];
  const scrolls = offset => ({scrolls: [{key: 's', offset, content: 1000, viewport: 100, rect: [0, 50, 200, 100]}]});
  frame(presenter, ops(30), scrolls(30));
  const clip = layer.children[0].children[0];
  assert.equal(clip.style.props.get('overflow-y'), 'auto');
  assert.equal(clip.scrollTop, 30, 'scrolled where the widget is');
  const content = clip.children.find(n => !n.tiloForeign);
  assert.equal(content.style.props.get('top'), '10px', 'placed where it lies in the content');
  // The person scrolls: the widget hears, and a frame drawn at the new
  // offset moves nothing.
  clip.scrollTop = 80;
  clip.dispatch('scroll');
  assert.deepEqual(scrolled, [['s', 80]]);
  frame(presenter, ops(80), scrolls(80));
  assert.equal(content.style.props.get('top'), '10px');
  assert.equal(presenter.stats.writes, 0, 'nothing to write');
  assert.equal(presenter.scrollerAt(10, 10, 1), clip);
});

test('a semantic element lives in the element of the widget it describes', () => {
  const layer = new Node('div'), mirror = new Node('div');
  const presenter = createDomPresenter(layer);
  const bridge = semanticBridge(mirror, () => true, () => {}, undefined, undefined, presenter.host);
  frame(presenter, [['k', 'aa', 10, 20, 1], box(10, 20), ['ke']]);
  const widget = layer.children[0];
  bridge.update({root: {path: 'app', paint_key: 'aa', rect: [10, 20, 50, 30], semantics: {role: 'Button', name: 'Save'}}});
  const button = widget.children.find(n => n.dataset.tiloSemantic);
  assert.ok(button, 'hosted in the painted widget');
  assert.equal(button.getAttribute('aria-label'), 'Save');
  assert.equal(button.style.left, '0px', 'placed from the widget\'s origin');
  assert.equal(button.style.pointerEvents, 'none', 'the pointer is the runtime\'s');
  assert.equal(mirror.children.length, 0);
  // The next frame keeps it there, ahead of what is painted.
  frame(presenter, [['k', 'aa', 10, 20, 1], box(10, 20), box(12, 22), ['ke']]);
  assert.ok(widget.children.includes(button));
  // A widget no longer drawn hands it back to the mirror.
  frame(presenter, []);
  bridge.update({root: {path: 'app', paint_key: 'aa', rect: [10, 20, 50, 30], semantics: {role: 'Button', name: 'Save'}}});
  assert.ok(mirror.children.includes(button));
});

test('a single-line field left to the page is its own input, set where the toolkit would set it', () => {
  const layer = new Node('div'), mirror = new Node('div');
  const presenter = createDomPresenter(layer);
  const bridge = semanticBridge(mirror, () => true, () => {}, undefined, undefined, presenter.host);
  const field = {key: 'ff', area: [10, 20, 180, 32], baseline: [22, 40], size: 14, family: 'face', variations: '', color: 'rgba(0,0,0,1)'};
  frame(presenter, [['k', 'ff', 10, 20, 1], box(10, 20), ['ke']], {fields: [field]});
  bridge.update({root: {path: 'app', paint_key: 'ff', rect: [10, 20, 200, 32],
    semantics: {role: 'TextInput', state: {focusable: true, autofill: 'Username'}, value: 'ada'}}});
  const input = layer.children[0].children.find(n => n.dataset.tiloSemantic);
  assert.equal(input.localName, 'input');
  assert.equal(input.getAttribute('autocomplete'), 'username webauthn');
  assert.equal(input.value, 'ada');
  assert.equal(input.dataset.tiloField, '1', 'the pointer is the field\'s own');
  assert.equal(input.style.props.get('opacity'), '1');
  assert.equal(input.style.props.get('font-size'), '14px');
  // Its line starts at the toolkit's: 12 px in, and its baseline 20 px down.
  const [top, right, , left] = input.style.props.get('padding').split(' ');
  assert.equal(left, '12px');
  assert.equal(right, '20px', 'it stops where the field\'s text stops');
  assert.equal(Number.parseFloat(top) + 14 * 0.8, 20);
  // A password field is a password input, and the tree, which never holds a
  // secret, does not empty what the person typed.
  bridge.update({root: {path: 'app', paint_key: 'ff', rect: [10, 20, 200, 32],
    semantics: {role: 'TextInput', state: {focusable: true, protected: true, autofill: 'CurrentPassword'}}}});
  input.value = 'hunter2';
  bridge.update({root: {path: 'app', paint_key: 'ff', rect: [10, 20, 200, 32],
    semantics: {role: 'TextInput', state: {focusable: true, protected: true, autofill: 'CurrentPassword', disabled: false}}}});
  assert.equal(input.type, 'password');
  assert.equal(input.getAttribute('autocomplete'), 'current-password');
  assert.equal(input.value, 'hunter2');
  // A frame that no longer leaves the field to the page hides the input.
  frame(presenter, [['k', 'ff', 10, 20, 1], box(10, 20), ['ke']]);
  assert.equal(input.style.props.get('opacity'), '0');
  assert.equal(input.dataset.tiloField, undefined);
});

test('a reordered frame moves the fewest elements, so a selection in the rest stays', () => {
  const layer = new Node('div');
  const presenter = createDomPresenter(layer);
  const widget = key => [['k', key, 0, 0, 1], box(0, 0), ['ke']];
  frame(presenter, ['a', 'b', 'c', 'd'].flatMap(widget));
  const [a, b, c, d] = layer.children;
  const moved = [];
  const insertBefore = layer.insertBefore.bind(layer);
  layer.insertBefore = (child, before) => { moved.push(child); insertBefore(child, before); };
  // The bar `a` now draws after the panel `b`, and `e` is new.
  frame(presenter, ['b', 'c', 'a', 'e', 'd'].flatMap(widget));
  assert.deepEqual(layer.children.slice(0, 3), [b, c, a]);
  assert.equal(layer.children[4], d);
  assert.equal(moved.length, 2, 'only the bar and the new widget are placed');
  assert.ok(!moved.includes(b) && !moved.includes(c) && !moved.includes(d));
});
