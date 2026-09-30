import test from 'node:test';
import assert from 'node:assert/strict';
import {pageWheelAsPixels,wheelPixels} from './host-core.mjs';

test('wheel deltas scale per unit on both axes',()=>{
  assert.deepEqual(wheelPixels(3,-2,0,800,600),[-3,2]);
  assert.deepEqual(wheelPixels(3,-2,1,800,600),[-48,32]);
  assert.deepEqual(wheelPixels(0.5,-1,2,800,600),[-400,600]);
});

test('only page-unit wheels are converted, keeping deltaX, the pointer and every modifier',()=>{
  const base={deltaX:0,deltaY:0,clientX:5,clientY:6,ctrlKey:false,shiftKey:false,altKey:false,metaKey:false};
  assert.equal(pageWheelAsPixels({...base,deltaX:40,deltaMode:0},800,600),undefined);
  assert.equal(pageWheelAsPixels({...base,deltaX:-2,deltaMode:1},800,600),undefined);
  assert.deepEqual(pageWheelAsPixels({...base,deltaX:-1,deltaY:0.25,deltaMode:2,ctrlKey:true,metaKey:true},800,600),
    {deltaX:-800,deltaY:150,deltaMode:0,clientX:5,clientY:6,ctrlKey:true,shiftKey:false,altKey:false,metaKey:true,bubbles:false,cancelable:true});
});
