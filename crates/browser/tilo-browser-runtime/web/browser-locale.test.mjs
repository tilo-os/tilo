import assert from 'node:assert/strict';
import test from 'node:test';
import {browserSettings, browserTimeFormat} from './browser-locale.mjs';

const intlWith = resolved => ({DateTimeFormat: class { resolvedOptions() { return resolved; } }});

test('a browser with a 12-hour clock starts the profile on 12-hour times', () => {
  assert.equal(browserTimeFormat(intlWith({hourCycle: 'h12'})), 'H12');
  assert.equal(browserTimeFormat(intlWith({hourCycle: 'h11'})), 'H12');
  assert.equal(browserTimeFormat(intlWith({hour12: true})), 'H12');
  assert.deepEqual(browserSettings(intlWith({hourCycle: 'h12'})), {locale: {time_format: 'H12'}});
});

test('a 24-hour browser keeps 24-hour times, and one without Intl seeds nothing', () => {
  assert.equal(browserTimeFormat(intlWith({hourCycle: 'h23'})), 'H24');
  assert.equal(browserTimeFormat(intlWith({})), 'H24');
  assert.deepEqual(browserSettings({}), {});
});

test('the real locale names one of the two styles', () => {
  assert.ok(['H12', 'H24'].includes(browserTimeFormat()));
  // A runtime built with only English locale data (small-icu) resolves
  // other locales to English, so only the locales it has are checked.
  for (const [locale, expected] of [['en-US', 'H12'], ['de-DE', 'H24']]) {
    if (Intl.DateTimeFormat.supportedLocalesOf([locale]).length === 0) continue;
    const intl = {DateTimeFormat: class extends Intl.DateTimeFormat { constructor(_, options) { super(locale, options); } }};
    assert.equal(browserTimeFormat(intl), expected, locale);
  }
});
