"""Production constructor and resolved normal/build graph stay compositor-free."""
import json
import os
from pathlib import Path
import subprocess

metadata = json.loads(subprocess.check_output([
    'cargo', 'metadata', '--format-version', '1', '--no-deps', '--locked', '--offline'
]))
packages = {p['name']: p for p in metadata['packages']}
provider = packages.get('tilo-action-broker', packages['shell'])
# cargo tree resolves external and platform dependencies too; --no-deps metadata
# alone cannot prove the transitive boundary. The default checks the native
# host target; BROKER_CHECK_TARGET=all also includes other platforms when cached.
target = os.environ.get("BROKER_CHECK_TARGET") or subprocess.check_output(
    ["rustc", "-vV"], text=True).split("host: ")[1].splitlines()[0]
graph = subprocess.check_output([
    'cargo', 'tree', '-p', provider['name'], '--edges', 'normal,build',
    '--target', target, '--prefix', 'none', '--format', '{p}', '--locked', '--offline'
], text=True)
names = {line.split()[0] for line in graph.splitlines() if line.strip()}
for forbidden in ('shell', 'smithay', 'pipewire'):
    assert forbidden not in names, f'production broker pulls compositor dependency: {forbidden}'
assert provider['name'] == 'tilo-action-broker'
source = Path(provider['manifest_path']).parent / 'src'
assert 'pub fn spawn_native_peer(' in (source / 'collab_broker.rs').read_text()
assert 'struct CapnpChannelHandle' in (source / 'collab_broker_capnp.rs').read_text()
shell = Path(packages['shell']['manifest_path']).parent
assert any(d['name'] == provider['name'] and d['kind'] is None
           for d in packages['shell']['dependencies'])
assert 'tilo_action_broker::collab_broker::spawn_with_services' in (shell / 'src/shell/collab_broker.rs').read_text()
assert 'pub use tilo_action_broker::collab_broker_capnp::*;' in (shell / 'src/shell/collab_broker_capnp.rs').read_text()
print(f'portable production broker: resolved normal/build graph passes ({len(names)} packages, target {target}); native shell consumes the same implementation')
