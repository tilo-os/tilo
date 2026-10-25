/**
 * The picture a person shows beside their name.
 *
 * It is part of their profile (`root-identity.mjs` `publishProfile`): the
 * hosts of their rooms keep it signed by their root, beside their name, so
 * each of their devices and everyone in those rooms shows the same one.
 * Chosen here from an image file, cropped to its centre square and scaled
 * to `PHOTO_SIDE` pixels, it travels as a small `data:` URL. The host's
 * answer to `/here` says when each person last changed their profile
 * (`profile_ms`); a page fetches it again then, decodes the picture and
 * hands its pixels to the desktop, whose avatars wear it from the next
 * frame (`desktop_set_person_photo`).
 */

/** The side, in pixels, of the square a picture is kept at. */
export const PHOTO_SIDE = 96;
/** The longest picture a host keeps, as its `data:` URL. */
export const MAX_PHOTO_CHARS = 48 * 1024;
const TYPES = ['image/webp', 'image/jpeg'];

/** Ask the person for an image file; resolves to it, or `null` if they
 * chose none. Called inside their gesture, so the browser opens its chooser. */
export function pickImageFile({document = globalThis.document} = {}) {
  return new Promise(resolve => {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = 'image/*';
    input.style.display = 'none';
    let settled = false;
    const done = file => {
      if (settled) return;
      settled = true;
      input.remove();
      resolve(file ?? null);
    };
    input.addEventListener('change', () => done(input.files?.[0]));
    input.addEventListener('cancel', () => done(null));
    document.body.append(input);
    input.click();
  });
}

function canvasOf(side, document = globalThis.document) {
  if (typeof OffscreenCanvas === 'function') return new OffscreenCanvas(side, side);
  const canvas = document.createElement('canvas');
  canvas.width = side;
  canvas.height = side;
  return canvas;
}

async function encode(canvas, type, quality) {
  if (typeof canvas.convertToBlob === 'function') {
    const blob = await canvas.convertToBlob({type, quality});
    if (blob.type !== type) return null;
    const bytes = new Uint8Array(await blob.arrayBuffer());
    let binary = '';
    for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    return `data:${type};base64,${btoa(binary)}`;
  }
  const url = canvas.toDataURL(type, quality);
  return url.startsWith(`data:${type};`) ? url : null;
}

/**
 * The centre square of `image` (a `Blob` or `File`), scaled to `side`
 * pixels, as a `data:` URL a profile keeps: WebP where the browser writes
 * it, else JPEG, at the best quality that fits `MAX_PHOTO_CHARS`.
 * Rejects what is not an image the browser can read.
 */
export async function squarePhoto(image, {side = PHOTO_SIDE} = {}) {
  const bitmap = await createImageBitmap(image);
  try {
    const crop = Math.min(bitmap.width, bitmap.height);
    if (!crop) throw new Error('That image is empty');
    const canvas = canvasOf(side);
    const context = canvas.getContext('2d');
    context.imageSmoothingEnabled = true;
    context.imageSmoothingQuality = 'high';
    context.drawImage(bitmap, (bitmap.width - crop) / 2, (bitmap.height - crop) / 2, crop, crop, 0, 0, side, side);
    for (const type of TYPES) {
      for (const quality of [0.9, 0.8, 0.65, 0.5]) {
        const url = await encode(canvas, type, quality);
        if (!url) break;
        if (url.length <= MAX_PHOTO_CHARS) return url;
      }
    }
    throw new Error('That image could not be made small enough');
  } finally {
    bitmap.close?.();
  }
}

/** The pixels of a profile picture (`data:` URL): `{width, height, rgba}`. */
export async function photoPixels(url) {
  const match = /^data:(image\/(?:png|jpeg|webp));base64,([A-Za-z0-9+/=]+)$/.exec(url ?? '');
  if (!match) throw new Error('not a profile picture');
  const binary = atob(match[2]);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  const bitmap = await createImageBitmap(new Blob([bytes], {type: match[1]}));
  try {
    const width = Math.min(bitmap.width, 256);
    const height = Math.min(bitmap.height, 256);
    const canvas = typeof OffscreenCanvas === 'function'
      ? new OffscreenCanvas(width, height)
      : Object.assign(globalThis.document.createElement('canvas'), {width, height});
    const context = canvas.getContext('2d');
    context.drawImage(bitmap, 0, 0, width, height);
    const {data} = context.getImageData(0, 0, width, height);
    return {width, height, rgba: new Uint8Array(data.buffer, data.byteOffset, data.byteLength)};
  } finally {
    bitmap.close?.();
  }
}

/** The profile of `user` (root key, hex) the host at `origin` keeps, or `null`. */
export async function fetchPersonProfile(origin, user, fetcher = (...args) => globalThis.fetch(...args)) {
  const response = await fetcher(new URL('/peer/v1/profile/fetch', origin).href, {
    method: 'POST', headers: {'content-type': 'text/plain'}, body: user, credentials: 'omit',
  });
  if (!response.ok) return null;
  const profile = (await response.json())?.profile ?? null;
  return profile?.user_public_key === user ? profile : null;
}

/**
 * Follow the pictures of the people in `/here`'s lines: `seen(lines)` with
 * each answer fetches the profile of anyone whose `profile_ms` moved
 * (`profileOf(user)`) and shows their picture (`show(user, url | null)`).
 */
export function followPhotos({profileOf, show, onError = () => {}}) {
  const stamps = new Map(); // user -> profile_ms fetched
  const shown = new Set(); // users with a picture shown
  async function refresh(user, stamp) {
    try {
      const profile = await profileOf(user);
      if (stamps.get(user) !== stamp) return;
      const photo = typeof profile?.photo === 'string' ? profile.photo : null;
      if (!photo && !shown.has(user)) return;
      await show(user, photo);
      if (photo) shown.add(user); else shown.delete(user);
    } catch (error) {
      stamps.delete(user);
      onError(error);
    }
  }
  return {
    seen(lines) {
      for (const line of Array.isArray(lines) ? lines : []) {
        const user = typeof line?.user === 'string' ? line.user : '';
        const stamp = Number(line?.profile_ms) || 0;
        if (!/^[0-9a-f]{64}$/.test(user) || stamps.get(user) === stamp) continue;
        stamps.set(user, stamp);
        if (stamp || shown.has(user)) void refresh(user, stamp);
      }
    },
    /** `user` changed their picture here: show it at once, as kept at `stamp`. */
    chose(user, url) {
      stamps.delete(user);
      if (url) shown.add(user); else shown.delete(user);
    },
  };
}
