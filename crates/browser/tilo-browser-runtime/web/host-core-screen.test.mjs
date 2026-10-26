import test from 'node:test';
import assert from 'node:assert/strict';
import {createScreenFill} from './host-core.mjs';

/** A surface and its canvas, as a browser keeps them: the edges written as
 * style properties, and the box the page laid them out in. */
function nodes() {
  const make = () => {
    return {
      style: {boxSizing: ''},
      getBoundingClientRect: () => ({top: 0, left: 0, right: 400, bottom: 300, width: 400, height: 300}),
    };
  };
  return {surface: make(), canvas: make()};
}

/** The observer the page would start, told the surface's box on request. */
function observer() {
  const started = [];
  class Fake {
    constructor(callback) { this.callback = callback; started.push(this); }
    observe(element) { this.element = element; }
    /** What a soft keyboard opened under the surface leaves: a kept band. */
    shrink(top, bottom) {
      this.callback([{contentRect: {top, left: 0, right: 400, bottom: 300 - bottom}}]);
    }
  }
  return {observe: Fake, started};
}

const edges = node => ['top', 'right', 'bottom', 'left'].map(edge => node.style[edge]);

test('the desktop fills the screen from its edges, a notch and all, while no field has focus', () => {
  const {surface, canvas} = nodes();
  const fill = createScreenFill(surface, {canvas, observe: observer().observe});
  assert.deepEqual(edges(surface), ['env(safe-area-inset-top)', 'env(safe-area-inset-right)',
    'env(safe-area-inset-bottom)', 'env(safe-area-inset-left)']);
  assert.deepEqual(edges(canvas), edges(surface));
  assert.equal(surface.style.boxSizing, 'border-box', 'the surface fills the screen without overhanging it');
});

test('a field focused under a soft keyboard fills what is left, then the screen again from its edges', () => {
  const {surface, canvas} = nodes();
  const fake = observer();
  const fill = createScreenFill(surface, {canvas, observe: fake.observe});
  fill(false);
  assert.deepEqual(edges(surface), ['0px', '0px', '0px', '0px']);
  assert.equal(surface.style.boxSizing, '', 'the browser sizes the shrunk surface itself');
  // The keyboard is up: the page lost the screen's lower half, which is the
  // soft keyboard and not an edge of the screen.
  fake.started[0].shrink(0, 150);
  // The keys go: the desktop grows back under the keyboard before it animated
  // away, so it fills the screen from the screen's own edges.
  fill(true);
  assert.deepEqual(edges(surface), ['env(safe-area-inset-top)', 'env(safe-area-inset-right)',
    'env(safe-area-inset-bottom)', 'env(safe-area-inset-left)']);
  assert.equal(surface.style.boxSizing, 'border-box');
});

test('a band the browser kept is measured on the surface’s own edges, top, bottom and both sides', () => {
  const {surface, canvas} = nodes();
  const fake = observer();
  const fill = createScreenFill(surface, {canvas, observe: fake.observe});
  fill(false);
  fake.started[0].callback([{contentRect: {top: 20, left: 8, right: 392, bottom: 260}}]);
  fill(true);
  // Still the screen's edges, and no keyboard shrank the surface this time.
  assert.deepEqual(edges(canvas), ['env(safe-area-inset-top)', 'env(safe-area-inset-right)',
    'env(safe-area-inset-bottom)', 'env(safe-area-inset-left)']);
  // A field focused again goes back to the page's own box.
  fill(false);
  assert.deepEqual(edges(canvas), ['20px', '8px', '40px', '8px'], 'the surface’s own box, edge pad by edge pad');
});

test('filling the screen again is only done where it changes, and a page without an observer still fills it', () => {
  const {surface, canvas} = nodes();
  const fill = createScreenFill(surface, {canvas});
  const before = edges(surface);
  fill(true);
  assert.deepEqual(edges(surface), before);
  fill(false);
  assert.deepEqual(edges(surface), ['0px', '0px', '0px', '0px']);
  fill(true);
  assert.deepEqual(edges(surface), before);
});
