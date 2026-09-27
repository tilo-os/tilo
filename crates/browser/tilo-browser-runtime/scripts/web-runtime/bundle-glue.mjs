#!/usr/bin/env node
// bundle-glue.mjs PKG_DIR GLUE: make wasm-bindgen's glue self-contained.
//
// The glue imports each `#[wasm_bindgen(module = ...)]` snippet by relative
// path (`./snippets/<crate>/<file>`), so it cannot run from an object URL
// or any source other than the directory it was generated into. Inline each
// snippet in place of its import and remove snippets/. A snippet that imports
// anything itself is refused: it would need a real bundler.
import {readFileSync, rmSync, writeFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

const IMPORT = /^import \{([^}]*)\} from '(\.\/snippets\/[^']+)';$/gm;

/** The snippet's source with its top-level exports made local, plus aliases. */
export function inlineSnippet(source, specifiers, name) {
  if (/^\s*import\b/m.test(source) || /\bimport\s*\(/.test(source)) throw Error(`${name} imports modules; it cannot be inlined`);
  if (/^\s*export\s+(default|\{|\*)/m.test(source)) throw Error(`${name} uses an export form the inliner does not handle`);
  const body = source.replace(/^export\s+(?=(async\s+)?(function|const|let|class)\b)/gm, '');
  const aliases = specifiers.split(',').map(s => s.trim()).filter(Boolean).flatMap(specifier => {
    const [imported, local] = specifier.split(/\s+as\s+/);
    return local && local !== imported ? [`const ${local} = ${imported};`] : [];
  });
  return [`// ---- inlined ${name}`, body.trimEnd(), ...aliases, `// ---- end ${name}`].join('\n');
}

export function bundleGlue(glue, readSnippet) {
  const seen = new Set();
  return glue.replace(IMPORT, (_, specifiers, path) => {
    if (seen.has(path)) throw Error(`${path} is imported twice`);
    seen.add(path);
    return inlineSnippet(readSnippet(path), specifiers, path);
  });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const [pkg, glueName] = process.argv.slice(2);
  const gluePath = join(pkg, glueName);
  const bundled = bundleGlue(readFileSync(gluePath, 'utf8'), path => readFileSync(join(dirname(gluePath), path), 'utf8'));
  if (/from '\.\//.test(bundled) || /import\('\.\//.test(bundled)) throw Error('the glue still imports by relative path');
  writeFileSync(gluePath, bundled);
  rmSync(join(pkg, 'snippets'), {recursive: true, force: true});
}
