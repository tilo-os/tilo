#!/usr/bin/env node
// A workspace in which an app outside the Tilo tree builds as a web side module.
//
//   external-workspace.mjs TILO_ROOT APP_DIR WORKSPACE -> {"package": "<name>", "workspace": "<dir>"}
//
// A side module links against the web core by symbol name, and a crate's
// symbols carry its Cargo metadata hash, which covers its path relative to the
// workspace root. So the app is built as a member of a workspace that mirrors
// the Tilo tree the core was built from: every top-level entry of TILO_ROOT is
// linked in as it is, Cargo.toml is Tilo's with one more member, Cargo.lock is
// Tilo's (Cargo adds the app's own packages), and the member is the app's
// package with its Tilo dependencies pointed at the mirror. Its sources stay
// where they are.
//
// The member builds for wasm32 only: `tilo-app` is the portable toolkit there
// (no default features), as in every web app of the Tilo tree.
import {execFileSync} from 'node:child_process';
import {copyFileSync, existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync} from 'node:fs';
import {join, relative} from 'node:path';

const LOCAL = new Set(['Cargo.toml', 'Cargo.lock', 'target', '.git', 'external']);
const SDK = 'tilo-app';
const str = value => JSON.stringify(value);

function metadata(manifestPath) {
  return JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--offline', '--format-version', '1', '--manifest-path', manifestPath],
    {encoding: 'utf8', maxBuffer: 1 << 28}));
}

function mirror(tiloRoot, workspace) {
  mkdirSync(workspace, {recursive: true});
  for (const name of readdirSync(tiloRoot)) {
    if (LOCAL.has(name)) continue;
    const link = join(workspace, name);
    if (existsSync(link) || lstatSync(link, {throwIfNoEntry: false})) rmSync(link, {recursive: true, force: true});
    symlinkSync(join(tiloRoot, name), link);
  }
}

export function rootManifest(tiloRoot, member) {
  const manifest = readFileSync(join(tiloRoot, 'Cargo.toml'), 'utf8');
  const members = /^members\s*=\s*\[/m;
  if (!members.test(manifest)) throw Error(`${tiloRoot}/Cargo.toml has no [workspace] members list`);
  return manifest.replace(members, match => `${match}\n    ${str(member)},`);
}

/** A dependency as an inline TOML table, its Tilo crates resolved inside the mirror. */
export function dependency(dep, tiloMembers, memberDir, workspace) {
  const fields = [];
  if (dep.rename) fields.push(`package = ${str(dep.name)}`);
  const tilo = tiloMembers.get(dep.name);
  if (tilo) fields.push(`path = ${str(relative(memberDir, join(workspace, tilo)))}`);
  else if (dep.path) fields.push(`path = ${str(dep.path)}`);
  else if (dep.source?.startsWith('git+')) {
    const url = new URL(dep.source.slice(4));
    const rev = url.hash.slice(1) || url.searchParams.get('rev');
    url.hash = ''; url.search = '';
    fields.push(`git = ${str(url.href)}`);
    if (rev) fields.push(`rev = ${str(rev)}`);
  } else {
    fields.push(`version = ${str(dep.req)}`);
    if (dep.registry) fields.push(`registry = ${str(dep.registry)}`);
  }
  const portableSdk = dep.name === SDK && !dep.target;
  if (portableSdk || !dep.uses_default_features) fields.push('default-features = false');
  if (dep.features.length) fields.push(`features = ${str(dep.features)}`);
  if (dep.optional) fields.push('optional = true');
  return `${dep.rename ?? dep.name} = { ${fields.join(', ')} }`;
}

export function memberManifest(pkg, tiloMembers, memberDir, workspace) {
  const lib = pkg.targets.find(target => target.kind.some(kind => ['lib', 'rlib'].includes(kind)));
  if (!lib) throw Error(`${pkg.name} has no library target; its web build registers the definition() its lib exports`);
  const build = pkg.targets.find(target => target.kind.includes('custom-build'));
  const lines = ['[package]', `name = ${str(pkg.name)}`, `version = ${str(pkg.version)}`, `edition = ${str(pkg.edition)}`, 'publish = false'];
  if (build) lines.push(`build = ${str(build.src_path)}`);
  lines.push('', '[lib]', `name = ${str(lib.name)}`, `path = ${str(lib.src_path)}`, '');
  lines.push('[features]');
  for (const [name, enables] of Object.entries(pkg.features)) lines.push(`${str(name)} = ${str(enables)}`);
  const tables = new Map();
  for (const dep of pkg.dependencies) {
    if (dep.kind === 'dev') continue;
    const table = `${dep.target ? `target.${str(dep.target)}.` : ''}${dep.kind === 'build' ? 'build-dependencies' : 'dependencies'}`;
    if (!tables.has(table)) tables.set(table, []);
    tables.get(table).push(dependency(dep, tiloMembers, memberDir, workspace));
  }
  for (const [table, deps] of tables) lines.push('', `[${table}]`, ...deps);
  return lines.join('\n') + '\n';
}

export function externalWorkspace(tiloRoot, appDir, workspace) {
  tiloRoot = realpathSync(tiloRoot);
  appDir = realpathSync(appDir);
  if (appDir === tiloRoot || appDir.startsWith(tiloRoot + '/')) throw Error(`${appDir} is inside the Tilo tree; build it there`);
  const manifestPath = join(appDir, 'Cargo.toml');
  const pkg = metadata(manifestPath).packages.find(p => realpathSync(p.manifest_path) === manifestPath);
  if (!pkg) throw Error(`${manifestPath} declares no package`);
  const tilo = metadata(join(tiloRoot, 'Cargo.toml'));
  const tiloMembers = new Map(tilo.packages.filter(p => tilo.workspace_members.includes(p.id))
    .map(p => [p.name, relative(tiloRoot, p.manifest_path.replace(/\/Cargo\.toml$/, ''))]));
  if (tiloMembers.has(pkg.name)) throw Error(`${pkg.name} is also the name of a Tilo crate; rename the package`);
  mirror(tiloRoot, workspace);
  const member = `external/${pkg.name}`;
  const memberDir = join(workspace, member);
  rmSync(memberDir, {recursive: true, force: true});
  mkdirSync(memberDir, {recursive: true});
  writeFileSync(join(memberDir, 'Cargo.toml'), memberManifest(pkg, tiloMembers, memberDir, workspace));
  // side-module.mjs reads the app's identity and web assets next to its manifest.
  for (const name of ['tilo-app.toml', 'assets']) {
    if (existsSync(join(appDir, name))) symlinkSync(join(appDir, name), join(memberDir, name));
  }
  writeFileSync(join(workspace, 'Cargo.toml'), rootManifest(tiloRoot, member));
  copyFileSync(join(tiloRoot, 'Cargo.lock'), join(workspace, 'Cargo.lock'));
  return {package: pkg.name, workspace};
}

const [tiloRoot, appDir, workspace] = process.argv.slice(2);
if (tiloRoot && appDir && workspace) {
  process.stdout.write(JSON.stringify(externalWorkspace(tiloRoot, appDir, workspace)) + '\n');
}
