#!/usr/bin/env node
// fonts.mjs FONT_SOURCE OUTPUT: publish the canonical product fonts as
// content-addressed assets. Each font is stored once as
// fonts/<sha256>.<ext>, so a host can cache it immutably and share it across
// runtime releases; fonts.json maps the name Rust demands to that path.
// Startup fonts form the mount barrier; every other font (CJK, emoji, math,
// symbols and the other scripts) is fetched only when text demands it.
import {cpSync, copyFileSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {extname, join} from 'node:path';
import {brotli} from './sizes.mjs';

const [source, out] = process.argv.slice(2);
const manifest = readFileSync(join(source, 'manifest.toml'), 'utf8');
const field = (block, name) => new RegExp(`^${name} = (.*)$`, 'm').exec(block)?.[1];
const assets = [];
mkdirSync(join(out, 'fonts'), {recursive: true});
for (const block of manifest.split('[[font]]').slice(1).map(block => block.split('[[license]]')[0])) {
  if (field(block, 'scope') !== '"product"') continue;
  const name = JSON.parse(field(block, 'file'));
  const bytes = readFileSync(join(source, name));
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  if (sha256 !== JSON.parse(field(block, 'sha256')) || bytes.length !== Number(field(block, 'bytes'))) {
    throw Error(`${name} does not match its manifest entry`);
  }
  const path = `fonts/${sha256}${extname(name).toLowerCase()}`;
  copyFileSync(join(source, name), join(out, path));
  assets.push({name, path, bytes: bytes.length, sha256, startup: field(block, 'browser_startup') === 'true', brotli: brotli(bytes)});
}
if (!assets.some(asset => asset.startup)) throw Error('no browser startup font');
cpSync(join(source, 'LICENSES'), join(out, 'fonts/LICENSES'), {recursive: true});
copyFileSync(join(source, 'manifest.toml'), join(out, 'fonts/manifest.toml'));
const catalog = JSON.parse(field(manifest, 'catalog_id'));
writeFileSync(join(out, 'fonts.json'), JSON.stringify({
  schema: 1, catalog, assets: assets.map(({brotli: _, ...asset}) => asset),
}) + '\n');
const total = (startup, key) => assets.filter(asset => asset.startup === startup).reduce((n, asset) => n + asset[key], 0);
console.log(JSON.stringify({fonts: {
  eager: {count: assets.filter(a => a.startup).length, bytes: total(true, 'bytes'), brotli: total(true, 'brotli')},
  lazy: {count: assets.filter(a => !a.startup).length, bytes: total(false, 'bytes'), brotli: total(false, 'brotli')},
}}));
