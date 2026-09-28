#!/usr/bin/env node
// sizes.mjs LABEL LINKED INTERMEDIATE FINAL: raw and brotli sizes of each
// stage of one module, as JSON.
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {brotliCompressSync, constants} from 'node:zlib';

export const brotli = bytes => brotliCompressSync(bytes, {
  params: {[constants.BROTLI_PARAM_QUALITY]: 11, [constants.BROTLI_PARAM_SIZE_HINT]: bytes.length},
}).length;

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const [label, linked, intermediate, final] = process.argv.slice(2);
  const stage = path => {const bytes = readFileSync(path); return {bytes: bytes.length, brotli: brotli(bytes)};};
  console.log(JSON.stringify({module: label, linked: stage(linked), intermediate: stage(intermediate), final: stage(final)}));
}
