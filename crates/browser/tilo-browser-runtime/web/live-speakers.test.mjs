import test from 'node:test';
import assert from 'node:assert/strict';
import {createSpeakers} from './live-speakers.mjs';

test('mounts sharing a layer: the one acted in speaks, and an action that moved nothing does not take over', () => {
  const published = [];
  const layer = {state: 'live', publishPresence: partial => { published.push(partial); return true; }, publishPreview: () => true};
  const speakers = createSpeakers();
  const notes = speakers.mount();
  const chat = speakers.mount();
  const idle = speakers.mount();
  const NOTES = {app: 'so.tilo.word-processor', record: 'notes'};
  const CHAT = {app: 'so.tilo.chat', record: 'general'};
  // The first to publish speaks until another mount is acted in.
  assert.equal(chat.view(layer).publishPresence({focus: CHAT, selection: {anchor: 'c0', head: 'c0'}}), true);
  // An edit in the notes moves their caret: the notes speak.
  notes.acted();
  assert.equal(notes.view(layer).publishPresence({focus: NOTES, selection: {anchor: 'n9', head: 'n9'}}), true);
  // Posting in Chat left its caret where it was: the notes keep speaking.
  chat.acted();
  assert.equal(chat.view(layer).publishPresence({focus: CHAT, selection: {anchor: 'c0', head: 'c0'}}), false);
  // An idle mount's state republish is dropped.
  assert.equal(idle.view(layer).publishPresence({focus: null, selection: null}), false);
  assert.deepEqual(published.at(-1).focus, NOTES);
  // Input claims at once; releasing hands the layer back.
  chat.claim(layer);
  assert.equal(chat.view(layer).publishPresence({pointer: [1, 2]}), true);
  assert.equal(idle.view(layer).publishPreview({app: 'a', record: 'r'}, []), false);
  chat.release();
  assert.equal(idle.view(layer).publishPresence({focus: null, selection: null}), true);
});

test('a mount that takes the layer back says its caret again', () => {
  const published = [];
  const layer = {publishPresence: partial => { published.push(partial); return true; }, publishPreview: () => true};
  const speakers = createSpeakers();
  const writer = speakers.mount();
  const chat = speakers.mount();
  writer.view(layer).publishPresence({selection: {anchor: 'w1', head: 'w1'}});
  chat.claim(layer);
  chat.view(layer).publishPresence({selection: {anchor: 'c1', head: 'c1'}});
  // The writer's caret did not move while Chat spoke, so its status sends
  // nothing new: the claim republishes it.
  writer.claim(layer);
  assert.deepEqual(published.at(-1), {selection: {anchor: 'w1', head: 'w1'}});
  const count = published.length;
  writer.claim(layer);
  assert.equal(published.length, count, 'claiming a layer it already speaks for sends nothing');
  speakers.mount().claim(layer);
  assert.equal(published.length, count, 'a mount that never published has nothing to say');
});

test('a view reads the layer it speaks through', () => {
  const layer = {state: 'live', members: () => ['cc'], publishPresence: () => true, publishPreview: () => true};
  const view = createSpeakers().mount().view(layer);
  assert.equal(view.state, 'live');
  assert.deepEqual(view.members(), ['cc']);
  assert.equal(createSpeakers().mount().view(layer) === view, false, 'each mount has its own view');
});
