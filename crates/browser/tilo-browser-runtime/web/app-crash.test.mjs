import test from 'node:test';
import assert from 'node:assert/strict';
import {createCrashQuarantine,crashAction} from './app-crash.mjs';

function memoryStorage(){
  const values=new Map();
  return {getItem:key=>values.has(key)?values.get(key):null,setItem:(key,value)=>values.set(key,String(value))};
}

test('a crashed window is quarantined in its own room until it expires', () => {
  let clock=1000;
  const storage=memoryStorage();
  const crashes=createCrashQuarantine({storage,now:()=>clock});
  assert.equal(crashes.record('room-a',{tab:'18446744073709551615',message:'no filesystem on this platform'}),true);
  assert.deepEqual(crashes.entries('room-a'),[{tab:'18446744073709551615',message:'no filesystem on this platform'}]);
  assert.deepEqual(crashes.entries('room-b'),[]);
  // Another page of the same profile sees it.
  assert.equal(createCrashQuarantine({storage,now:()=>clock}).entries('room-a').length,1);
  clock+=8*24*60*60*1000;
  assert.deepEqual(crashes.entries('room-a'),[]);
});

test('a window that crashes again while quarantined does not restart the page', () => {
  const crashes=createCrashQuarantine({storage:memoryStorage(),now:()=>0});
  const report={tab:'7',message:'boom'};
  assert.equal(crashAction(report,crashes.record('room',report)),'restart');
  assert.equal(crashAction(report,crashes.record('room',report)),'report');
  crashes.forget('room','7');
  assert.deepEqual(crashes.entries('room'),[]);
});

test('a panic outside any window is only reported', () => {
  assert.equal(crashAction({tab:null,message:'boom'},true),'report');
  assert.equal(crashAction(undefined,true),'report');
});

test('unreadable or unavailable storage quarantines nothing and does not throw', () => {
  const broken={getItem:()=>'{not json',setItem:()=>{throw Error('quota');}};
  const crashes=createCrashQuarantine({storage:broken,now:()=>0});
  assert.deepEqual(crashes.entries('room'),[]);
  assert.equal(crashes.record('room',{tab:'1',message:'x'}),true);
  assert.deepEqual(createCrashQuarantine({storage:undefined}).entries('room'),[]);
});
