#!/usr/bin/env python3
"""Project registry-only dependencies, then verify Cargo's private lock pruning.

The crate is projected as the kernel builds it, without default features: its
optional dependencies and the features naming them are left out, so only what
that build links is resolved. Path/git dependencies (including reachable
workspace patches) are unsupported and rejected. Unused workspace patches, such
as one behind an optional dependency, do not affect this standalone crate.
The root lockfile is never modified or regenerated.
"""
import argparse
import copy
import json
from pathlib import Path
import shutil
import tomllib


def toml(value):
    if isinstance(value, str): return json.dumps(value, ensure_ascii=False)
    if isinstance(value, bool): return 'true' if value else 'false'
    if isinstance(value, (int, float)): return repr(value)
    if isinstance(value, list): return '['+', '.join(toml(x) for x in value)+']'
    if isinstance(value, dict): return '{'+', '.join(toml(k)+' = '+toml(v) for k,v in value.items())+'}'
    raise ValueError('unsupported TOML value')


def project_manifest(root, member):
    result = copy.deepcopy(member)
    workspace = root['workspace']
    for key,value in result['package'].items():
        if isinstance(value,dict) and value.get('workspace'):
            result['package'][key]=copy.deepcopy(workspace['package'][key])
    def dependencies(table):
        for group in ('dependencies','dev-dependencies','build-dependencies'):
            for name,value in table.get(group,{}).items():
                if isinstance(value,dict) and value.get('workspace'):
                    inherited=copy.deepcopy(workspace['dependencies'][name])
                    if isinstance(inherited,str):inherited={'version':inherited}
                    extra={k:v for k,v in value.items() if k!='workspace'}
                    inherited['features']=list(dict.fromkeys(inherited.get('features',[])+extra.pop('features',[])))
                    if inherited.get('default-features',True):extra.pop('default-features',None)
                    inherited.update(extra);table[group][name]=inherited
                selected=table[group][name]
                if isinstance(selected,dict) and ('path' in selected or 'git' in selected):
                    raise ValueError('standalone projection requires registry dependencies')
    def required(table):
        for group in ('dependencies','dev-dependencies','build-dependencies'):
            if group in table:
                table[group]={name:value for name,value in table[group].items()
                              if not (isinstance(value,dict) and value.get('optional'))}
    result.pop('features',None)
    required(result)
    for table in result.get('target',{}).values():required(table)
    dependencies(result)
    for table in result.get('target',{}).values():dependencies(table)
    if result.get('lints',{}).get('workspace'):result['lints']=copy.deepcopy(workspace['lints'])
    result['workspace']={}
    return result


def project(root_manifest, member_manifest, lockfile, output):
    root=tomllib.loads(root_manifest.read_text());member=tomllib.loads(member_manifest.read_text())
    result=project_manifest(root,member)
    packages=tomllib.loads(lockfile.read_text())['package']
    member_package=resolve(result['package']['name']+' '+result['package']['version'],packages)
    root_identity=identity(member_package);seen={root_identity}
    declared=dependency_names(result)
    pending=[resolve(d,packages) for d in member_package.get('dependencies',[])
             if d.split(' ',1)[0] in declared]
    while pending:
        package=pending.pop();key=identity(package)
        if key in seen:continue
        seen.add(key)
        if key!=root_identity and not package.get('source','').startswith('registry+'):
            raise ValueError('reachable path/git patch is unsupported')
        pending.extend(resolve(d,packages) for d in package.get('dependencies',[]))
    output.mkdir(parents=True,exist_ok=True)
    (output/'Cargo.toml').write_text('\n'.join(toml(k)+' = '+toml(v) for k,v in result.items())+'\n')
    shutil.copyfile(lockfile,output/'Cargo.lock')


def dependency_names(manifest):
    """Package names the projected manifest depends on, aliases resolved."""
    tables=[manifest,*manifest.get('target',{}).values()]
    return {value.get('package',name) if isinstance(value,dict) else name
            for table in tables
            for group in ('dependencies','dev-dependencies','build-dependencies')
            for name,value in table.get(group,{}).items()}


def identity(package):
    return package['name'],package['version'],package.get('source','')


def resolve(reference,packages):
    parts=reference.split(' ',2)
    matches=[p for p in packages if p['name']==parts[0] and (len(parts)<2 or p['version']==parts[1]) and (len(parts)<3 or p.get('source')==parts[2].strip('()'))]
    if len(matches)!=1:raise ValueError('ambiguous locked dependency: '+reference)
    return matches[0]


def verify_lock(root_lock, projected_lock):
    """Cargo may prune feature edges, but cannot change any locked input."""
    original=tomllib.loads(root_lock.read_text())['package']
    projected=tomllib.loads(projected_lock.read_text())['package']
    expected={identity(p):p for p in original}
    member=tomllib.loads(projected_lock.with_name('Cargo.toml').read_text())['package']
    root_identity=(member['name'],member['version'],'')
    if sum(identity(p)==root_identity for p in projected)!=1:raise ValueError('projected root package missing')
    for package in projected:
        key=identity(package)
        if key!=root_identity and not package.get('source','').startswith('registry+'):
            raise ValueError('non-registry projected dependency')
        if key not in expected or package.get('checksum')!=expected[key].get('checksum'):
            raise ValueError('projected dependency drifted from root lock: '+str(key))
        old=expected[key]
        allowed={identity(resolve(d,original)) for d in old.get('dependencies',[])}
        actual={identity(resolve(d,projected)) for d in package.get('dependencies',[])}
        if not actual.issubset(allowed):raise ValueError('projected dependency edges drifted: '+str(key))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('workspace','member','lockfile','output'):p.add_argument('--'+name,type=Path)
    p.add_argument('--verify-lock',type=Path)
    a=p.parse_args()
    if a.verify_lock:
        if not a.lockfile:p.error('--lockfile required')
        verify_lock(a.lockfile,a.verify_lock)
    else:
        if not all((a.workspace,a.member,a.lockfile,a.output)):p.error('workspace/member/lockfile/output required')
        project(a.workspace,a.member,a.lockfile,a.output)
