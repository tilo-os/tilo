import test from 'node:test';
import assert from 'node:assert/strict';

import {placeNear, progressText} from './drop-feedback.mjs';

test('what a drop shows stands whole inside the page, above the place it fell where there is room', () => {
  const page = {width: 1280, height: 800};
  assert.deepEqual(placeNear({x: 640, y: 400}, {width: 200, height: 40}, page), {left: 540, top: 348});
  assert.deepEqual(placeNear({x: 5, y: 400}, {width: 200, height: 40}, page), {left: 16, top: 348}, 'pushed in from the left edge');
  assert.deepEqual(placeNear({x: 1279, y: 400}, {width: 200, height: 40}, page), {left: 1064, top: 348}, 'and from the right');
  assert.deepEqual(placeNear({x: 640, y: 20}, {width: 200, height: 40}, page), {left: 540, top: 32}, 'below a point at the top');
  assert.deepEqual(placeNear({x: 640, y: 790}, {width: 200, height: 40}, page).top, 738);
});

test('the copy says how far it is while it goes, and only what it did at its end', () => {
  assert.equal(progressText({phase: 'copying', done: 45, total: 100, text: 'Adding plan.pdf'}), 'Adding plan.pdf · 45%');
  assert.equal(progressText({phase: 'copying', done: 100, total: 100, text: 'Adding plan.pdf'}), 'Adding plan.pdf · 99%',
    'never all of it while it still goes');
  assert.equal(progressText({phase: 'copying', done: 0, total: 0, text: 'Adding empty.txt'}), 'Adding empty.txt · 0%');
  assert.equal(progressText({phase: 'done', done: 100, total: 100, text: 'Added plan.pdf to the workspace'}),
    'Added plan.pdf to the workspace');
  assert.equal(progressText({phase: 'failed', done: 0, total: 0, text: 'Calculator doesn\'t open files'}),
    'Calculator doesn\'t open files');
});
