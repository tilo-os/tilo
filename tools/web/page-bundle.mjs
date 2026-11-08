// The runtime module for a page test: what `serve` needs to answer a page
// module's `./pkg/tilo_browser_runtime.js` import when the page's sources are
// served straight from `crates/browser/tilo-browser-runtime/web/`.
//
// `pageBundle()` builds `pkg/` with the bundle script (one build path, the
// one a release takes) in the `quick` profile, and hands back the directory
// to serve it from. A run that already has a bundle names it with
// `TILO_PAGE_BUNDLE` and builds nothing (CI can build it once for every page
// test); `pageBundle()` then closes to a no-op.
import {spawnSync} from 'node:child_process';
import {existsSync, mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

const repo = join(dirname(fileURLToPath(import.meta.url)), '../..');

/** A built bundle, resolved to the directory that holds `pkg/`. */
export function existingPageBundle() {
  const named = process.env.TILO_PAGE_BUNDLE;
  if (!named) return null;
  if (!existsSync(join(named, 'pkg/tilo_browser_runtime.js'))) {
    throw new Error(`TILO_PAGE_BUNDLE (${named}) holds no pkg/tilo_browser_runtime.js`);
  }
  return named;
}

/**
 * `{dir, close}`: the directory to serve beside the page's sources, and the
 * call that removes it when this run built it.
 */
export function pageBundle({profile = 'quick'} = {}) {
  const existing = existingPageBundle();
  if (existing) return {dir: existing, close: () => {}};
  const dir = mkdtempSync(join(tmpdir(), 'tilo-page-bundle-'));
  const result = spawnSync(
    'bash',
    [join(repo, 'crates/browser/tilo-browser-runtime/scripts/build.sh'), dir, '--profile', profile, '--module-only'],
    {cwd: repo, stdio: 'inherit'},
  );
  if (result.status !== 0) {
    rmSync(dir, {recursive: true, force: true});
    throw new Error('the runtime module did not build (set TILO_PAGE_BUNDLE to a built bundle)');
  }
  return {dir, close: () => rmSync(dir, {recursive: true, force: true})};
}
