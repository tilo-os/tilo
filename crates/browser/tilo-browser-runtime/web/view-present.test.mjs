import test from 'node:test';
import assert from 'node:assert/strict';
import {createViewPresenter, VIEW_CSS} from './view-present.mjs';

// The page's host puts this sheet in the document before the first frame: a module that stops
// exporting it fails to load, and the whole page with it.
test('the presenter hands the page its stylesheet', () => {
  assert.equal(typeof VIEW_CSS, 'string');
  assert.match(VIEW_CSS, /\.tilo-view/);
});

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
  get parentElement() { return this.parentNode instanceof Node ? this.parentNode : null; }
  querySelector(selector) { return all(this).find(node => node !== this && matches(node, selector)) ?? null; }
  closest(selector) { for (let node = this; node; node = node.parentNode) if (matches(node, selector)) return node; return null; }
}

// The selectors the presenter's press handlers look up.
function matches(node, selector) {
  if (!(node instanceof Node)) return false;
  return selector.split(',').some(part => {
    const name = part.trim();
    if (name === '[data-path]') return node.dataset.path !== undefined;
    if (name === '[data-tilo-act]') return node.dataset.tiloAct !== undefined;
    if (name === '[data-tilo-field]') return node.dataset.tiloField !== undefined;
    return false;
  });
}
globalThis.document = {createElement: tag => new Node(tag), createElementNS: (_, tag) => new Node(tag), createTextNode: data => new Text(data), activeElement: null};
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

test('a ground fills its element under the words, and an image carries its source and alt', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  show(presenter, widget('Card[0]', [0, 0, 200, 120], {tag: 'article', ground: {fill: [250, 250, 252, 255], edge: [0, 0, 0, 20], radius: 8}, children: [
    widget('Card[0]/Image[0]', [8, 8, 40, 40], {tag: 'img', attrs: [['src', 'https://example.test/a.png'], ['alt', 'Ada']]}),
  ]}));
  const card = byPath(layer, 'Card[0]');
  assert.match(card.style.cssText, /background:rgba\(250,250,252,1\.000\)/);
  assert.match(card.style.cssText, /box-shadow:inset 0 0 0 1px rgba\(0,0,0,0\.078\)/);
  assert.match(card.style.cssText, /border-radius:8px/);
  const image = byPath(layer, 'Card[0]/Image[0]');
  assert.equal(image.localName, 'img');
  assert.equal(image.getAttribute('src'), 'https://example.test/a.png');
  assert.equal(image.getAttribute('alt'), 'Ada');
  // A source it no longer shows comes off the element.
  show(presenter, widget('Card[0]', [0, 0, 200, 120], {tag: 'article', children: [
    widget('Card[0]/Image[0]', [8, 8, 40, 40], {tag: 'img', attrs: [['alt', 'Ada']]}),
  ]}));
  assert.equal(byPath(layer, 'Card[0]/Image[0]').getAttribute('src'), null);
});

test('a notice is a live status region, read out as it arrives', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  show(presenter, widget('Banner[0]', [0, 0, 300, 40], {tag: 'aside', semantics: {role: 'Group', name: 'Could not save', live: 'polite', state: {}}, children: [
    widget('Banner[0]/Text[0]', [8, 8, 200, 20], {tag: 'p', own: {kind: 'text', text: 'Try again', size: 13, line_height: 1.4, weight: 400, italic: false, color: null, align: 'start', max_lines: null, mono: false}}),
  ]}));
  const notice = byPath(layer, 'Banner[0]');
  assert.equal(notice.getAttribute('role'), 'status');
  assert.equal(notice.getAttribute('aria-live'), 'polite');
  assert.equal(byPath(layer, 'Banner[0]/Text[0]').textContent, 'Try again');
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
  const block = (id, kind, text, extra = {}) => ({id, kind, level: 0, done: false, ordinal: null, tag: null, soft_break: false, text, marks: [], selection: null, editable: true, gap: 6, size: 15, line_height: 1.4, weight: 400, ...extra});
  show(presenter, widget('BlockEditor[0]', [0, 0, 400, 200], {own: {kind: 'blocks', blocks: [
    block('h', 'heading', 'Trip', {level: 1, gap: 0, size: 28, line_height: 1.2, weight: 700}),
    block('p', 'paragraph', 'Pack light, é', {marks: [{start: 0, end: 4, mark: 'bold'}], selection: [15, 15]}),
    block('b', 'bullet', 'Passport'),
  ], edges: {text: 22, list: 28, indent: 28, quote: 16}}}));
  const body = byPath(layer, 'BlockEditor[0]#blocks');
  // A paragraph's text starts at the editor's text edge, where the title
  // above it starts.
  assert.match(body.style.cssText, /padding-left:22px;/);
  const items = body.children;
  assert.deepEqual(items.map(item => item.localName), ['h1', 'p', 'li']);
  assert.equal(items[1].textContent, 'Pack light, é');
  assert.equal(items[1].children[0].localName, 'strong', 'the bold run');
  // The caret stands after the last character: byte 15 is the end of the
  // two-byte é.
  assert.equal(items[1].children.at(-1).className, 'tilo-caret');
  assert.equal(items[2].className, 'tilo-block tilo-bullet');
  // Each block is set as the editor sets it, not in sizes of the page's own.
  assert.match(items[0].style.cssText, /margin-top:0px;font-size:28px;line-height:1.2;font-weight:700;/);
  assert.match(items[1].style.cssText, /margin-top:6px;font-size:15px;line-height:1.4;font-weight:400;/);
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

test("a button's face is its icon and words on its ground, not a picture", () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  const look = {text: 'Share', size: 13, line_height: 1.3, weight: 500, italic: false, color: [20, 20, 20, 255], align: 'start', max_lines: 1, mono: false};
  const icon = {name: 'plus', paths: ['M5 12h14', 'M12 5v14'], size: 16, stroke: 1.5, color: [20, 20, 20, 255], filled: false};
  show(presenter, widget('Button[0]', [0, 0, 96, 32], {tag: 'button', semantics: {role: 'Button', name: 'Share'},
    own: {kind: 'face', icon, label: look, fill: [230, 230, 235, 255], radius: 8, pad: 12, gap: 6, spread: false}}));
  const button = byPath(layer, 'Button[0]');
  assert.equal(button.localName, 'button');
  assert.match(button.style.cssText, /background:rgba\(230,230,235,1.000\)/);
  assert.equal(button.getAttribute('aria-label'), null, 'its words name it');
  const svg = byPath(layer, 'Button[0]#icon').children[0];
  assert.equal(svg.localName, 'svg');
  assert.deepEqual(svg.children.map(path => path.getAttribute('d')), icon.paths);
  assert.equal(byPath(layer, 'Button[0]#label').textContent, 'Share');
  assert.equal(all(layer).some(node => node.localName === 'canvas'), false, 'no picture of its paint');
});

test('a separator says how far it stands, and which way it divides', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  show(presenter, widget('Split[0]', [0, 0, 400, 300], {children: [
    widget('Split[0]#divider', [198, 0, 4, 300], {tag: 'div', focusable: true,
      semantics: {role: 'Separator', name: 'Resize preview', value: '55%', state: {}}}),
  ]}));
  const divider = byPath(layer, 'Split[0]#divider');
  assert.equal(divider.getAttribute('role'), 'separator');
  assert.equal(divider.getAttribute('aria-valuenow'), '55');
  assert.equal(divider.getAttribute('aria-valuetext'), '55%');
  assert.equal(divider.getAttribute('aria-orientation'), 'vertical');
  assert.equal(divider.getAttribute('aria-label'), 'Resize preview');
});

test('a box is a label around its own input, and pressing it toggles the widget', () => {
  const layer = new Node('div');
  const sent = [];
  const presenter = createViewPresenter(layer, {semantic: (...args) => sent.push(args)});
  const look = {text: 'Show hidden files', size: 14, line_height: 1.4, weight: 400, italic: false, color: null, align: 'start', max_lines: null, mono: false};
  show(presenter, widget('Checkbox[0]', [0, 0, 220, 28], {tag: 'label', semantics: {role: 'Checkbox', name: 'Show hidden files', state: {}},
    own: {kind: 'control', controls: [{kind: 'check', style: 'checkbox', checked: false, mixed: false, label: look}]}}));
  const label = byPath(layer, 'Checkbox[0]');
  assert.equal(label.localName, 'label');
  const input = byPath(layer, 'Checkbox[0]#control:0:input');
  assert.equal(input.localName, 'input');
  assert.equal(input.getAttribute('type'), 'checkbox');
  assert.equal(input.checked, false);
  assert.equal(byPath(layer, 'Checkbox[0]#control:0:label').textContent, 'Show hidden files');
  layer.dispatch('click', {target: input});
  assert.deepEqual(sent.at(-1), ['Checkbox[0]', undefined, 'toggle', '', 0, 0]);
});

test('a range is an input holding the value the widget gives it, and dragging it sets the value', () => {
  const layer = new Node('div');
  const sent = [];
  const presenter = createViewPresenter(layer, {semantic: (...args) => sent.push(args)});
  show(presenter, widget('Slider[0]', [0, 0, 200, 20], {semantics: {role: 'Slider', name: 'Volume', state: {}},
    own: {kind: 'control', controls: [{kind: 'range', value: 0.25, min: 0, max: 2, step: 0.1, text: '25 percent'}]}}));
  const input = byPath(layer, 'Slider[0]#control:0:input');
  assert.equal(input.getAttribute('type'), 'range');
  assert.equal(input.value, '0.25');
  assert.equal(input.getAttribute('aria-valuetext'), '25 percent');
  input.value = '1.5';
  layer.dispatch('input', {target: input});
  assert.deepEqual(sent.at(-1), ['Slider[0]', undefined, 'value', '1.5', 0, 0]);
});

test("a chooser shows its value and opens its options, each choosing a value", () => {
  const layer = new Node('div');
  const sent = [];
  const presenter = createViewPresenter(layer, {semantic: (...args) => sent.push(args)});
  const look = {text: 'Large', size: 14, line_height: 1.4, weight: 400, italic: false, color: null, align: 'start', max_lines: 1, mono: false};
  show(presenter, widget('Dropdown[0]', [0, 0, 160, 32], {semantics: {role: 'ComboBox', name: 'Size', state: {expanded: true}},
    own: {kind: 'control', controls: [{kind: 'select', current: look, placeholder: 'Choose', open: true, options: [
      {label: 'Small', value: 'sm', selected: false},
      {label: 'Large', value: 'lg', selected: true},
    ]}]}}));
  const button = byPath(layer, 'Dropdown[0]#control:0:current');
  assert.equal(button.localName, 'button');
  assert.equal(button.getAttribute('aria-haspopup'), 'listbox');
  assert.equal(button.getAttribute('aria-expanded'), 'true');
  assert.equal(byPath(layer, 'Dropdown[0]#control:0:value').textContent, 'Large');
  const option = byPath(layer, 'Dropdown[0]#control:0:option:1');
  assert.equal(option.getAttribute('role'), 'option');
  assert.equal(option.getAttribute('aria-selected'), 'true');
  layer.dispatch('click', {target: option});
  assert.deepEqual(sent.at(-1), ['Dropdown[0]', undefined, 'value', 'lg', 0, 0]);
});

test('a link is an anchor whose press opens the address through the widget', () => {
  const layer = new Node('div');
  const sent = [];
  const presenter = createViewPresenter(layer, {semantic: (...args) => sent.push(args)});
  const look = {text: 'Tilo', size: 14, line_height: 1.4, weight: 400, italic: false, color: null, align: 'start', max_lines: null, mono: false};
  show(presenter, widget('Link[0]', [0, 0, 60, 20], {tag: 'a', semantics: {role: 'Link', name: 'Tilo', state: {}},
    own: {kind: 'control', controls: [{kind: 'link', href: 'https://tilo.coffee', label: look}]}}));
  const anchor = byPath(layer, 'Link[0]');
  assert.equal(anchor.localName, 'a');
  assert.equal(anchor.getAttribute('href'), 'https://tilo.coffee');
  let prevented = false;
  layer.dispatch('click', {target: anchor, preventDefault: () => { prevented = true; }});
  assert.ok(prevented, 'the page never leaves');
  assert.deepEqual(sent.at(-1), ['Link[0]', undefined, 'activate', '', 0, 0]);
});

test('a secret entry shows its mask and never holds its characters in the page', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  show(presenter, widget('SecretPrompt[0]', [0, 0, 300, 40], {tag: 'dialog', semantics: {role: 'Dialog', name: 'Sign in', state: {}},
    own: {kind: 'control', controls: [{kind: 'entry', input: 'search', text: '••••••', placeholder: 'Password', steppers: false, secret: true}]}}));
  const input = byPath(layer, 'SecretPrompt[0]#control:0:input');
  assert.equal(input.getAttribute('type'), 'password');
  assert.equal(input.value, '••••••');
  assert.equal(input.dataset.tiloAct, undefined, 'the page never types the secret');
});

test('words the layout set on one line never wrap, and cut-short words get a little slack', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  const look = (text, extra = {}) => ({kind: 'text', text, size: 20, line_height: 1.25, weight: 600, italic: false, color: null, align: 'start', max_lines: null, mono: false, ...extra});
  show(presenter, widget('Column[0]', [0, 0, 400, 300], {children: [
    widget('Column[0]/Heading[0]', [0, 0, 111.5, 25], {tag: 'h2', own: look('Collections')}),
    widget('Column[0]/Text[0]', [0, 30, 120, 75], {tag: 'p', own: look('A paragraph the layout set over three lines')}),
    widget('Column[0]/Text[1]', [0, 110, 80, 25], {own: look('Choices and more', {max_lines: 1})}),
  ]}));
  const heading = byPath(layer, 'Column[0]/Heading[0]').style.cssText;
  assert.match(heading, /white-space:pre;/);
  assert.doesNotMatch(heading, /overflow-wrap|pre-wrap/);
  // Several lines wrap where the page's words run out, never inside a word that fits a line.
  const paragraph = byPath(layer, 'Column[0]/Text[0]').style.cssText;
  assert.match(paragraph, /white-space:pre-wrap;overflow-wrap:break-word;/);
  assert.doesNotMatch(paragraph, /anywhere/);
  const cut = byPath(layer, 'Column[0]/Text[1]').style.cssText;
  assert.match(cut, /text-overflow:ellipsis;width:max-content;min-width:80px;max-width:84px;/);
});

test('a box, a switch or a radio the widget draws itself says whether it is on', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  const frame = (checked, mixed = false) => widget('Row[0]', [0, 0, 400, 300], {children: [
    widget('Row[0]/Target[0]', [0, 0, 20, 20], {tag: 'button', semantics: {role: 'Checkbox', name: 'Done', state: {checked, mixed}}}),
    widget('Row[0]/Target[1]', [30, 0, 40, 20], {tag: 'button', semantics: {role: 'Switch', name: 'Wi-Fi', state: {checked: !checked}}}),
    widget('Row[0]/Target[2]', [80, 0, 40, 20], {tag: 'button', semantics: {role: 'Button', name: 'Save', state: {checked: null}}}),
  ]});
  show(presenter, frame(true));
  assert.equal(byPath(layer, 'Row[0]/Target[0]').getAttribute('aria-checked'), 'true');
  assert.equal(byPath(layer, 'Row[0]/Target[0]').getAttribute('role'), 'checkbox');
  assert.equal(byPath(layer, 'Row[0]/Target[1]').getAttribute('aria-checked'), 'false');
  assert.equal(byPath(layer, 'Row[0]/Target[2]').getAttribute('aria-checked'), null);
  show(presenter, frame(false, true));
  assert.equal(byPath(layer, 'Row[0]/Target[0]').getAttribute('aria-checked'), 'mixed');
});

test('a flex item keeps the margin its layout node gives it, and a slot centres its one line', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  const look = {kind: 'text', text: '1204', size: 12, line_height: 1.35, weight: 400, italic: false, color: null, align: 'end', max_lines: 1, mono: false};
  show(presenter, widget('Section[0]', [0, 0, 400, 300], {flows: true, layout: 'display:flex;flex-direction:column;', children: [
    widget('Section[0]/Text[0]', [0, 0, 400, 20], {layout: 'display:flex;margin:12px 0px 0px 0px;', own: {...look, text: 'Files'}}),
    widget('Section[0]/Table[0]', [0, 32, 400, 28], {children: [widget('Section[0]/Table[0]#td:0', [8, 32, 60, 28], {tag: 'td', own: look})]}),
  ]}));
  assert.match(byPath(layer, 'Section[0]/Text[0]').style.cssText, /margin:12px 0px 0px 0px;/);
  // A cell's line stands in the middle of its row, as the table paints it.
  const cell = byPath(layer, 'Section[0]/Table[0]#td:0').style.cssText;
  assert.match(cell, /line-height:28px;/);
  assert.match(cell, /text-align:end;/);
});

test('a window floats over the shell on its ground, edge and shadow; a ground alone casts none', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  const ground = {fill: [255, 255, 255, 255], edge: [200, 200, 200, 255], radius: 14};
  show(presenter, widget('Map[0]', [0, 0, 800, 600], {children: [
    widget('Map[0]/WindowFrame[0]', [40, 40, 300, 200], {ground, floats: true}),
    widget('Map[0]/Card[0]', [400, 40, 200, 100], {ground}),
  ]}));
  const window = byPath(layer, 'Map[0]/WindowFrame[0]').style.cssText;
  assert.match(window, /box-shadow:0 1px 3px rgba\(0,0,0,.07\),0 8px 22px rgba\(0,0,0,.09\),inset 0 0 0 1px rgba\(200,200,200/);
  assert.match(window, /border-radius:14px;/);
  const card = byPath(layer, 'Map[0]/Card[0]').style.cssText;
  assert.match(card, /box-shadow:inset 0 0 0 1px/);
  assert.doesNotMatch(card, /28px/);
});

test('a toolbar its widget lays out keeps its tools where they stand, a separator between and a dot on the new', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  const glyph = {name: 'pencil', paths: ['M0 0'], size: 16, stroke: 1.5, color: [0, 0, 0, 255], filled: false};
  show(presenter, widget('Toolbar[0]', [0, 0, 400, 32], {own: {kind: 'control', controls: [{kind: 'choices', list: 'toolbar', item: 'button', options: [
    {label: 'Edit', icon: glyph, at: 0},
    {label: 'Flag', icon: glyph, at: 34, dot: true},
    {label: 'Pin', icon: glyph, at: 80, rule: 72},
    {label: 'Share', icon: glyph, at: 368},
  ]}]}}));
  const share = byPath(layer, 'Toolbar[0]#control:0:choice:3');
  assert.match(share.style.cssText, /position:absolute;.*left:368px;/);
  assert.match(byPath(layer, 'Toolbar[0]#control:0:choice:2:rule').style.cssText, /left:71.5px;/);
  assert.ok(byPath(layer, 'Toolbar[0]#control:0:choice:1:dot'), 'the new one carries its dot');
});

test('a swatch is its colour, named for a reader and a tooltip, the chosen one ringed', () => {
  const layer = new Node('div');
  const presenter = createViewPresenter(layer);
  show(presenter, widget('ColorSwatchPicker[0]', [0, 0, 160, 32], {own: {kind: 'control', controls: [{kind: 'choices', list: 'radiogroup', item: 'radio', options: [
    {label: 'Blue', colour: [58, 123, 213, 255], selected: true},
    {label: 'Red', colour: [213, 90, 58, 255]},
  ]}]}}));
  const blue = byPath(layer, 'ColorSwatchPicker[0]#control:0:choice:0');
  const red = byPath(layer, 'ColorSwatchPicker[0]#control:0:choice:1');
  assert.equal(blue.getAttribute('aria-label'), 'Blue');
  assert.equal(red.getAttribute('title'), 'Red');
  assert.equal(byPath(layer, 'ColorSwatchPicker[0]#control:0:choice:0:label'), undefined, 'no code beside the colour');
  assert.match(blue.style.cssText, /box-shadow:inset 0 0 0 2px/);
  assert.doesNotMatch(red.style.cssText, /box-shadow/);
});
