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
