import test from 'node:test';
import assert from 'node:assert/strict';
import {browserFamily, isWebKit, rendererUnavailable, webgpuRefusal, WEBKIT_WEBGPU_REFUSAL} from './webgpu-safety.mjs';

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
  // Safari's Develop menu claiming Chrome keeps Apple's vendor.
  assert.equal(isWebKit({userAgent: CHROME_MAC, vendor: 'Apple Computer, Inc.'}), true);
});

// Catches a guard keyed on navigator.vendor alone, which a spoofed Safari
// user agent in Chrome (the only safe way to test this) would bypass.
test('the fallback flag refuses a Safari user agent even where the vendor says Google', () => {
  assert.equal(webgpuRefusal(env(SAFARI_MAC, {vendor: 'Google Inc.', search: '?unsafeWebKitGPU=0'})), WEBKIT_WEBGPU_REFUSAL);
  assert.equal(webgpuRefusal(env(CHROME_MAC, {vendor: 'Google Inc.', search: '?unsafeWebKitGPU=0'})), undefined);
});

test('WebKit renders by default; only ?unsafeWebKitGPU=0 refuses it', () => {
  assert.equal(webgpuRefusal(env(SAFARI_MAC, {vendor: 'Apple Computer, Inc.'})), undefined);
  assert.equal(webgpuRefusal(env(CHROME_IOS, {vendor: 'Apple Computer, Inc.'})), undefined);
  assert.equal(webgpuRefusal(env(SAFARI_MAC, {search: '?unsafeWebKitGPU=1'})), undefined);
  assert.equal(webgpuRefusal(env(SAFARI_MAC, {search: '?unsafeWebKitGPU'})), undefined);
  assert.equal(webgpuRefusal(env(SAFARI_MAC, {search: '?unsafeWebKitGPU=0'})), WEBKIT_WEBGPU_REFUSAL);
  assert.match(WEBKIT_WEBGPU_REFUSAL, /unsafeWebKitGPU=0/);
});

const LIBREWOLF = 'Mozilla/5.0 (X11; Linux x86_64; rv:154.0) Gecko/20100101 Firefox/154.0 LibreWolf/154.0';
const CHROME_LINUX = 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36';
const withGpu = (userAgent, gpu, options) => {
  const base = env(userAgent, options);
  return {...base, navigator: {...base.navigator, gpu}};
};

test('browsers are told apart for the setting that turns WebGPU on', () => {
  assert.equal(browserFamily({userAgent: LIBREWOLF}), 'librewolf');
  assert.equal(browserFamily({userAgent: FIREFOX_MAC}), 'firefox');
  assert.equal(browserFamily({userAgent: CHROME_MAC, vendor: 'Google Inc.'}), 'chromium');
  assert.equal(browserFamily({userAgent: EDGE_MAC, vendor: 'Google Inc.'}), 'chromium');
  assert.equal(browserFamily({userAgent: SAFARI_MAC, vendor: 'Apple Computer, Inc.'}), 'safari');
});

test('without navigator.gpu the visitor is told the setting to change, not that joining failed', async () => {
  const off = await rendererUnavailable(env(LIBREWOLF));
  assert.match(off.reason, /WebGPU.*turned off/);
  assert.ok(off.steps.some(step => step.includes('dom.webgpu.enabled')));
  assert.ok((await rendererUnavailable(env(CHROME_LINUX))).steps.some(step => step.includes('chrome://flags')));
});

test('an adapter request that yields nothing or throws is unavailable; an adapter is enough', async () => {
  assert.match((await rendererUnavailable(withGpu(CHROME_LINUX, {requestAdapter: async () => null}))).reason, /no graphics adapter/);
  assert.ok(await rendererUnavailable(withGpu(CHROME_LINUX, {requestAdapter: async () => { throw Error('lost'); }})));
  assert.equal(await rendererUnavailable(withGpu(CHROME_LINUX, {requestAdapter: async () => ({})})), undefined);
});

test('the WebKit fallback flag refuses before any adapter is asked for', async () => {
  const asked = [];
  const refused = withGpu(SAFARI_MAC, {requestAdapter: async () => { asked.push(1); return {}; }}, {search: '?unsafeWebKitGPU=0'});
  assert.deepEqual(await rendererUnavailable(refused), {reason: WEBKIT_WEBGPU_REFUSAL, steps: []});
  assert.equal(asked.length, 0);
});
