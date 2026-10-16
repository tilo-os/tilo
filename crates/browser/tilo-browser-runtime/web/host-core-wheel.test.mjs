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
  assert.equal(wheelNotchesAsLines(notch({deltaMode:2,deltaX:1,wheelDeltaX:-120}),false),undefined);
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

// Firefox on Linux, a mouse's thumb wheel spun one way and back: every notch
// is six lines with a wheelDelta of 120 against them, 17 to 50 ms apart.
const firefox=(o)=>({isTrusted:true,deltaMode:1,deltaX:0,deltaY:0,wheelDeltaX:0,wheelDeltaY:0,clientX:7,clientY:8,
  ctrlKey:false,shiftKey:false,altKey:false,metaKey:false,...o});
// The strip pans 40 px a line (`WHEEL_LINE_PX` in the desktop surface).
const panned=(events,perNotch={x:0,y:0})=>events.reduce((px,e)=>px+40*wheelNotchesAsLines(firefox(e),false,perNotch).deltaX,0);

test("Firefox's spin of a sideways wheel pans 40 px a notch, as in Chrome and natively",()=>{
  const right=Array.from({length:20},()=>({deltaX:6,wheelDeltaX:-120}));
  const left=Array.from({length:20},()=>({deltaX:-6,wheelDeltaX:120}));
  const perNotch={x:0,y:0};
  assert.equal(panned(right,perNotch),20*40);
  assert.equal(panned(left,perNotch),-20*40);
});

test("Firefox's merged notches keep the first one's wheelDelta and still pan 40 px each",()=>{
  // Its content process sums merged events' lines but not their ticks.
  const stream=[6,12,6,18,6,30,48].map(deltaX=>({deltaX,wheelDeltaX:-120}));
  assert.equal(panned(stream),(1+2+1+3+1+5+8)*40);
});

test('Firefox line wheels without a wheelDelta count six lines a sideways notch, three a vertical one',()=>{
  assert.equal(panned([{deltaX:6},{deltaX:-12}]),-40);
  assert.equal(wheelNotchesAsLines(firefox({deltaY:9}),false).deltaY,3);
  // A vertical notch is three lines, learnt the same way.
  const perNotch={x:0,y:0};
  assert.equal(wheelNotchesAsLines(firefox({deltaY:3,wheelDeltaY:-120}),false,perNotch).deltaY,1);
  assert.equal(wheelNotchesAsLines(firefox({deltaY:-6,wheelDeltaY:120}),false,perNotch).deltaY,-2);
});

test("Chrome's merged notches count every notch: it sums their wheelDelta with their pixels",()=>{
  // Twenty queued notches as headless Chrome handed them to a busy page.
  const chrome=(o)=>({isTrusted:true,deltaMode:0,deltaX:0,deltaY:0,wheelDeltaX:0,wheelDeltaY:0,clientX:7,clientY:8,
    ctrlKey:false,shiftKey:false,altKey:false,metaKey:false,...o});
  const stream=[1,2,2,2,2,2,2,2,2,2,1].map(n=>chrome({deltaX:-120*n,wheelDeltaX:120*n}));
  assert.equal(stream.reduce((px,e)=>px+40*wheelNotchesAsLines(e,false).deltaX,0),-20*40);
});
