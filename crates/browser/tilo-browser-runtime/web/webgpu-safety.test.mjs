import test from 'node:test';
import assert from 'node:assert/strict';
import {isWebKit, webgpuRefusal, WEBKIT_WEBGPU_REFUSAL} from './webgpu-safety.mjs';

const SAFARI_MAC = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/27.0 Safari/605.1.15';
const CHROME_MAC = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36';
const EDGE_MAC = `${CHROME_MAC} Edg/140.0.0.0`;
const FIREFOX_MAC = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:143.0) Gecko/20100101 Firefox/143.0';
const CHROME_IOS = 'Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/140.0.0.0 Mobile/15E148 Safari/604.1';

const env = (userAgent, {vendor = '', search = '', maxTouchPoints = 0} = {}) => ({navigator: {userAgent, vendor, maxTouchPoints}, location: {search}});

test('Safari and every iOS browser are WebKit; Chrome, Edge and Firefox are not', () => {
  assert.equal(isWebKit({userAgent: SAFARI_MAC, vendor: 'Apple Computer, Inc.'}), true);
  assert.equal(isWebKit({userAgent: CHROME_IOS, vendor: 'Apple Computer, Inc.'}), true);
  assert.equal(isWebKit({userAgent: SAFARI_MAC, maxTouchPoints: 5}), true);
  assert.equal(isWebKit({userAgent: CHROME_MAC, vendor: 'Google Inc.'}), false);
  assert.equal(isWebKit({userAgent: EDGE_MAC, vendor: 'Google Inc.'}), false);
  assert.equal(isWebKit({userAgent: FIREFOX_MAC, vendor: ''}), false);
});

// Catches a guard keyed on navigator.vendor alone, which a spoofed Safari
// user agent in Chrome (the only safe way to test this) would bypass.
test('a Safari user agent is refused even where the vendor says Google', () => {
  assert.equal(webgpuRefusal(env(SAFARI_MAC, {vendor: 'Google Inc.'})), WEBKIT_WEBGPU_REFUSAL);
  assert.equal(webgpuRefusal(env(CHROME_MAC, {vendor: 'Google Inc.'})), undefined);
});

test('only the explicit query flag lets WebKit start the renderer', () => {
  assert.equal(webgpuRefusal(env(SAFARI_MAC, {search: '?unsafeWebKitGPU=1'})), undefined);
  assert.equal(webgpuRefusal(env(SAFARI_MAC, {search: '?unsafeWebKitGPU=0'})), WEBKIT_WEBGPU_REFUSAL);
  assert.equal(webgpuRefusal(env(SAFARI_MAC, {search: '?unsafeWebKitGPU'})), WEBKIT_WEBGPU_REFUSAL);
  assert.match(WEBKIT_WEBGPU_REFUSAL, /Safari isn't supported yet — use Chrome or Firefox/);
});
