import test from 'node:test';
import assert from 'node:assert/strict';
import {pageWheelAsPixels,wheelNotchesAsLines,wheelPixels} from './host-core.mjs';

test('a trusted mouse wheel notch becomes the line a native wheel sends, trackpads and scripts stay pixels',()=>{
  const notch=(o)=>({isTrusted:true,deltaMode:0,deltaX:0,deltaY:0,wheelDeltaX:0,wheelDeltaY:0,clientX:7,clientY:8,
    ctrlKey:false,shiftKey:false,altKey:false,metaKey:false,...o});
  const right=wheelNotchesAsLines(notch({deltaX:100,wheelDeltaX:-120}),false);
  assert.deepEqual([right.deltaMode,right.deltaX,right.deltaY,right.clientX],[1,1,0,7]);
  assert.equal(wheelNotchesAsLines(notch({deltaY:-200,wheelDeltaY:240}),false).deltaY,-2);
  // A trackpad's legacy delta is three times its pixels.
  assert.equal(wheelNotchesAsLines(notch({deltaX:40,wheelDeltaX:-120}),false),undefined);
  // Events made by scripts, line wheels and tiny per-notch pixels are left alone.
  assert.equal(wheelNotchesAsLines(notch({isTrusted:false,deltaX:100,wheelDeltaX:-120}),false),undefined);
  assert.equal(wheelNotchesAsLines(notch({deltaMode:1,deltaX:3,wheelDeltaX:-120}),false),undefined);
  assert.equal(wheelNotchesAsLines(notch({deltaX:10,wheelDeltaX:-120}),false),undefined);
  assert.equal(wheelNotchesAsLines(notch({deltaX:100,wheelDeltaX:-100}),false),undefined);
  // Off a Mac the browser turns Shift+wheel sideways; it goes back to vertical with Shift, as natively.
  const shifted=wheelNotchesAsLines(notch({shiftKey:true,deltaX:100,wheelDeltaX:-120}),false);
  assert.deepEqual([shifted.deltaX,shifted.deltaY,shifted.shiftKey],[0,1,true]);
  const mac=wheelNotchesAsLines(notch({shiftKey:true,deltaX:100,wheelDeltaX:-120}),true);
  assert.deepEqual([mac.deltaX,mac.deltaY],[1,0]);
});

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
