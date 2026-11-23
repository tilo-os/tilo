/** Lowercase hexadecimal encoding of a byte sequence, two characters per byte. */
export const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');

function nibble(code) {
  if (code >= 0x30 && code <= 0x39) return code - 0x30;
  if (code >= 0x41 && code <= 0x46) return code - 0x37;
  if (code >= 0x61 && code <= 0x66) return code - 0x57;
  return -1;
}

/** Bytes decoded from pairs of ASCII hex digits; throws a `RangeError` for anything else. */
export function unhex(text) {
  if (typeof text !== 'string' || (text.length & 1) !== 0) {
    throw new RangeError('expected hex');
  }
  const bytes = new Uint8Array(text.length / 2);
  for (let index = 0; index < bytes.length; index++) {
    const high = nibble(text.charCodeAt(index * 2));
    const low = nibble(text.charCodeAt(index * 2 + 1));
    if (high < 0 || low < 0) throw new RangeError('expected hex');
    bytes[index] = (high << 4) | low;
  }
  return bytes;
}
