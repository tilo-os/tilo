import test from 'node:test';
import assert from 'node:assert/strict';
import {UNNAMED, initial, isUnnamed, personLabel, shownName} from './person.mjs';

test('a person nobody named is said in words, never by a key or a name made up', () => {
  assert.equal(personLabel(''), UNNAMED);
  assert.equal(personLabel('   '), UNNAMED);
  assert.equal(personLabel(undefined), UNNAMED);
  assert.equal(personLabel('a'.repeat(64)), UNNAMED, 'a key says what a person is, not who');
  assert.equal(personLabel(UNNAMED), UNNAMED, 'the words said in place of a name are not one');
  assert.equal(personLabel(' Ada Lovelace '), 'Ada Lovelace');
  assert.equal(personLabel('dev-7'), 'dev-7', 'a short label is a name, not a key');
});

test('a face shows the first letter of a name, and the glyph where nobody named the person', () => {
  assert.equal(initial('ada'), 'A');
  assert.equal(initial('  Bo  '), 'B');
  assert.equal(initial(''), '');
  assert.equal(initial(UNNAMED), '');
  assert.equal(initial('f'.repeat(40)), '');
});

test('these are no name of a person’s', () => {
  for (const name of ['', ' ', null, undefined, UNNAMED, 'a'.repeat(64)]) {
    assert.equal(isUnnamed(name), true, JSON.stringify(name));
    assert.equal(shownName(name), '', JSON.stringify(name));
  }
  assert.equal(isUnnamed('Ada'), false);
});
