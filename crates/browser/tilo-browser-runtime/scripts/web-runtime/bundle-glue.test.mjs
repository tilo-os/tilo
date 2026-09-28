import test from 'node:test';
import assert from 'node:assert/strict';
import {bundleGlue} from './bundle-glue.mjs';

const glue = `import { writeClipboardText } from './snippets/tilo-browser-runtime-5b7977b0b55a5a77/web/clipboard.mjs';
import { a as b, c } from './snippets/x-1/web/two.mjs';
export function call() { return writeClipboardText('t', () => {}) + b() + c; }
`;
const snippets = {
  './snippets/tilo-browser-runtime-5b7977b0b55a5a77/web/clipboard.mjs': 'export function writeClipboardText(text, report) { return text; }\n',
  './snippets/x-1/web/two.mjs': 'export function a() { return 1; }\nexport const c = 2;\n',
};

test('snippet imports become inlined local declarations, so the glue imports nothing', async () => {
  const bundled = bundleGlue(glue, path => snippets[path]);
  assert.doesNotMatch(bundled, /from '\.\//);
  assert.match(bundled, /^function writeClipboardText/m);
  assert.match(bundled, /^const b = a;$/m);
  const url = `data:text/javascript,${encodeURIComponent(bundled)}`;
  const module = await import(url);
  assert.equal(module.call(), 't12');
  assert.equal(module.writeClipboardText, undefined, 'snippet exports stay private to the glue');
});

test('a snippet that imports modules itself is refused', () => {
  assert.throws(() => bundleGlue(glue, path => path.includes('two') ? "import x from './y.mjs';\nexport const c = x;" : snippets[path]), /imports modules/);
});
