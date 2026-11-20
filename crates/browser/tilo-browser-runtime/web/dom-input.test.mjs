import test from 'node:test';
import assert from 'node:assert/strict';
import {createDomInput} from './dom-input.mjs';

// Just what the router touches: listeners in the capture phase, containment
// and dispatch.
class Target {
  constructor(parent, box = [0, 0, 1000, 1000]) {
    this.parent = parent;
    this.listeners = new Map();
    this.sent = [];
    this.style = {};
    this.box = box;
  }
  getBoundingClientRect() {
    const [left, top, width, height] = this.box;
    return {left, top, width, height, right: left + width, bottom: top + height};
  }
  addEventListener(type, listener, options = {}) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
    options.signal?.addEventListener?.('abort', () => this.listeners.delete(type));
  }
  contains(node) { for (let at = node; at; at = at.parent) if (at === this) return true; return false; }
  dispatchEvent(event) { this.sent.push(event); return true; }
  fire(type, init) {
    const event = {type, target: this, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; }, ...init};
    for (const listener of this.listeners.get(type) ?? []) listener(event);
    return event;
  }
}
class Synthetic { constructor(type, init) { Object.assign(this, init, {type}); } }
const view = {PointerEvent: Synthetic, MouseEvent: Synthetic, WheelEvent: Synthetic, getSelection: () => selection, addEventListener() {}};
let selection = null;

function setup({scroller = false} = {}) {
  const layer = new Target(null);
  const canvas = new Target(null);
  const text = new Target(layer);
  const container = {scrollTop: 0};
  const scrollerAt = (x, y, dy) => scroller && (dy === 0 || dy > 0) && container;
  const input = createDomInput({layer, canvas, scrollerAt, view});
  return {layer, canvas, text, input, container};
}

test('a pointer event on the layer goes on to the canvas, without bubbling', () => {
  const {layer, canvas} = setup();
  layer.fire('pointerdown', {pointerType: 'mouse', pointerId: 1, clientX: 10, clientY: 20, button: 0, buttons: 1});
  assert.equal(canvas.sent.length, 1);
  assert.equal(canvas.sent[0].type, 'pointerdown');
  assert.equal(canvas.sent[0].bubbles, false);
  assert.equal(canvas.sent[0].clientX, 10);
  assert.equal(canvas.sent[0].pointerId, 1);
});

test('a context menu asked for over the layer is the runtime\'s: the browser\'s own stays away', () => {
  const {layer, canvas} = setup();
  // The runtime's listener on the canvas takes every request, as winit's does.
  canvas.dispatchEvent = event => { canvas.sent.push(event); event.defaultPrevented = true; return false; };
  const press = layer.fire('pointerdown', {pointerType: 'mouse', pointerId: 1, clientX: 10, clientY: 20, button: 2, buttons: 2});
  const asked = layer.fire('contextmenu', {pointerType: 'mouse', pointerId: 1, clientX: 10, clientY: 20, button: 2, buttons: 2});
  assert.deepEqual(canvas.sent.map(event => [event.type, event.button, event.clientX, event.clientY]),
    [['pointerdown', 2, 10, 20], ['contextmenu', 2, 10, 20]]);
  assert.equal(asked.defaultPrevented, true, 'the browser shows no menu of its own over the desktop');
  assert.equal(press.defaultPrevented, false, 'only the menu request is answered for the browser');
});

test('moving between the layer\'s own elements neither enters nor leaves the surface', () => {
  const {layer, canvas, text} = setup();
  layer.fire('pointerout', {pointerType: 'mouse', relatedTarget: text});
  layer.fire('pointerover', {pointerType: 'mouse', relatedTarget: text});
  assert.equal(canvas.sent.length, 0);
  // The pointer left the surface: past the layer's edge, so the runtime hears
  // it leave.
  layer.fire('pointerout', {pointerType: 'mouse', relatedTarget: null, clientX: 1200, clientY: 20});
  assert.deepEqual(canvas.sent.map(event => event.type), ['pointerout']);
});

// A frame replaces the element under a still pointer: the browser fires a
// pointerout for the removed node, with no related target and the pointer
// nowhere near the edge. Forwarded, the runtime would take it as the pointer
// leaving the surface and end the press in hand, so a control that acts when
// a press ends — a slot in the window's bar, a button that raises a dialog —
// would never act.
test('a frame that replaces the element under a still pointer is no leave, nor an entry', () => {
  const {layer, canvas} = setup();
  layer.fire('pointerover', {pointerType: 'mouse', relatedTarget: null, clientX: 140, clientY: 20});
  assert.deepEqual(canvas.sent.map(event => event.type), ['pointerover'], 'the pointer enters the surface');
  layer.fire('pointerdown', {pointerType: 'mouse', pointerId: 1, clientX: 140, clientY: 20, button: 0, buttons: 1});
  // The press's frame replaced the element under it.
  layer.fire('pointerout', {pointerType: 'mouse', relatedTarget: null, clientX: 140, clientY: 20});
  layer.fire('pointerover', {pointerType: 'mouse', relatedTarget: null, clientX: 140, clientY: 20});
  layer.fire('pointerup', {pointerType: 'mouse', pointerId: 1, clientX: 140, clientY: 20, button: 0, buttons: 0});
  assert.deepEqual(canvas.sent.map(event => event.type), ['pointerover', 'pointerdown', 'pointerup'],
    'the press ends where it began, so it is a click');
  // Leaving past the layer's edge and coming back is a leave and an entry.
  layer.fire('pointerout', {pointerType: 'mouse', relatedTarget: null, clientX: 1200, clientY: 20});
  layer.fire('pointerover', {pointerType: 'mouse', relatedTarget: null, clientX: 140, clientY: 20});
  assert.deepEqual(canvas.sent.map(event => event.type).slice(3), ['pointerout', 'pointerover']);
});

test('a touch on a scroll container is the browser\'s pan: the runtime hears it begin and end', () => {
  const {layer, canvas} = setup({scroller: true});
  for (const type of ['pointerdown', 'pointermove', 'pointermove', 'pointercancel']) layer.fire(type, {pointerType: 'touch', pointerId: 7});
  assert.deepEqual(canvas.sent.map(event => event.type), ['pointerdown', 'pointercancel']);
  // A touch elsewhere is the runtime's, moves and all.
  const other = setup();
  for (const type of ['pointerdown', 'pointermove', 'pointerup']) other.layer.fire(type, {pointerType: 'touch', pointerId: 8});
  assert.deepEqual(other.canvas.sent.map(event => event.type), ['pointerdown', 'pointermove', 'pointerup']);
});

test('a wheel a scroll container can take moves it at once; any other goes to the runtime', () => {
  const {layer, canvas, container} = setup({scroller: true});
  const native = layer.fire('wheel', {deltaY: 40, deltaMode: 0});
  assert.equal(native.defaultPrevented, true, 'moved here, not by the browser later');
  assert.equal(container.scrollTop, 40);
  assert.equal(canvas.sent.length, 0);
  const up = layer.fire('wheel', {deltaY: -40, deltaMode: 0});
  assert.equal(up.defaultPrevented, true);
  assert.deepEqual(canvas.sent.map(event => [event.type, event.deltaY]), [['wheel', -40]]);
});

test('a selection of the layer\'s text is held; one elsewhere or collapsed is not', () => {
  const {layer, text, input} = setup();
  selection = {isCollapsed: false, rangeCount: 1, anchorNode: text};
  assert.equal(input.holdsSelection(), true);
  selection = {isCollapsed: true, rangeCount: 1, anchorNode: text};
  assert.equal(input.holdsSelection(), false);
  selection = {isCollapsed: false, rangeCount: 1, anchorNode: new Target(null)};
  assert.equal(input.holdsSelection(), false);
  void layer;
});

test('a press the runtime takes moves no focus and starts no selection', () => {
  const {layer} = setup();
  assert.equal(layer.fire('mousedown', {button: 0}).defaultPrevented, true);
  let native = true;
  const own = new Target(null), canvas = new Target(null);
  createDomInput({layer: own, canvas, scrollerAt: () => false, native: () => native, view});
  assert.equal(own.fire('mousedown', {button: 0}).defaultPrevented, false, 'selecting natively');
  native = false;
  assert.equal(own.fire('mousedown', {button: 0}).defaultPrevented, true);
});

test('a press on a field the page shows takes the focus to it; one on its other controls does not', () => {
  const {layer, canvas} = setup();
  const element = (localName, init = {}) => {
    const own = {localName, dataset: {}, ...init};
    own.closest = () => own;
    return own;
  };
  const pressed = target => layer.fire('mousedown', {button: 0, target}).defaultPrevented;
  assert.equal(pressed(element('textarea', {dataset: {tiloField: 'true'}})), false, 'a text field');
  assert.equal(pressed(element('input', {type: 'text', dataset: {tiloAct: 'value'}})), false, 'an entry');
  assert.equal(pressed(element('input', {type: 'checkbox', dataset: {tiloAct: 'toggle'}})), true, 'a box');
  assert.equal(pressed(element('button', {dataset: {tiloAct: 'activate'}})), true, 'a button');
  // Its pointer events stay the page's: the runtime never sees the press.
  layer.fire('pointerdown', {pointerType: 'mouse', pointerId: 1, button: 0, target: element('textarea', {dataset: {tiloField: 'true'}})});
  assert.equal(canvas.sent.length, 0);
});

test('a native selection is held from the press that starts it', () => {
  const layer = new Target(null), canvas = new Target(null);
  const page = new Target(null);
  const local = {...view, addEventListener: (...args) => page.addEventListener(...args), getSelection: () => null};
  const input = createDomInput({layer, canvas, scrollerAt: () => false, native: () => true, view: local});
  layer.fire('mousedown', {button: 0});
  assert.equal(input.holdsSelection(), true);
  page.fire('mouseup', {button: 0});
  assert.equal(input.holdsSelection(), false);
});
