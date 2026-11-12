import test from 'node:test';
import assert from 'node:assert/strict';
import {createViewPresenter} from './view-present.mjs';

// Just the DOM the presenter touches.
class Text {
  constructor(data) { this.data = data; this.parentNode = null; this.children = []; }
  get textContent() { return this.data; }
  remove() { if (this.parentNode) this.parentNode.children = this.parentNode.children.filter(c => c !== this); this.parentNode = null; }
  contains(node) { return node === this; }
}
class Node {
  constructor(tag) {
    this.localName = tag; this.children = []; this.parentNode = null; this.attributes = new Map();
    this.dataset = {}; this.listeners = new Map(); this.scrollTop = 0; this.className = ''; this.value = '';
    const props = new Map();
    this.style = {props, cssText: '', setProperty: (name, value) => props.set(name, value)};
    this.classList = {add: name => { this.className = `${this.className} ${name}`.trim(); }, contains: name => this.className.split(' ').includes(name)};
  }
  get firstChild() { return this.children[0] ?? null; }
  get firstElementChild() { return this.children.find(child => child instanceof Node) ?? null; }
  get nextSibling() { const siblings = this.parentNode?.children; return siblings?.[siblings.indexOf(this) + 1] ?? null; }
  get textContent() { return this.children.map(child => child.textContent).join(''); }
  set textContent(value) { this.replaceChildren(new Text(String(value))); }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  removeAttribute(name) { this.attributes.delete(name); }
  addEventListener(type, listener) { this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]); }
  dispatch(type, event = {}) { for (const listener of this.listeners.get(type) ?? []) listener({type, target: this, ...event}); }
  remove() { if (this.parentNode) this.parentNode.children = this.parentNode.children.filter(c => c !== this); this.parentNode = null; }
  insertBefore(child, before) {
    child.remove?.();
    const at = before ? this.children.indexOf(before) : -1;
    if (at < 0) this.children.push(child); else this.children.splice(at, 0, child);
    child.parentNode = this;
  }
  append(...children) { for (const child of children) this.insertBefore(typeof child === 'string' ? new Text(child) : child, null); }
  replaceChildren(...children) { for (const child of this.children) child.parentNode = null; this.children = []; this.append(...children); }
  contains(node) { return node === this || this.children.some(child => child.contains(node)); }
  getContext() { return {putImageData: (...args) => { this.drawn = args; }}; }
}
globalThis.document = {createElement: tag => new Node(tag), createTextNode: data => new Text(data), activeElement: null};
globalThis.ImageData = class { constructor(pixels, width, height) { Object.assign(this, {pixels, width, height}); } };

const all = node => [node, ...(node.children ?? []).filter(child => child instanceof Node).flatMap(all)];
const byPath = (layer, path) => all(layer).find(node => node.dataset.path === path);
const meta = JSON.stringify({background: '#fff'});
const show = (presenter, root, layers = []) => presenter.present(meta, JSON.stringify({viewport: [400, 300], root, layers}));
const widget = (path, rect, extra = {}) => ({path, widget: path.split('/').at(-1).replace(/\[.*/, ''), rect, ...extra});
const tick = () => new Promise(resolve => queueMicrotask(resolve));

test('a widget keeps its element by path, and an unchanged frame writes nothing', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  const frame = () => widget('Column[0]', [0, 0, 400, 300], {flows: true, layout: 'display:flex;flex-direction:column;gap:8px 0px;', children: [
    widget('Column[0]/Text[0]', [0, 0, 400, 20], {tag: 'h1', own: {kind: 'text', text: 'Groceries', size: 24, line_height: 1.2, weight: 700, italic: false, color: [0, 0, 0, 255], align: 'start', max_lines: null, mono: false}}),
    widget('Column[0]/Text[1]', [0, 28, 400, 20], {tag: 'p', own: {kind: 'text', text: 'Milk', size: 14, line_height: 1.5, weight: 400, italic: false, color: null, align: 'start', max_lines: null, mono: false}}),
  ]});
  show(presenter, frame());
  const heading = byPath(layer, 'Column[0]/Text[0]');
  assert.equal(heading.localName, 'h1');
  assert.equal(heading.textContent, 'Groceries');
  // A flowing container is a flex container with its node's style, and its
  // children are its flex items at the size the shared layout gave them.
  assert.match(byPath(layer, 'Column[0]').style.cssText, /display:flex;flex-direction:column;gap:8px 0px;/);
  assert.match(heading.style.cssText, /^position:relative;flex:none;width:400px;height:20px;/);
  const writes = presenter.stats.writes;
  show(presenter, frame());
  assert.equal(byPath(layer, 'Column[0]/Text[0]'), heading, 'the same element');
  assert.equal(presenter.stats.writes, writes, 'nothing written again');
});

test('a widget that places its children itself has them placed at their rectangles', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  show(presenter, widget('Shell[0]', [0, 0, 400, 300], {layout: 'display:flex;flex-direction:row;', children: [
    widget('Shell[0]/Button[0]', [360, 8, 32, 32], {tag: 'button', semantics: {role: 'Button', name: 'Search', state: {}}, own: {kind: 'painted', version: 1}}),
  ]}));
  const button = byPath(layer, 'Shell[0]/Button[0]');
  assert.equal(button.localName, 'button');
  assert.equal(button.getAttribute('aria-label'), 'Search');
  assert.match(button.style.cssText, /position:absolute;left:360px;top:8px;width:32px;height:32px;/);
  assert.doesNotMatch(byPath(layer, 'Shell[0]').style.cssText, /display:flex/);
});

test('a painted widget fetches its picture once the frame is handed over, and again only when it changed', async () => {
  const layer = new Node('div');
  const asked = [];
  let presenting = false;
  const presenter = createViewPresenter(layer, {picture: (path, scale) => {
    assert.equal(presenting, false, 'never while the runtime presents');
    asked.push(path);
    return [2, 2, new Uint8ClampedArray(16)];
  }});
  const frame = version => widget('Icon[0]', [0, 0, 16, 16], {own: {kind: 'painted', version}});
  presenting = true; show(presenter, frame(1)); presenting = false;
  await tick();
  assert.deepEqual(asked, ['Icon[0]']);
  const canvas = byPath(layer, 'Icon[0]#picture');
  assert.equal(canvas.width, 2);
  show(presenter, frame(1));
  await tick();
  assert.equal(asked.length, 1, 'an unchanged picture is not fetched again');
  show(presenter, frame(2));
  await tick();
  assert.equal(asked.length, 2);
});

test('a field is an input whose typing goes to the widget as its value', () => {
  const layer = new Node('div');
  const sent = [];
  const presenter = createViewPresenter(layer, {semantic: (...args) => sent.push(args)});
  const look = {text: 'Groceries', size: 26, line_height: 1.2, weight: 700, italic: false, color: null, align: 'start', max_lines: null, mono: false};
  show(presenter, widget('EditableLabel[0]', [0, 0, 300, 40], {tag: 'input', semantics: {role: 'TextInput', name: 'Page title', state: {}}, own: {kind: 'field', value: 'Groceries', multiline: false, placeholder: 'Untitled', look}}));
  const input = byPath(layer, 'EditableLabel[0]');
  assert.equal(input.localName, 'input');
  assert.equal(input.value, 'Groceries');
  assert.equal(input.getAttribute('placeholder'), 'Untitled');
  input.value = 'Groceries!';
  input.dispatch('input');
  assert.deepEqual(sent, [['EditableLabel[0]', undefined, 'value', 'Groceries!', 0, 0]]);
});

test("a document's blocks are paragraphs, headings and list items with their marks and caret", () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  const block = (id, kind, text, extra = {}) => ({id, kind, level: 0, done: false, ordinal: null, tag: null, soft_break: false, text, marks: [], selection: null, editable: true, ...extra});
  show(presenter, widget('BlockEditor[0]', [0, 0, 400, 200], {own: {kind: 'blocks', blocks: [
    block('h', 'heading', 'Trip', {level: 1}),
    block('p', 'paragraph', 'Pack light, é', {marks: [{start: 0, end: 4, mark: 'bold'}], selection: [15, 15]}),
    block('b', 'bullet', 'Passport'),
  ]}}));
  const items = byPath(layer, 'BlockEditor[0]#blocks').children;
  assert.deepEqual(items.map(item => item.localName), ['h1', 'p', 'li']);
  assert.equal(items[1].textContent, 'Pack light, é');
  assert.equal(items[1].children[0].localName, 'strong', 'the bold run');
  // The caret stands after the last character: byte 15 is the end of the
  // two-byte é.
  assert.equal(items[1].children.at(-1).className, 'tilo-caret');
  assert.equal(items[2].className, 'tilo-block tilo-bullet');
});

test('a scroller is native: the page scrolls it to the widget\'s offset, and the person\'s scroll goes back once', () => {
  const layer = new Node('div');
  const scrolled = [];
  const presenter = createViewPresenter(layer, {onScroll: (...args) => scrolled.push(args)});
  const frame = offset => widget('List[0]', [0, 0, 200, 100], {scroll: [offset, 1000, 100], children: [
    widget('List[0]/Row[0]', [0, -offset, 200, 30], {own: {kind: 'painted', version: 1}}),
  ]});
  show(presenter, frame(40));
  const list = byPath(layer, 'List[0]');
  assert.match(list.style.cssText, /overflow-y:auto/);
  assert.equal(list.scrollTop, 40);
  list.dispatch('scroll');
  assert.deepEqual(scrolled, [], 'the page\'s own scroll does not echo back');
  list.scrollTop = 90;
  list.dispatch('scroll');
  assert.deepEqual(scrolled, [['List[0]', 90]]);
  // The row stands in the content where it is, not where it shows now.
  assert.match(byPath(layer, 'List[0]/Row[0]').style.cssText, /top:0px;/);
});
