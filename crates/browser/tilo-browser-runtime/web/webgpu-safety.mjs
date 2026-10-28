// WebKit's WebGPU (Safari, and every browser on iOS/iPadOS) once ran the
// desktop's vello compute frames into a GPU hang: Safari's GPU process passed
// its 600 MB limit, was killed mid-frame, and froze macOS's WindowServer until
// the watchdog rebooted the machine. The renderer now bounds its GPU memory
// and work (canvas cap, total vello budget, WebKit's conservative profile,
// terminating fine shader); in real Safari the desktop peaked at 448 MB with
// eleven windows open, so WebKit renders by default.
//
// `?unsafeWebKitGPU=0` on the page URL refuses WebGPU on WebKit again, as a
// fallback; `?unsafeWebKitGPU=1` forces it on if the default is flipped back.
//
// A browser that offers no WebGPU at all is not refused: the desktop is drawn
// on the CPU instead (`rendererChoice`, `web/cpu-present.mjs`), so the
// visitors whose browsers ship with it off (Firefox on Linux) get the desktop
// rather than instructions. `?unsafeWebKitGPU=0` is still a refusal: it asks
// for no GPU, not for the slower renderer.

export const WEBKIT_GUARD_DEFAULT = false;
export const WEBKIT_WEBGPU_OVERRIDE = 'unsafeWebKitGPU';
export const WEBKIT_WEBGPU_REFUSAL = "The desktop's renderer is turned off for Safari on this page (?unsafeWebKitGPU=0). Remove that from the address to use it, or use Chrome or Firefox.";

/** Whether the user agent is WebKit: Safari on macOS, or any browser on iOS/iPadOS. */
export function isWebKit({userAgent = '', vendor = '', maxTouchPoints = 0} = {}) {
  if (/\b(iPhone|iPad|iPod)\b/.test(userAgent)) return true;
  // iPadOS reports itself as a Mac; touch points tell them apart.
  if (/\bMacintosh\b/.test(userAgent) && maxTouchPoints > 1 && /AppleWebKit\//.test(userAgent)) return true;
  // WebKit froze its token at 605.1.15; Blink (and jsdom) report 537.36.
  if (/AppleWebKit\/6\d\d/.test(userAgent) && !/\b(Chrome|Chromium|Edg|OPR|Firefox)\//.test(userAgent)) return true;
  // Safari's Develop menu can claim Chrome's user agent; its vendor stays
  // Apple's. jsdom imitates that vendor.
  return vendor === 'Apple Computer, Inc.' && !/\bjsdom\//.test(userAgent);
}

/** The refusal to show instead of starting WebGPU, or `undefined` when it is safe to start. */
export function webgpuRefusal(env = globalThis) {
  const navigator = env.navigator ?? {};
  if (!isWebKit(navigator)) return undefined;
  let search = '';
  try { search = env.location?.search ?? ''; } catch {}
  const override = new URLSearchParams(search).get(WEBKIT_WEBGPU_OVERRIDE);
  const guarded = override === '1' ? false : override === '0' ? true : WEBKIT_GUARD_DEFAULT;
  return guarded ? WEBKIT_WEBGPU_REFUSAL : undefined;
}

/**
 * Which browser's settings a visitor drawing on the CPU would change.
 * `firefox` covers its forks too: LibreWolf resists fingerprinting and sends
 * Firefox's user agent unchanged, so the setting is named the same way.
 */
export function browserFamily({userAgent = '', vendor = '', maxTouchPoints = 0} = {}) {
  if (/\bFirefox\//.test(userAgent)) return 'firefox';
  if (isWebKit({userAgent, vendor, maxTouchPoints})) return 'safari';
  if (/\b(Chrome|Chromium|Edg|OPR)\//.test(userAgent)) return 'chromium';
  return 'other';
}

// How a page that must draw without WebGPU says so, per browser. Firefox and
// its forks have no per-site WebGPU prompt: with `dom.webgpu.enabled` off
// (LibreWolf's default, and Firefox's on Linux) the page has no
// `navigator.gpu` at all, so the setting is named. LibreWolf also prompts for
// WebGL, which its visitors may take for the fix: allowing it does not make
// the desktop faster.
const ON_CPU = Object.freeze({
  firefox: 'Drawing the desktop on the CPU: WebGPU is off in this browser (dom.webgpu.enabled in about:config turns it on, for a faster desktop).',
  safari: 'Drawing the desktop on the CPU: this Safari has no WebGPU (Safari 26 and later have it, for a faster desktop).',
  chromium: 'Drawing the desktop on the CPU: this browser gave the page no WebGPU (hardware acceleration, or chrome://flags’ Unsafe WebGPU Support, turns it on).',
  other: 'Drawing the desktop on the CPU, without WebGPU: it works, more slowly.',
});

// What to check when WebGPU is on but no adapter came back; browsers not
// listed have the same reason as for turning it on.
const NO_ADAPTER = Object.freeze({
  firefox: 'Drawing the desktop on the CPU: WebGPU is on, but this browser offered no graphics adapter (gfx.webgpu.ignore-blocklist in about:config may help, and hardware acceleration under Settings › General › Performance).',
  chromium: 'Drawing the desktop on the CPU: WebGPU is on, but this browser offered no graphics adapter (hardware acceleration, or chrome://flags, may help).',
});

/** How long the page waits for the browser's adapter answer before it
 * goes on without one. A busy GPU process (several pages starting, a
 * software rasteriser compiling another page's pipelines) answers late, and
 * one that never answers left the page on "Loading Tilo…" for good. */
export const ADAPTER_ANSWER_MS = 10_000;
const LATE = Symbol('late');

/** [`pageRenderer`]'s answer, once asked. */
let cached;

/**
 * How this page draws Tilo: `{renderer: 'webgpu'}` through vello's compute
 * pipeline on the GPU, or `{renderer: 'cpu', reason}` with the scene replayed
 * on the CPU into the canvas (`web/cpu-present.mjs`), which is what a browser
 * with no WebGPU or no adapter gets. Never throws, and asks for an adapter
 * exactly as the renderer will (`__tiloAdapter` is the answer the entry page
 * already has, if it asked); an answer later than `answerMs` counts as
 * available, so a busy GPU process does not demote a page that has a GPU.
 */
export async function rendererChoice(env = globalThis, {answerMs = ADAPTER_ANSWER_MS} = {}) {
  const navigator = env.navigator ?? {};
  const family = browserFamily(navigator);
  if (!navigator.gpu) return {renderer: 'cpu', reason: ON_CPU[family]};
  let adapter = null;
  let timer;
  const late = new Promise(resolve => { timer = setTimeout(() => resolve(LATE), answerMs); });
  try {
    adapter = await Promise.race([env.__tiloAdapter ?? navigator.gpu.requestAdapter(), late]);
  } catch { /* Same as no adapter. */ } finally { clearTimeout(timer); }
  if (adapter === LATE) {
    console.warn(`WebGPU: no adapter answer after ${answerMs / 1000} s; drawing on the GPU`);
    return {renderer: 'webgpu'};
  }
  if (!adapter) return {renderer: 'cpu', reason: NO_ADAPTER[family] ?? ON_CPU[family]};
  return {renderer: 'webgpu'};
}

/** The renderer this page draws with, chosen once however many mounts ask:
 * the adapter request is the page's own, and its answer serves them all. */
export function pageRenderer(env = globalThis, options) {
  cached ??= rendererChoice(env, options);
  return cached;
}
