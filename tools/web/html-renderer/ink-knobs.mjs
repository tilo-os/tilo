// Scratch: does a CSS text knob change the ink the page draws, and by how much
// against the canvas? Reads the ink of a text region from screenshots.
import {readFileSync, writeFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {serve} from '../chrome.mjs';
import {open, sleep} from './engines.mjs';
import {decode} from './png.mjs';

const bundle = resolve(process.argv[2]);
const app = process.argv[3] ?? 'so.tilo.ebook';
const region = (process.argv[4] ?? '180,55,700,350').split(',').map(Number);
const CSS = [
  ['as is', ''],
  ['geometricPrecision', '.tilo-dom span,.tilo-dom div{text-rendering:geometricPrecision}'],
];

const ink = (png, [x0, y0, w, h]) => {
  const p = decode(png);
  const counts = new Map();
  for (let y = y0; y < y0 + h; y++) for (let x = x0; x < x0 + w; x++) {
    const i = (y * p.width + x) * 4;
    const k = (p.pixels[i] << 16) | (p.pixels[i + 1] << 8) | p.pixels[i + 2];
    counts.set(k, (counts.get(k) ?? 0) + 1);
  }
  const ground = [...counts].sort((a, b) => b[1] - a[1])[0][0];
  const g = ((ground >> 16 & 255) * 0.2126 + (ground >> 8 & 255) * 0.7152 + (ground & 255) * 0.0722);
  let total = 0, count = 0, dark = 0, lo = 1e9, hi = -1;
  for (let y = y0; y < y0 + h; y++) for (let x = x0; x < x0 + w; x++) {
    const i = (y * p.width + x) * 4;
    const v = p.pixels[i] * 0.2126 + p.pixels[i + 1] * 0.7152 + p.pixels[i + 2] * 0.0722;
    const d = Math.max(0, g - v);
    total += d;
    if (d > 8) count++;
    if (d > 100) dark++;
    if (d > 40 && x < lo) lo = x;
    if (d > 40 && x > hi) hi = x;
  }
  return {ink: Math.round(total), count, dark, extent: [lo, hi, hi - lo + 1]};
};

const server = await serve(bundle);
const shot = async render => {
  const gpuArgs = process.platform === 'linux' ? ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader'] : [];
  const opened = await open({engine: 'chromium', width: 1280, height: 800, args: gpuArgs});
  try {
    await opened.tab.colourScheme('light');
    await opened.tab.navigate(`${server.url}/app.html?app=${app}&render=${render}&theme=light`);
    await opened.tab.waitFor('window.renderMarks.presented[0]', {timeout: 90000});
    for (let i = 0, last = -1; i < 30; i++) {
      await sleep(250);
      const n = await opened.tab.evaluate('window.renderMarks.presented.length');
      if (n === last) break;
      last = n;
    }
    return opened;
  } catch (error) { await opened.tab.close(); throw error; }
};
try {
  const canvas = await shot('gpu');
  const canvasPng = await canvas.tab.screenshot();
  await canvas.tab.close();
  writeFileSync('/tmp/knob-gpu.png', canvasPng);
  const canvasInk = ink(canvasPng, region);
  console.log('canvas', JSON.stringify(canvasInk));
  const dom = await shot('dom');
  for (const [name, css] of CSS) {
    await dom.tab.evaluate(`(() => {
      let style = document.getElementById('probe-css');
      if (!style) { style = document.createElement('style'); style.id = 'probe-css'; document.head.append(style); }
      style.textContent = ${JSON.stringify(css)};
      return true;
    })()`);
    await sleep(1200);
    const png = await dom.tab.screenshot();
    const measured = ink(png, region);
    console.log(name.padEnd(22), JSON.stringify(measured), 'ink vs canvas', (measured.ink / canvasInk.ink).toFixed(4), 'dark vs canvas', (measured.dark / canvasInk.dark).toFixed(4));
  }
  await dom.tab.close();
} finally {
  await server.close();
}
