import test from 'node:test';
import assert from 'node:assert/strict';
import { MountRegistry, physicalViewport, wheelPixels, transactionComplete } from './host-core.mjs';

test('old cleanup and cancelled async mount cannot remove a newer generation', () => {
  const mounts = new MountRegistry();
  const target = {};
  const old = mounts.begin(target);
  const current = mounts.begin(target);
  assert.equal(mounts.current(old), false);
  assert.equal(mounts.finish(old, {close() { throw Error('stale mounted'); }}), false);
  assert.equal(mounts.remove(old), false);
  assert.equal(mounts.current(current), true);
  assert.equal(mounts.remove(current), true);
  assert.equal(mounts.current(current), false);
});
test('simultaneous mounts retain independent lifetime and terminal shutdown', () => {
  const mounts = new MountRegistry();
  const first = mounts.begin({}), second = mounts.begin({});
  assert.equal(mounts.remove(first), true);
  assert.equal(mounts.current(second), true);
  mounts.shutdown();
  assert.equal(mounts.current(second), false);
  assert.throws(() => mounts.begin({}), /shutdown/);
});
test('resize keeps logical geometry stable at DPR2 and suspends zero canvas', () => {
  assert.deepEqual(physicalViewport(600, 400, 2), {width:1200,height:800,dpr:2,suspended:false});
  assert.equal(physicalViewport(0,400,2).suspended, true);
  assert.deepEqual(wheelPixels(2,3,1,600,400), [-32,-48]);
  assert.deepEqual(wheelPixels(1,1,2,600,400), [-600,-400]);
});
test('a successful write request followed by transaction abort never acknowledges durability', async () => {
  const transaction = new EventTarget();
  const completion = transactionComplete(transaction);
  transaction.dispatchEvent(new Event('success'));
  transaction.dispatchEvent(new Event('abort'));
  await assert.rejects(completion, /aborted/);
});
test('only transaction completion acknowledges durability', async () => {
  const transaction = new EventTarget();
  let acknowledged = false;
  const completion = transactionComplete(transaction).then(() => { acknowledged=true; });
  transaction.dispatchEvent(new Event('success'));
  await Promise.resolve();
  assert.equal(acknowledged,false);
  transaction.dispatchEvent(new Event('complete'));
  await completion;
  assert.equal(acknowledged,true);
});
