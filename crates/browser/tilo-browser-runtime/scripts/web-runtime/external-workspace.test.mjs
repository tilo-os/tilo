import assert from 'node:assert/strict';
import test from 'node:test';
import {mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {memberManifest, rootManifest} from './external-workspace.mjs';

const dep = (name, fields = {}) => ({name, req: '*', kind: null, rename: null, optional: false,
  uses_default_features: true, features: [], target: null, source: null, path: null, registry: null, ...fields});

test('the member points Tilo crates into the mirror and keeps the rest', () => {
  const tilo = new Map([['tilo-app', 'crates/sdk/tilo-app'], ['runtime-dylib', 'crates/runtime/runtime-dylib']]);
  const pkg = {
    name: 'rt-demo', version: '0.1.0', edition: '2024',
    targets: [{kind: ['lib'], name: 'rt_demo', src_path: '/apps/rt-demo/src/lib.rs'},
      {kind: ['bin'], name: 'rt-demo', src_path: '/apps/rt-demo/src/main.rs'}],
    features: {thin: ['dep:runtime-dylib']},
    dependencies: [
      dep('tilo-app', {rename: 'ui', path: '/elsewhere/tilo/crates/sdk/tilo-app'}),
      dep('runtime-dylib', {optional: true, path: '/elsewhere/tilo/crates/runtime/runtime-dylib'}),
      dep('serde', {req: '^1', features: ['derive'], uses_default_features: false}),
      dep('left-pad', {source: 'git+https://example.com/left-pad?rev=abc#abc'}),
      dep('proptest', {kind: 'dev'}),
    ],
  };
  const manifest = memberManifest(pkg, tilo, '/work/workspace/external/rt-demo', '/work/workspace');
  assert.match(manifest, /^name = "rt-demo"$/m);
  assert.match(manifest, /^path = "\/apps\/rt-demo\/src\/lib.rs"$/m, 'sources stay where they are');
  assert.doesNotMatch(manifest, /main\.rs/, 'the web build is the library alone');
  assert.match(manifest, /^ui = \{ package = "tilo-app", path = "..\/..\/crates\/sdk\/tilo-app", default-features = false \}$/m,
    'the SDK is the portable toolkit from the mirror');
  assert.match(manifest, /^runtime-dylib = \{ path = "..\/..\/crates\/runtime\/runtime-dylib", optional = true \}$/m);
  assert.match(manifest, /^serde = \{ version = "\^1", default-features = false, features = \["derive"\] \}$/m);
  assert.match(manifest, /^left-pad = \{ git = "https:\/\/example.com\/left-pad", rev = "abc" \}$/m);
  assert.doesNotMatch(manifest, /proptest/, 'dev-dependencies are not built');
  assert.match(manifest, /^"thin" = \["dep:runtime-dylib"\]$/m);
});

test('the workspace is Tilo\'s with the app as one more member', () => {
  const tilo = mkdtempSync(join(tmpdir(), 'tilo-root-'));
  try {
    writeFileSync(join(tilo, 'Cargo.toml'), '[workspace]\nresolver = "3"\nmembers = [\n    "crates/sdk/tilo-app",\n]\n\n[patch.crates-io]\nvello = { path = "vendor/vello" }\n');
    const manifest = rootManifest(tilo, 'external/rt-demo');
    assert.match(manifest, /^members = \[\n    "external\/rt-demo",\n    "crates\/sdk\/tilo-app",\n\]$/m);
    assert.match(manifest, /^vello = \{ path = "vendor\/vello" \}$/m, 'Tilo\'s patches still apply');
  } finally {
    rmSync(tilo, {recursive: true, force: true});
  }
});
