// A Discord person's avatar, kept in the workspace's content store so a call's presence can
// carry it as a hash rather than an address only Discord serves.
//
// The image comes from Discord's CDN, which the package grant's domain entries already allow.
// The host names content by its BLAKE3 hash, and the call report wants that bare hash: the
// `tilo.content:<hash>` reference the upload answers is trimmed to it.

/// The one host an avatar is fetched from.
export const AVATAR_HOST = "cdn.discordapp.com";
/// How large an avatar may be; Discord serves a 128px picture far below this.
export const AVATAR_MAX_BYTES = 2 * 1024 * 1024;
/// The image types a profile picture may be.
const PICTURE_TYPES = ["image/png", "image/jpeg", "image/webp"];
/// How long an avatar may take to arrive before presence goes without it.
const AVATAR_TIMEOUT_MS = 5_000;

/// The address an avatar is fetched from, or null when `url` is not Discord's CDN.
export function avatarAddress(url) {
  let parsed;
  try {
    parsed = new URL(String(url ?? ""));
  } catch {
    return null;
  }
  if (parsed.protocol !== "https:" || parsed.hostname !== AVATAR_HOST) return null;
  return parsed.href;
}

/// The bare content hash of an upload answer (`tilo.content:<hash>`), or null when the answer
/// names no content.
export function contentHash(answer) {
  const text = String(answer ?? "");
  const bare = text.startsWith("tilo.content:") ? text.slice("tilo.content:".length) : text;
  return /^[0-9a-f]{64}$/.test(bare) ? bare : null;
}

/// The picture `url` serves, `{bytes, type}`, or null when it is not an avatar this bridge may
/// fetch or is larger than `max`.
export async function fetchAvatar(url, { fetch = globalThis.fetch, max = AVATAR_MAX_BYTES } = {}) {
  const address = avatarAddress(url);
  if (!address) return null;
  let answer;
  try {
    const signal =
      typeof AbortSignal?.timeout === "function" ? AbortSignal.timeout(AVATAR_TIMEOUT_MS) : undefined;
    answer = await fetch(address, {
      headers: { accept: "image/png,image/jpeg,image/webp" },
      signal,
    });
  } catch {
    return null;
  }
  if (!answer || Number(answer.status) !== 200) return null;
  const type = String(answer.headers?.get?.("content-type") ?? "")
    .split(";")[0]
    .trim()
    .toLowerCase();
  if (!PICTURE_TYPES.includes(type)) return null;
  const declared = Number(answer.headers?.get?.("content-length") ?? 0);
  if (declared > max) return null;
  let bytes;
  try {
    bytes = new Uint8Array(await answer.arrayBuffer());
  } catch {
    return null;
  }
  if (bytes.length === 0 || bytes.length > max) return null;
  return { bytes, type };
}

/// Fetch `url` into the workspace's content store with `content(bytes)` and answer the hash of
/// the picture, or null when it is not an avatar this bridge may keep.
export async function keepAvatar(
  url,
  { fetch = globalThis.fetch, content, max = AVATAR_MAX_BYTES } = {},
) {
  if (!content) return null;
  const picture = await fetchAvatar(url, { fetch, max });
  if (!picture) return null;
  try {
    return contentHash(await content(picture.bytes));
  } catch {
    return null;
  }
}

/// How large a link's picture may be as the record keeps it: a base64 `data:` URL the record
/// takes up to 48 KiB of.
export const PHOTO_MAX_BYTES = 36 * 1024;

/// A linked account's avatar as the small picture its link keeps, a base64 `data:` URL, or null
/// when it is not an avatar this bridge may keep or is too large for the record.
export async function linkPhoto(url, { fetch = globalThis.fetch } = {}) {
  const picture = await fetchAvatar(url, { fetch, max: PHOTO_MAX_BYTES });
  if (!picture) return null;
  return `data:${picture.type};base64,${Buffer.from(picture.bytes).toString("base64")}`;
}
