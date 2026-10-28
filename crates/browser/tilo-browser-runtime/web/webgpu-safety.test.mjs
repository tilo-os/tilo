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

// LibreWolf 157 as it reports itself: Firefox's user agent, no LibreWolf token.
const LIBREWOLF = 'Mozilla/5.0 (X11; Linux x86_64; rv:157.0) Gecko/20100101 Firefox/157.0';
const CHROME_LINUX = 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36';
const withGpu = (userAgent, gpu, options) => {
  const base = env(userAgent, options);
  return {...base, navigator: {...base.navigator, gpu}};
};

test('browsers are told apart for the setting that turns WebGPU on', () => {
  assert.equal(browserFamily({userAgent: LIBREWOLF}), 'firefox');
  assert.equal(browserFamily({userAgent: FIREFOX_MAC}), 'firefox');
  assert.equal(browserFamily({userAgent: CHROME_MAC, vendor: 'Google Inc.'}), 'chromium');
  assert.equal(browserFamily({userAgent: EDGE_MAC, vendor: 'Google Inc.'}), 'chromium');
  assert.equal(browserFamily({userAgent: SAFARI_MAC, vendor: 'Apple Computer, Inc.'}), 'safari');
});

test('without navigator.gpu the visitor is told the setting to change, not that joining failed', async () => {
  const off = await rendererUnavailable(env(LIBREWOLF));
  assert.match(off.reason, /WebGPU.*turned off/);
  assert.ok(off.steps.some(step => step.includes('dom.webgpu.enabled') && step.includes('Try again')));
  assert.ok(off.steps.some(step => step.includes('LibreWolf')), 'LibreWolf cannot be told from Firefox, so both are named');
  assert.ok(off.steps.some(step => step.includes('WebGL')), "LibreWolf's WebGL prompt is not mistaken for the fix");
  assert.ok((await rendererUnavailable(env(CHROME_LINUX))).steps.some(step => step.includes('chrome://flags')));
});

test('Firefox with WebGPU on but no adapter is pointed at acceleration and the blocklist, not the setting it already has', async () => {
  const none = await rendererUnavailable(withGpu(LIBREWOLF, {requestAdapter: async () => null}));
  assert.match(none.reason, /no graphics adapter/);
  assert.ok(none.steps.some(step => step.includes('gfx.webgpu.ignore-blocklist')));
  assert.ok(!none.steps.some(step => step.includes('dom.webgpu.enabled')));
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

test('the check takes the adapter the entry page asked for while its modules loaded', async () => {
  const asked = [];
  const early = {...withGpu(CHROME_LINUX, {requestAdapter: async () => { asked.push(1); return {}; }}), __tiloAdapter: Promise.resolve(null)};
  assert.match((await rendererUnavailable(early)).reason, /no graphics adapter/);
  assert.equal(await rendererUnavailable({...early, __tiloAdapter: Promise.resolve({})}), undefined);
  assert.equal(asked.length, 0);
});

// Catches a GPU process too busy to answer (several pages starting at once
// on a software rasteriser) leaving the page on "Loading Tilo…" for good.
test('an adapter answer that never comes does not hold the page', async () => {
  const silent = {...withGpu(CHROME_LINUX, {requestAdapter: () => new Promise(() => {})}), __tiloAdapter: new Promise(() => {})};
  const warned = [];
  const warn = console.warn;
  console.warn = (...args) => warned.push(args.join(' '));
  try {
    assert.equal(await rendererUnavailable(silent, {answerMs: 20}), undefined);
  } finally {
    console.warn = warn;
  }
  assert.match(warned.join('\n'), /no adapter answer/);
  // An answer in time still counts.
  assert.match((await rendererUnavailable({...silent, __tiloAdapter: Promise.resolve(null)}, {answerMs: 20})).reason, /no graphics adapter/);
});
