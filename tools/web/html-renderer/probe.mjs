// Scratch probe: what the page did with a frame's text, for explaining a pixel
// difference between HTML and the canvas.
//
// The toolkit places each run and the page sets it in the same font file, so
// the interest is whether the browser's own advance for a run's characters is
// the toolkit's: for a run followed on the same line, the toolkit's advance
// is the step to the next run, and the page's is what the browser measures.
//
// usage: node tools/web/html-renderer/probe.mjs BUNDLE_DIR APP [--render dom] [--width N]
import {resolve} from 'node:path';
import {serve} from '../chrome.mjs';
import {open, sleep} from './engines.mjs';

const bundle = resolve(process.argv[2]);
const app = process.argv[3];
const option = name => { const at = process.argv.indexOf(name); return at < 0 ? null : process.argv[at + 1]; };
const render = option('--render') ?? 'dom';
const width = Number(option('--width')) || 1280;
const server = await serve(bundle);
const opened = await open({engine: 'chromium', width, height: 800});
const tab = opened.tab;
try {
  await tab.colourScheme('light');
  await tab.navigate(`${server.url}/app.html?app=${app}&render=${render}&theme=light`);
  await tab.waitFor('window.renderMarks.presented[0]', {timeout: 90000});
  for (let i = 0, last = -1; i < 40; i++) {
    await sleep(250);
    const n = await tab.evaluate('window.renderMarks.presented.length');
    if (n === last) break;
    last = n;
  }
  const TEXT = `(() => {
    const stats = window.__tiloDom?.stats;
    if (!stats?.ops) return JSON.stringify({error: 'no dom'});
    const ops = JSON.parse(stats.ops);
    const spans = [...document.querySelectorAll('.tilo-dom span')];
    // Every text run in order, with the span the page made for it.
    const runs = ops.filter(op => op[0] === 't').map((op, i) => ({op, span: spans[i]}));
    const out = [];
    runs.forEach((run, i) => {
      const [, text, x, y, size, family, variations] = run.op;
      const next = runs[i + 1];
      const sameLine = next && Math.abs(next.op[3] - y) < 0.5 && next.op[2] > x;
      const range = document.createRange();
      range.selectNodeContents(run.span);
      const measured = range.getBoundingClientRect().width;
      // Where the last character starts, from where the run starts: the same
      // measure as the toolkit's advance to its last glyph.
      const toLast = (() => {
        const node = run.span.firstChild;
        if (!node || node.nodeType !== 3 || node.length < 2) return null;
        const at = document.createRange();
        at.setStart(node, node.length - 1);
        at.setEnd(node, node.length);
        return at.getBoundingClientRect().left - range.getBoundingClientRect().left;
      })();
      const cs = getComputedStyle(run.span);
      out.push({
        i, text: text.slice(0, 24), size: Math.round(size * 100) / 100, family, variations,
        toolkitStep: sameLine ? Math.round((next.op[2] - x) * 100) / 100 : null,
        pageAdvance: Math.round(measured * 100) / 100,
        toolkitToLast: Math.round(run.op[8] * 100) / 100,
        pageToLast: toLast === null ? null : Math.round(toLast * 100) / 100,
        ratio: toLast === null || !run.op[8] ? null : Math.round((toLast / run.op[8]) * 10000) / 10000,
        css: [cs.fontFamily, cs.fontWeight, cs.fontStyle, cs.fontStretch, cs.fontSize,
          'ls=' + cs.letterSpacing, 'fs=' + cs.fontFeatureSettings, 'k=' + cs.fontKerning,
          'opsz=' + cs.fontOpticalSizing, 'syn=' + cs.fontSynthesis, 'var=' + cs.fontVariationSettings].join(' '),
      });
    });
    const faces = [...document.fonts].map(f => [f.family, f.weight, f.style, f.stretch, f.status]);
    return JSON.stringify({faces, runs: out}, null, 1);
  })()`;
  const data = JSON.parse(await tab.evaluate(TEXT));
  if (data.error) { console.log(data.error); }
  else {
    const ratios = data.runs.filter(run => run.ratio !== null).map(run => run.ratio);
    const sorted = [...ratios].sort((a, b) => a - b);
    console.log('runs', data.runs.length, 'with a following step', ratios.length,
      'ratio min', sorted[0], 'median', sorted[Math.floor(sorted.length / 2)], 'max', sorted.at(-1));
    const odd = data.runs.filter(run => run.ratio !== null && Math.abs(run.ratio - 1) > 0.002).slice(0, 12);
    console.log('runs whose last glyph sits elsewhere than the toolkit put it:');
    for (const run of odd) console.log(JSON.stringify(run));
    console.log('first runs:', JSON.stringify(data.runs.slice(0, 4), null, 1));
    console.log('faces:', JSON.stringify(data.faces.slice(0, 8)));
  }
} finally {
  await tab.close();
  await server.close();
}
