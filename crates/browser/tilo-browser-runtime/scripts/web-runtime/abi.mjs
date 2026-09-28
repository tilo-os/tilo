#!/usr/bin/env node
// The web runtime ABI id: which side modules may be linked into which core.
//
//   abi.mjs CARGO_JSON GRAPH_JSON REPO RUNTIME_NAME > abi-inputs.json
//
// CARGO_JSON is the core build's `--message-format=json` output, GRAPH_JSON
// the package graph of each root (side-module.mjs graph). The id is a
// SHA-256 over everything that decides the core's symbols and data layout,
// and nothing that depends on the build machine:
//
//   - the compiler (`rustc -vV`, including its commit and LLVM version),
//   - RUSTFLAGS and the target configuration with the profile's panic
//     strategy (`rustc --print cfg`: target features, panic, ...),
//   - the release profile as declared in Cargo.toml and the
//     CARGO_PROFILE_RELEASE_* overrides env.sh applies to it,
//   - the Cargo.lock entries of the core's own packages (an app built in
//     another workspace adds entries of its own, which the core never links),
//   - every unit the core links except the core crate itself: its package,
//     features and Cargo's metadata hash (which encodes the package, its
//     features, profile and dependency graph, and is independent of the
//     checkout path), plus for workspace crates the digest of the source
//     files rustc read (from the unit's dep-info).
//
// The core crate (tilo-browser-runtime) is left out: apps never link against
// it except through `tilo_runtime_register_app`, whose contract is versioned
// by REGISTRATION below. Host changes therefore keep existing app modules.
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {existsSync, readFileSync} from 'node:fs';
import {join, relative, sep} from 'node:path';
import {fileURLToPath} from 'node:url';

/** Bump when `tilo_runtime_register_app` or the loader contract changes. */
export const REGISTRATION = 1;
export const CORE_PACKAGE = 'tilo-browser-runtime';
const sha256 = data => createHash('sha256').update(data).digest('hex');

export function cargoMessages(text) {
  return text.split('\n').filter(line => line.startsWith('{')).map(line => JSON.parse(line));
}

export function cargoArtifacts(text) {
  return cargoMessages(text).filter(message => message.reason === 'compiler-artifact');
}

/** Each package's build-script OUT_DIR, where it ran one. */
export function buildScriptOutDirs(text) {
  return new Map(cargoMessages(text).filter(message => message.reason === 'build-script-executed')
    .map(message => [message.package_id, message.out_dir]));
}

/** `name@version` of a Cargo package id, as `cargo tree` names packages too. */
export function packageKey(packageId) {
  const fragment = packageId.slice(packageId.lastIndexOf('#') + 1);
  if (fragment.includes('@')) return fragment;
  const base = packageId.slice(0, packageId.lastIndexOf('#')).replace(/\/+$/, '');
  return `${base.slice(base.lastIndexOf('/') + 1)}@${fragment}`;
}

/** An artifact compiled for the web target (not a build script or proc macro). */
export const isTargetUnit = (artifact, target = 'wasm32-unknown-unknown') =>
  artifact.filenames.some(name => name.includes(`/${target}/`) && name.endsWith('.rlib'));

/** Package name and repo-relative source of a Cargo package id. */
export function packageIdentity(packageId, repo) {
  // path+file:///abs/crates/x#name@0.1.0 | registry+https://...#name@1.2.3 | name 1.2.3 (source)
  const spec = /^(\w+)\+([^#]+)#(.+)$/.exec(packageId);
  if (!spec) return {id: packageId, workspace: false};
  const [, kind, source, version] = spec;
  if (kind === 'path') {
    const path = fileURLToPath(source);
    return {id: `path:${relative(repo, path).split(sep).join('/')}#${version}`, workspace: true, path};
  }
  return {id: `${kind}:${source}#${version}`, workspace: false};
}

/** Source files rustc read for an rlib, from its dep-info next to it. */
export function depInfoFiles(rlib) {
  const depInfo = rlib.replace(/\/lib([^/]+)\.rlib$/, '/$1.d');
  if (!existsSync(depInfo)) throw Error(`no dep-info for ${rlib}`);
  const [first] = readFileSync(depInfo, 'utf8').split('\n');
  const inputs = first.slice(first.indexOf(': ') + 2);
  return inputs.split(/(?<!\\) /).filter(Boolean).map(path => path.replaceAll('\\ ', ' '));
}

function sourceDigest(files, repo, outDir) {
  const hash = createHash('sha256');
  for (const file of [...new Set(files)].sort()) {
    let name;
    if (outDir && file.startsWith(outDir + '/')) name = `$OUT_DIR/${file.slice(outDir.length + 1)}`;
    else if (file.startsWith(repo + '/')) name = file.slice(repo.length + 1);
    else continue;
    hash.update(`${name}\0${sha256(readFileSync(file))}\n`);
  }
  return hash.digest('hex');
}

/** Digest of the `[[package]]` entries of Cargo.lock whose `name@version` is in `packages`. */
export function lockDigest(lock, packages) {
  const entries = lock.split(/^\[\[package\]\]\n/m).slice(1).filter(entry => {
    const name = /^name = "([^"]+)"/m.exec(entry)?.[1];
    const version = /^version = "([^"]+)"/m.exec(entry)?.[1];
    return packages.has(`${name}@${version}`);
  });
  return sha256(entries.map(entry => entry.trimEnd()).sort().join('\n'));
}

function profileText(manifest, name) {
  const match = new RegExp(`^\\[profile\\.${name}\\]\\n([\\s\\S]*?)(?=^\\[|(?![\\s\\S]))`, 'm').exec(manifest);
  if (!match) throw Error(`Cargo.toml declares no [profile.${name}]`);
  return match[1].split('\n').map(line => line.replace(/#.*/, '').trim()).filter(Boolean).join('\n');
}

/**
 * `packages` is the core's graph (`name@version`): the build that produced
 * `cargoJson` also compiled the app collection, whose own crates are not part
 * of the runtime ABI.
 */
export function abiInputs({cargoJson, repo, runtime, rustflags, packages, rustc = 'rustc', target = 'wasm32-unknown-unknown'}) {
  const units = [];
  const outDirs = buildScriptOutDirs(cargoJson);
  for (const artifact of cargoArtifacts(cargoJson)) {
    const rlib = artifact.filenames.find(name => name.endsWith('.rlib'));
    if (!rlib || !isTargetUnit(artifact, target) || !packages.has(packageKey(artifact.package_id))) continue;
    const identity = packageIdentity(artifact.package_id, repo);
    if (identity.workspace && artifact.target.name.replaceAll('-', '_') === CORE_PACKAGE.replaceAll('-', '_')) continue;
    const unit = {
      package: identity.id,
      target: artifact.target.name,
      features: [...artifact.features].sort(),
      metadata: /-([0-9a-f]+)\.rlib$/.exec(rlib)?.[1],
    };
    if (identity.workspace) {
      const outDir = outDirs.get(artifact.package_id);
      unit.source = sourceDigest(depInfoFiles(rlib), repo, outDir);
    }
    units.push(unit);
  }
  units.sort((a, b) => `${a.package} ${a.target}`.localeCompare(`${b.package} ${b.target}`));
  if (!units.length) throw Error('the core build reported no units');
  const manifest = readFileSync(join(repo, 'Cargo.toml'), 'utf8');
  const flags = rustflags.split(/\s+/).filter(Boolean);
  return {
    runtime,
    registration: REGISTRATION,
    target,
    rustc: execFileSync(rustc, ['-vV'], {cwd: repo, encoding: 'utf8'}).trim(),
    rustflags: flags,
    cfg: execFileSync(rustc, ['--print', 'cfg', '--target', target, '-C', 'panic=abort', ...flags], {cwd: repo, encoding: 'utf8'})
      .trim().split('\n').sort(),
    profile: {
      release: profileText(manifest, 'release'),
      overrides: Object.entries(process.env).filter(([name]) => name.startsWith('CARGO_PROFILE_RELEASE_'))
        .map(([name, value]) => `${name}=${value}`).sort(),
    },
    lock: lockDigest(readFileSync(join(repo, 'Cargo.lock'), 'utf8'), packages),
    units,
  };
}

/** `tilo-runtime/2026.07+wasm.<32 hex>`: the runtime name plus 128 bits of the digest. */
export function abiId(inputs) {
  return `${inputs.runtime}+wasm.${sha256(JSON.stringify(inputs)).slice(0, 32)}`;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const [cargoJsonPath, graphPath, repo, runtime] = process.argv.slice(2);
  const packages = new Set(JSON.parse(readFileSync(graphPath, 'utf8'))[CORE_PACKAGE]);
  const inputs = abiInputs({cargoJson: readFileSync(cargoJsonPath, 'utf8'), repo, runtime, packages, rustflags: process.env.RUSTFLAGS ?? ''});
  process.stdout.write(JSON.stringify({abi: abiId(inputs), inputs}, null, 2) + '\n');
}
