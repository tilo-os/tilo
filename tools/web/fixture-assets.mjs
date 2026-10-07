// The browser qualification driver reads the same verified fixture cache as Cargo.
import {execFileSync} from 'node:child_process';
import {readFileSync} from 'node:fs';
import {resolve} from 'node:path';

export function fixtureBytes(repository, manifest, filename) {
  const python = process.env.PYTHON || (process.platform === 'win32' ? 'python' : 'python3');
  const args = [resolve(repository, 'tools/test-assets.py'), resolve(repository, manifest), '--file', filename];
  if (process.env.CARGO_NET_OFFLINE === 'true') args.push('--offline');
  const paths = execFileSync(python, args, {encoding: 'utf8'}).trim().split(/\r?\n/).filter(Boolean);
  if (paths.length !== 1) throw Error('fixture resolver must return exactly one verified file');
  return readFileSync(paths[0]);
}
