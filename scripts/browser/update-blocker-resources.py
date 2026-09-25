#!/usr/bin/env python3
"""Regenerate the pinned upstream resource bundle (Python 3.12+, Node 22+)."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'crates/browser/chromium-blocker/resources'
UBLOCK_REV = '4dfbb048607ec4fd19c1c252fd255df6cc653720'
UBLOCK_SHA = 'f40c26fee854a01fe654ff753799feee6c642e264084203ed84ded594509c84e'
BRAVE_REV = '6da9b247c3368baa8b4f24cd02046e18702a6527'
BRAVE_SHA = 'd085eb0754b7a54d9e1a18a0314b1946b066f61a15457de834d98beb53b4e40f'
LICENSE_SHA = '1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5'

def download(url, expected):
    data = urllib.request.urlopen(url, timeout=60).read(16*1024*1024)
    actual = hashlib.sha256(data).hexdigest()
    if actual != expected:
        raise ValueError(f'SHA256 mismatch for {url}: {actual}')
    return data

# adblock-rust 0.12.1 understands named function resources directly; dependency
# functions use fn/javascript, invokable scriptlets application/javascript.
CONVERT = r'''
import fs from 'node:fs';
import {pathToFileURL} from 'node:url';
const root=process.argv[2];
const {builtinScriptlets}=await import(pathToFileURL(root+'/src/js/resources/scriptlets.js'));
const {default:redirects}=await import(pathToFileURL(root+'/src/js/redirect-resources.js'));
const resources=[];
const skipped=[];
for(const script of builtinScriptlets) {
  const content=script.fn.toString();
  if(!script.name.endsWith('.fn') && !/^function\s+[\w$]+\s*\(/.test(content))throw Error('Unnamed scriptlet: '+script.name);
  const resource={name:script.name,aliases:script.aliases||[],kind:{mime:script.name.endsWith('.fn')?'fn/javascript':'application/javascript'},content:Buffer.from(content).toString('base64'),dependencies:script.dependencies||[]};
  if(script.requiresTrust)resource.permission=1;
  resources.push(resource);
}
const mime={js:'application/javascript',css:'text/css',html:'text/html',json:'application/json',xml:'text/xml',txt:'text/plain',gif:'image/gif',png:'image/png',mp3:'audio/mp3',mp4:'video/mp4'};
for(const [name,meta] of redirects) {
  if(meta.params) { skipped.push({name,reason:'URL-parameterized redirect is not supported by adblock-rust resources'});continue; }
  let content=fs.readFileSync(root+'/src/web_accessible_resources/'+name);
  const type=mime[name.split('.').pop()]||'application/octet-stream';
  if(['application/javascript','text/html','text/plain'].includes(type))content=Buffer.from(content.toString('utf8').replaceAll('\r',''));
  const aliases=typeof meta.alias==='string'?[meta.alias]:(meta.alias||[]);
  const resource={name,aliases,kind:{mime:type},content:content.toString('base64')};
  if(meta.requiresTrust)resource.permission=1;
  resources.push(resource);
}
process.stdout.write(JSON.stringify({resources,skipped}));
'''

def main():
    args = argparse.ArgumentParser(description=__doc__)
    args.add_argument('--node', default='node')
    args.add_argument('--check', action='store_true', help='verify the committed bundle reproduces exactly')
    args = args.parse_args()
    archive = download(f'https://codeload.github.com/brave/uBlock/tar.gz/{UBLOCK_REV}', UBLOCK_SHA)
    brave_root = f'https://raw.githubusercontent.com/brave/adblock-resources/{BRAVE_REV}/'
    custom = json.loads(download(brave_root+'dist/resources.json', BRAVE_SHA))
    mpl_license = download(brave_root+'LICENSE', LICENSE_SHA)
    with tempfile.TemporaryDirectory(prefix='qos-blocker-resources-') as temp:
        temp = Path(temp)
        with tarfile.open(fileobj=io.BytesIO(archive), mode='r:gz') as tar:
            tar.extractall(temp, filter='data')
        source = temp/f'uBlock-{UBLOCK_REV}'
        converter = temp/'convert.mjs'
        converter.write_text(CONVERT)
        assembled = json.loads(subprocess.check_output([args.node, str(converter), str(source)]))
        gpl_license = (source/'LICENSE.txt').read_bytes()
    resources = assembled['resources']+custom
    names = {item['name'] for item in resources}
    if len(names) != len(resources):
        raise ValueError('Duplicate primary resource names')
    for item in resources:
        missing = set(item.get('dependencies', []))-names
        if missing:
            raise ValueError(f'Missing dependencies for {item["name"]}: {missing}')
    resources.sort(key=lambda item:item['name'])
    bundle = (json.dumps(resources, ensure_ascii=False, separators=(',', ':'))+'\n').encode()
    metadata = {
        'ublock': {'repository':'https://github.com/brave/uBlock','revision':UBLOCK_REV,'archive_sha256':UBLOCK_SHA,'license':'GPL-3.0-or-later'},
        'brave': {'repository':'https://github.com/brave/adblock-resources','revision':BRAVE_REV,'resources_sha256':BRAVE_SHA,'license':'MPL-2.0'},
        'bundle_sha256':hashlib.sha256(bundle).hexdigest(), 'resource_count':len(resources),
        'skipped':assembled['skipped'],
        'trusted_resource_policy':'requiresTrust maps to permission mask 1; ordinary filter lists cannot invoke these resources',
    }
    outputs = {'resources.json':bundle,'provenance.json':(json.dumps(metadata,indent=2)+'\n').encode(),
               'LICENSE.uBlock.txt':gpl_license,'LICENSE.Brave.txt':mpl_license}
    OUT.mkdir(parents=True, exist_ok=True)
    for name, data in outputs.items():
        path = OUT/name
        if args.check:
            if path.read_bytes() != data:
                raise ValueError(f'Committed {path} differs from pinned generated output')
        else:
            path.write_bytes(data)
    print(f'{len(resources)} resources; bundle SHA256 {metadata["bundle_sha256"]}')
    print(f'Unsupported parameterized resources: {assembled["skipped"]}')

if __name__ == '__main__':
    main()
