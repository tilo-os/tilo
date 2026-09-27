#!/usr/bin/env node
// Planning and manifest steps of build-web-app.sh.
//
//   side-module.mjs app PACKAGE REPO       -> {package, crate, dir, id, title, runtime}
//   cargo tree ... | side-module.mjs graph -> {"<root>": ["name@version", ...]}
//   side-module.mjs plan CORE_JSON CORE_GRAPH APP_JSON APP_GRAPH APP_INFO_JSON REPO RUNTIME CORE_ABI
//                                          -> {appRlib, link: [rlib...]}
//   side-module.mjs manifest OUT_DIR APP_INFO_JSON ABI ASSET_DIR
//
// `plan` splits the app's graph into the core's crates (whose units must be
// the very units the core linked: same Cargo metadata hash and, recomputed,
// the same ABI id) and the app's own, whose objects go into the side module.
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync} from 'node:fs';
import {extname, join, relative} from 'node:path';
import {fileURLToPath} from 'node:url';
import {CORE_PACKAGE, abiId, abiInputs, cargoArtifacts, cargoMessages, isTargetUnit, packageKey} from './abi.mjs';

const unitKey = artifact => `${artifact.package_id} ${artifact.target.name}`;
const rlibOf = artifact => artifact.filenames.find(name => name.endsWith('.rlib'));
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

/** The app package's crate and its tilo-app.toml identity. */
export function appInfo(packageName, repo) {
  const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--offline', '--format-version', '1'], {cwd: repo, encoding: 'utf8', maxBuffer: 1 << 28}));
  const pkg = metadata.packages.find(p => p.name === packageName);
  if (!pkg) throw Error(`no workspace package ${packageName}`);
  const lib = pkg.targets.find(target => target.kind.includes('lib') || target.kind.includes('rlib'));
  if (!lib) throw Error(`${packageName} has no library target; a component app exposes definition() from its lib`);
  const dir = pkg.manifest_path.replace(/\/Cargo\.toml$/, '');
  const manifestPath = join(dir, 'tilo-app.toml');
  if (!existsSync(manifestPath)) throw Error(`${packageName} has no tilo-app.toml`);
  const manifest = readFileSync(manifestPath, 'utf8');
  const packageTable = /^\[package\]\n([\s\S]*?)(?=^\[)/m.exec(manifest)?.[1] ?? '';
  const field = name => {
    const match = new RegExp(`^${name}\\s*=\\s*"([^"]*)"`, 'm').exec(packageTable);
    if (!match) throw Error(`tilo-app.toml of ${packageName} has no [package] ${name}`);
    return match[1];
  };
  return {package: packageName, crate: lib.name.replaceAll('-', '_'), dir, id: field('id'), title: field('name'), runtime: field('runtime')};
}

/** `cargo tree --prefix none --no-dedupe -f {p}` output: each root's packages as `name@version`. */
export function parseGraph(text) {
  const graph = {};
  let root;
  for (const line of text.split('\n')) {
    const match = /^(\S+) v(\S+)/.exec(line.trim());
    if (!match) {root = undefined; continue;}
    const key = `${match[1]}@${match[2]}`;
    if (!root) {root = match[1]; graph[root] = [];}
    if (!graph[root].includes(key)) graph[root].push(key);
  }
  return graph;
}

/**
 * The core's graph comes from the core build; the app's from its own build,
 * which compiled the core, the collection and the app in one invocation.
 */
export function plan({coreJson, coreGraph, appJson, appGraph, packageName, crate, repo, runtime, coreAbi, rustflags}) {
  const corePackages = new Set(coreGraph[CORE_PACKAGE]);
  const appPackages = new Set(appGraph[packageName]);
  if (!appPackages.size) throw Error(`no dependency graph for ${packageName}`);
  const targetUnits = json => cargoArtifacts(json).filter(a => isTargetUnit(a));
  const core = new Map(targetUnits(coreJson).filter(a => corePackages.has(packageKey(a.package_id))).map(a => [unitKey(a), a]));
  const mismatched = [], link = [];
  let app;
  for (const artifact of targetUnits(appJson)) {
    const key = packageKey(artifact.package_id);
    if (!appPackages.has(key)) continue;
    const rlib = rlibOf(artifact);
    if (corePackages.has(key)) {
      const coreUnit = core.get(unitKey(artifact));
      if (!coreUnit) mismatched.push(`${artifact.target.name}: not built by the core`);
      else if (rlibOf(coreUnit) !== rlib) {
        const extra = artifact.features.filter(f => !coreUnit.features.includes(f));
        mismatched.push(`${artifact.target.name}: [${extra.join(', ') || 'dependencies'}] beyond the core's`);
      }
      continue;
    }
    if (!link.includes(rlib)) link.push(rlib);
    if (artifact.target.name.replaceAll('-', '_') === crate && key.startsWith(`${packageName}@`)) app = rlib;
  }
  if (mismatched.length) {
    throw Error(`${packageName} needs crates of the core with features the core was built without. Add it to the web collection (TILO_WEB_COLLECTION, env.sh) and rebuild the core:\n  ${mismatched.join('\n  ')}`);
  }
  if (!app) throw Error(`the build of ${packageName} reported no library`);
  const nativeLibraries = cargoMessages(appJson)
    .filter(m => m.reason === 'build-script-executed' && appPackages.has(packageKey(m.package_id)) && !corePackages.has(packageKey(m.package_id)) && m.linked_libs?.length)
    .map(m => `${m.package_id}: ${m.linked_libs.join(' ')}`);
  if (nativeLibraries.length) throw Error(`side modules cannot link native libraries:\n  ${nativeLibraries.join('\n  ')}`);
  const seenAbi = abiId(abiInputs({cargoJson: appJson, repo, runtime, rustflags, packages: corePackages}));
  if (seenAbi !== coreAbi) {
    throw Error(`the core's crates changed since the core was built (its ABI ${coreAbi}, now ${seenAbi}); rebuild the core first`);
  }
  return {appRlib: app, link};
}

function filesUnder(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir).flatMap(name => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? filesUnder(path) : [path];
  });
}

/**
 * app.json next to app.wasm. Static assets are the files under the app's
 * `assets/web/`, stored content-addressed; the map names each by its path
 * below that directory.
 */
export function manifest({outDir, info, abi, assetDir}) {
  const module = readFileSync(join(outDir, 'app.wasm'));
  const assets = {};
  for (const file of filesUnder(assetDir).sort()) {
    const bytes = readFileSync(file);
    const path = `assets/${sha256(bytes)}${extname(file).toLowerCase()}`;
    mkdirSync(join(outDir, 'assets'), {recursive: true});
    copyFileSync(file, join(outDir, path));
    assets[relative(assetDir, file).split('\\').join('/')] = path;
  }
  return {
    schema: 1, id: info.id, title: info.title, package: info.package, runtime: info.runtime, abi,
    module: {path: 'app.wasm', bytes: module.length, sha256: sha256(module)},
    assets,
  };
}

function main([command, ...args]) {
  const read = path => readFileSync(path, 'utf8');
  if (command === 'app') {
    console.log(JSON.stringify(appInfo(args[0], args[1])));
  } else if (command === 'graph') {
    console.log(JSON.stringify(parseGraph(readFileSync(0, 'utf8'))));
  } else if (command === 'plan') {
    const [coreJson, coreGraph, appJson, appGraph, infoJson, repo, runtime, coreAbi] = args;
    const info = JSON.parse(infoJson);
    console.log(JSON.stringify(plan({
      coreJson: read(coreJson), coreGraph: JSON.parse(read(coreGraph)), appJson: read(appJson), appGraph: JSON.parse(read(appGraph)),
      packageName: info.package, crate: info.crate, repo, runtime, coreAbi, rustflags: process.env.RUSTFLAGS ?? '',
    })));
  } else if (command === 'manifest') {
    const [outDir, infoJson, abi, assetDir] = args;
    const result = manifest({outDir, info: JSON.parse(infoJson), abi, assetDir});
    writeFileSync(join(outDir, 'app.json'), JSON.stringify(result, null, 2) + '\n');
  } else {
    console.error('usage: side-module.mjs app|plan|manifest ...');
    process.exit(2);
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {main(process.argv.slice(2));} catch (error) {console.error(`error: ${error.message}`); process.exit(1);}
}
