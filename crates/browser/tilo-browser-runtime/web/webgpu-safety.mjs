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
 * Which browser's settings a visitor without WebGPU would change. `firefox`
 * covers its forks too: LibreWolf resists fingerprinting and sends Firefox's
 * user agent unchanged, so the steps name both.
 */
export function browserFamily({userAgent = '', vendor = '', maxTouchPoints = 0} = {}) {
  if (/\bFirefox\//.test(userAgent)) return 'firefox';
  if (isWebKit({userAgent, vendor, maxTouchPoints})) return 'safari';
  if (/\b(Chrome|Chromium|Edg|OPR)\//.test(userAgent)) return 'chromium';
  return 'other';
}

// What turns WebGPU on, per browser. Firefox and its forks have no per-site
// WebGPU prompt: with `dom.webgpu.enabled` off (LibreWolf's default, and
// Firefox's on Linux) the page has no `navigator.gpu` to ask through, so it
// names the setting. LibreWolf does prompt for WebGL, which pages that fall
// back from WebGPU to WebGL trigger; allowing it does not help this renderer.
const ENABLE = Object.freeze({
  firefox: [
    'In Firefox or LibreWolf, open about:config in a new tab and accept the warning.',
    'Set dom.webgpu.enabled to true, then press Try again.',
    'A prompt to allow WebGL is not the same: Tilo needs WebGPU.',
  ],
  safari: ['Update to Safari 26 or later, where WebGPU is on.', 'On an older Safari, turn on Develop › Feature Flags › WebGPU.'],
  chromium: ['Check that hardware acceleration is on, in the browser’s system settings.', 'On Linux, if it stays off: open chrome://flags, enable “Unsafe WebGPU Support”, and relaunch.'],
  other: ['Open this page in a current Chrome, Edge, Firefox or Safari.'],
});

// What to check when WebGPU is on but no adapter came back; browsers not
// listed have the same steps as for turning it on.
const NO_ADAPTER = Object.freeze({
  firefox: [
    'Check that hardware acceleration is on, under Settings › General › Performance.',
    'If your graphics card is blocklisted for WebGPU, set gfx.webgpu.ignore-blocklist to true in about:config, then press Try again.',
  ],
});

/**
 * Why this browser cannot draw Tilo, or `undefined` when it can:
 * `{reason, steps}`, the steps saying how to turn WebGPU on here. The desktop
 * renders with Vello's WebGPU compute pipeline and has no CPU or WebGL
 * fallback, so this asks for an adapter as the renderer will, before the page
 * downloads the runtime or takes a seat. Never throws.
 */
export async function rendererUnavailable(env = globalThis) {
  const refusal = webgpuRefusal(env);
  if (refusal) return {reason: refusal, steps: []};
  const navigator = env.navigator ?? {};
  const family = browserFamily(navigator);
  if (!navigator.gpu) return {reason: 'Tilo draws with WebGPU, which is turned off in this browser.', steps: ENABLE[family]};
  let adapter = null;
  try { adapter = await navigator.gpu.requestAdapter(); } catch { /* Same as no adapter. */ }
  if (!adapter) {
    return {
      reason: 'Tilo draws with WebGPU. This browser has it, but offered this page no graphics adapter.',
      steps: NO_ADAPTER[family] ?? ENABLE[family],
    };
  }
  return undefined;
}
