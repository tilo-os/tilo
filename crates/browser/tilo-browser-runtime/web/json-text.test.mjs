import assert from 'node:assert/strict';
import test from 'node:test';
import {memberText, objectText} from './json-text.mjs';

// What a Linux host issues: nanosecond timestamps with 19 significant digits.
const CERT = '{"user_public_key":[1,2],"device_label":"Browser \\"A\\", {b}","capabilities":{"foreign":false},'
  + '"issued_at_ns":1792023733547123457,"expires_at_ns":1794615733547123457,"signature":[9,8]}';
const CERTIFIED = `{"cert":${CERT},"hostCert":{"issued_at_ns":1,"expires_at_ns":null}}`;

test('a JS round trip changes a nanosecond timestamp; member text keeps it', () => {
  assert.notEqual(JSON.stringify(JSON.parse(CERT)), CERT, 'the reason these helpers exist');
  assert.equal(memberText(CERTIFIED, 'cert'), CERT);
  assert.equal(memberText(CERTIFIED, 'hostCert'), '{"issued_at_ns":1,"expires_at_ns":null}');
  assert.equal(memberText(CERTIFIED, 'missing'), undefined);
});

test('members are found past strings with quotes, braces and escapes, nested values and whitespace', () => {
  const json = ' { "a" : "x\\"}{" , "b":[{"c":"]"},2] ,\n"d": -1.5e3, "e" : true , "f":null } ';
  assert.equal(memberText(json, 'a'), '"x\\"}{"');
  assert.equal(memberText(json, 'b'), '[{"c":"]"},2]');
  assert.equal(memberText(json, 'd'), '-1.5e3');
  assert.equal(memberText(json, 'e'), 'true');
  assert.equal(memberText(json, 'f'), 'null');
  assert.equal(memberText('{}', 'a'), undefined);
});

test('an object is composed from member texts, leaving undefined ones out', () => {
  const composed = objectText({cert: memberText(CERTIFIED, 'cert'), hostCert: memberText(CERTIFIED, 'hostCert'),
    displayName: JSON.stringify('Ada "the" Lovelace'), absent: undefined});
  assert.equal(composed, `{"cert":${CERT},"hostCert":{"issued_at_ns":1,"expires_at_ns":null},"displayName":"Ada \\"the\\" Lovelace"}`);
  assert.equal(JSON.parse(composed).displayName, 'Ada "the" Lovelace');
});

test('text that is not an object is refused', () => {
  for (const bad of ['[1]', '"cert"', '{"a" 1}', '{"a":1', '{"a":"x}', '']) {
    assert.throws(() => memberText(bad, 'a'), SyntaxError, bad);
  }
  assert.throws(() => memberText({cert: {}}, 'cert'), TypeError);
});
