import test from 'node:test';
import assert from 'node:assert/strict';
import {browserFamily, isWebKit, rendererChoice, webgpuRefusal, WEBKIT_WEBGPU_REFUSAL} from './webgpu-safety.mjs';

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

test('browsers are told apart for how they are told the desktop is on the CPU', () => {
  assert.equal(browserFamily({userAgent: LIBREWOLF}), 'firefox');
  assert.equal(browserFamily({userAgent: FIREFOX_MAC}), 'firefox');
  assert.equal(browserFamily({userAgent: CHROME_MAC, vendor: 'Google Inc.'}), 'chromium');
  assert.equal(browserFamily({userAgent: EDGE_MAC, vendor: 'Google Inc.'}), 'chromium');
  assert.equal(browserFamily({userAgent: SAFARI_MAC, vendor: 'Apple Computer, Inc.'}), 'safari');
});

// Firefox on Linux, and LibreWolf anywhere, have no navigator.gpu: the page
// draws the desktop on the CPU instead of the refusal it used to show.
test('a browser without WebGPU draws on the CPU, and Firefox is told the setting that would speed it up', async () => {
  const firefox = await rendererChoice(env(LIBREWOLF));
  assert.equal(firefox.renderer, 'cpu');
  assert.match(firefox.reason, /dom\.webgpu\.enabled/);
  assert.ok(!/WebGL/.test(firefox.reason), 'a WebGL prompt is not offered as the fix');
  const chromium = await rendererChoice(env(CHROME_LINUX));
  assert.equal(chromium.renderer, 'cpu');
  assert.match(chromium.reason, /hardware acceleration|chrome:\/\/flags/);
});

test('WebGPU on but no adapter is the CPU too, pointed at acceleration and the blocklist', async () => {
  const none = await rendererChoice(withGpu(LIBREWOLF, {requestAdapter: async () => null}));
  assert.equal(none.renderer, 'cpu');
  assert.match(none.reason, /no graphics adapter/);
  assert.match(none.reason, /gfx\.webgpu\.ignore-blocklist/);
});

test('an adapter request that yields nothing or throws is the CPU; an adapter is the GPU', async () => {
  assert.equal((await rendererChoice(withGpu(CHROME_LINUX, {requestAdapter: async () => null}))).renderer, 'cpu');
  assert.equal((await rendererChoice(withGpu(CHROME_LINUX, {requestAdapter: async () => { throw Error('lost'); }}))).renderer, 'cpu');
  assert.deepEqual(await rendererChoice(withGpu(CHROME_LINUX, {requestAdapter: async () => ({})})), {renderer: 'webgpu'});
});

// Catches the page probing twice, or the renderer asking again: the adapter
// request is the page's, and its answer serves every mount.
test('the choice takes the adapter the entry page asked for while its modules loaded', async () => {
  const asked = [];
  const early = {...withGpu(CHROME_LINUX, {requestAdapter: async () => { asked.push(1); return {}; }}), __tiloAdapter: Promise.resolve(null)};
  assert.equal((await rendererChoice(early)).renderer, 'cpu');
  assert.equal((await rendererChoice({...early, __tiloAdapter: Promise.resolve({})})).renderer, 'webgpu');
  assert.equal(asked.length, 0);
});

// Catches a GPU process too busy to answer (several pages starting at once
// on a software rasteriser) demoting a page that has a GPU to the CPU.
test('an adapter answer that never comes keeps the page on the GPU', async () => {
  const silent = {...withGpu(CHROME_LINUX, {requestAdapter: () => new Promise(() => {})}), __tiloAdapter: new Promise(() => {})};
  const warned = [];
  const warn = console.warn;
  console.warn = (...args) => warned.push(args.join(' '));
  try {
    assert.deepEqual(await rendererChoice(silent, {answerMs: 20}), {renderer: 'webgpu'});
  } finally {
    console.warn = warn;
  }
  assert.match(warned.join('\n'), /no adapter answer/);
  // An answer in time still counts either way.
  assert.equal((await rendererChoice({...silent, __tiloAdapter: Promise.resolve(null)}, {answerMs: 20})).renderer, 'cpu');
});
