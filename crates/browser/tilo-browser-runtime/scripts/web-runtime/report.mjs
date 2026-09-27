#!/usr/bin/env node
// report.mjs OUTPUT: the size table of a web runtime build, as Markdown.
import {existsSync, readdirSync, readFileSync} from 'node:fs';
import {join} from 'node:path';

const out = process.argv[2];
const build = join(out, 'build');
const read = path => JSON.parse(readFileSync(path, 'utf8'));
const mib = bytes => `${(bytes / 1048576).toFixed(2)} MiB`;
const kib = bytes => bytes >= 1048576 ? mib(bytes) : `${(bytes / 1024).toFixed(0)} KiB`;
const rows = [read(join(build, 'core-sizes.json'))];
const appsDir = join(build, 'apps');
if (existsSync(appsDir)) {
  for (const app of readdirSync(appsDir).sort()) {
    const sizes = join(appsDir, app, 'sizes.json');
    if (existsSync(sizes)) rows.push(read(sizes));
  }
}
const runtime = read(join(out, 'runtime.json'));
const lines = [
  `Runtime \`${runtime.runtime}\`, ABI \`${runtime.abi}\`${runtime.features.length ? `, features: ${runtime.features.join(' ')}` : ''}`,
  '',
  '| module | linked (raw) | linked (brotli) | -Oz (raw) | -Oz (brotli) |',
  '|---|---:|---:|---:|---:|',
  ...rows.map(row => `| ${row.module} | ${kib(row.linked.bytes)} | ${kib(row.linked.brotli)} | ${kib(row.final.bytes)} | ${kib(row.final.brotli)} |`),
];
const fonts = existsSync(join(build, 'fonts-sizes.json')) && read(join(build, 'fonts-sizes.json')).fonts;
if (fonts) {
  lines.push('', '| fonts | count | raw | brotli |', '|---|---:|---:|---:|',
    `| eager (mount barrier) | ${fonts.eager.count} | ${kib(fonts.eager.bytes)} | ${kib(fonts.eager.brotli)} |`,
    `| lazy (on text demand) | ${fonts.lazy.count} | ${kib(fonts.lazy.bytes)} | ${kib(fonts.lazy.brotli)} |`);
}
lines.push('', 'The core\'s linked stage is before wasm-bindgen; its -Oz stage is the shipped pkg/tilo_browser_runtime_bg.wasm.');
console.log(lines.join('\n'));
