// WebKit's WebGPU (Safari, and every browser on iOS/iPadOS) ran the desktop's
// vello compute frames into a GPU hang that froze macOS's WindowServer until
// the watchdog rebooted the machine. Until that is shown to be fixed, the
// runtime refuses to start the renderer there: a refusal loses nothing, a
// reboot loses the user's work in every application.
//
// `?unsafeWebKitGPU=1` on the page URL opts in, for controlled testing only.

export const WEBKIT_WEBGPU_OVERRIDE = 'unsafeWebKitGPU';
export const WEBKIT_WEBGPU_REFUSAL = "Safari isn't supported yet — use Chrome or Firefox. Its WebGPU can freeze and restart your Mac with this desktop.";

/** Whether the user agent is WebKit: Safari on macOS, or any browser on iOS/iPadOS. */
export function isWebKit({userAgent = '', vendor = '', maxTouchPoints = 0} = {}) {
  if (/\b(iPhone|iPad|iPod)\b/.test(userAgent)) return true;
  // iPadOS reports itself as a Mac; touch points tell them apart.
  if (/\bMacintosh\b/.test(userAgent) && maxTouchPoints > 1 && /AppleWebKit\//.test(userAgent)) return true;
  if (/AppleWebKit\//.test(userAgent) && !/\b(Chrome|Chromium|Edg|OPR|Firefox)\//.test(userAgent)) return true;
  // Chrome's user agent string can be overridden; the vendor cannot.
  return vendor === 'Apple Computer, Inc.' && !/\b(Chrome|Chromium|Edg|OPR|Firefox)\//.test(userAgent);
}

/** The refusal to show instead of starting WebGPU, or `undefined` when it is safe to start. */
export function webgpuRefusal(env = globalThis) {
  const navigator = env.navigator ?? {};
  if (!isWebKit(navigator)) return undefined;
  let search = '';
  try { search = env.location?.search ?? ''; } catch {}
  if (new URLSearchParams(search).get(WEBKIT_WEBGPU_OVERRIDE) === '1') return undefined;
  return WEBKIT_WEBGPU_REFUSAL;
}
