/** Lowercase hexadecimal encoding of a byte sequence, two characters per byte. */
export const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
