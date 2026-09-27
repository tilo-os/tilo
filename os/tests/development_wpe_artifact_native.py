#!/usr/bin/env python3
"""Synthetic completed-stage transport proof inside disposable native CI only.

The payload is /usr/bin/true copied under WPE filenames, NOT WebKit. This tests
real TLS registry/config verification and immutable stage reuse, not compilation
or WPE functionality. Import never executes these synthetic image programs.
"""
import copy
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
from unittest.mock import patch

from tls_registry_fixture import run, tls_registry

DEV = Path(__file__).resolve().parents[1] / 'dev'
SPEC = importlib.util.spec_from_file_location('wpe_artifact', DEV / 'wpe-stage-artifact.py')
wpe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wpe)


def podman(*arguments):
    return run('podman', '--remote=false', *arguments)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def main():
    architecture = platform.machine()
    require(os.getuid() == 0 and architecture in wpe.ARCHES and
            os.environ.get('TILO_REQUIRE_BOOT_SIGNATURE_FIXTURE') == '1' and
            any(Path(name).exists() for name in ('/.dockerenv', '/run/.containerenv')),
            'requires the disposable root native CI container')
    images = []
    with tls_registry() as authority, tempfile.TemporaryDirectory(
            prefix='tilo-wpe-artifact-fixture-', dir='/var/lib') as temporary:
        root = Path(temporary)

        def build(context, name, *extra):
            iid = root / (name + '.iid')
            podman('build', '--pull=never', '--network=none', '--no-cache',
                   '--skip-unused-stages=true', '--iidfile', str(iid), *extra, str(context))
            identity = iid.read_text().strip()
            require(identity.startswith('sha256:') and len(identity) == 71, 'full image ID required')
            images.append(identity)
            return identity

        try:
            source = root / 'source'
            source.mkdir()
            native_elf = Path('/usr/bin/true').resolve(strict=True)
            shutil.copyfile(native_elf, source / 'native-elf')
            (source / 'fixture.pc').write_text('Name: Synthetic transport fixture; not WebKit\n')
            (source / 'unrelated-toolchain-marker').write_text('must not be exported\n')
            (source / 'Containerfile').write_text(
                'FROM scratch\n'
                'COPY native-elf /wpe/usr/lib64/libWPEWebKit-2.0.so\n'
                'COPY native-elf /wpe/usr/libexec/wpe-webkit-2.0/WPEWebProcess\n'
                'COPY fixture.pc /wpe/usr/lib64/pkgconfig/wpe-webkit-2.0.pc\n'
                'COPY unrelated-toolchain-marker /unrelated-toolchain-marker\n')
            original = build(source, 'synthetic')

            # Construct independently hashable synthetic dependency inputs.
            # Their tiny contents are intentionally not WPE release archives.
            repo = root / 'dependency-repo'
            (repo / 'os/dev').mkdir(parents=True)
            assets = root / 'assets'
            assets.mkdir()
            pins = []
            for variable in ('LIBWPE', 'FDO', 'WPEWK'):
                name = variable.lower() + '-fixture.tar.xz'
                data = ('synthetic-' + variable).encode()
                (assets / name).write_bytes(data)
                pins.extend((f'{variable}={name}', f'{variable}_SHA256={wpe.sha(data)}'))
            (repo / 'os/dev/build-wpe.sh').write_text('\n'.join(pins) + '\n')
            (repo / 'os/Containerfile.wpe').write_text((source / 'Containerfile').read_text())
            buildroot = 'registry.tilo.so/tilo/tilo-wpe-buildroot@sha256:' + '1' * 64
            inputs = wpe.dependency_inputs(repo, assets, architecture, '0', buildroot)

            # Only the destination repository is substituted. All production
            # export/import, TLS, Skopeo, config, Podman and mount code executes.
            with patch.object(wpe, 'repository', side_effect=lambda arch: f'{authority}/wpe-stage-{arch}'):
                projected = wpe._project_payload(original, inputs)
                images.append(projected)
                require(projected != original, 'payload projection retained original image')
                with wpe.mounted(projected) as mounted:
                    require(not (mounted / 'unrelated-toolchain-marker').exists(), 'toolchain marker exported')
                    require({entry.name for entry in mounted.iterdir()} == {'wpe'}, 'unrelated root entries exported')
                receipt = wpe._export_stage(projected, inputs, 6, 'a' * 40, '42', '1',
                                            source_completed_stage_id=original)
                podman('image', 'rm', '--no-prune', projected)
                absent = subprocess.run(['podman', '--remote=false', 'image', 'exists', projected],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                require(absent.returncode == 1, 'exported payload image still exists locally')
                imported = wpe.import_stage(receipt, inputs)
                require(imported == projected, 'registry roundtrip changed config identity')

                rejected = []
                negatives = []
                for field, value in [('recipes', inputs['recipes'] | {'os/Containerfile.wpe': '0' * 64}),
                                     ('assets', [{'name': 'wrong.tar.xz', 'sha256': '0' * 64}]),
                                     ('epoch', '1'),
                                     ('architecture', 'aarch64' if architecture == 'x86_64' else 'x86_64')]:
                    negatives.append((field, receipt, inputs | {field: value}))
                wrong_digest = copy.deepcopy(receipt)
                wrong_digest['config_digest'] = 'sha256:' + '0' * 64
                negatives.append(('config digest', wrong_digest, inputs))
                wrong_manifest = copy.deepcopy(receipt)
                wrong_manifest['reference'] = f'{authority}/wpe-stage-{architecture}@sha256:' + '0' * 64
                negatives.append(('manifest digest', wrong_manifest, inputs))
                negatives.append(('missing receipt', {}, inputs))
                for name, evidence, expected in negatives:
                    try:
                        wpe.import_stage(evidence, expected)
                    except ValueError:
                        rejected.append(name)
                    else:
                        raise ValueError('invalid import accepted: ' + name)

            consumer = root / 'consumer'
            consumer.mkdir()
            (consumer / 'Containerfile').write_text(
                'FROM scratch AS wpe-builder\n'
                'RUN ["/deliberately-missing-original-stage-program"]\n'
                'FROM wpe-builder AS final\n'
                'COPY --from=wpe-builder /wpe/ /copied-wpe/\n')
            final = build(consumer, 'consumer', '--target', 'final', '--build-context',
                          f'wpe-builder=container-image://{imported}')
            with wpe.mounted(final) as mounted:
                require(wpe.payload_identity(mounted, architecture) == receipt['payload'],
                        'inherited WPE payload changed')
                for relative, digest in receipt['payload'].items():
                    require(wpe.file_hash(mounted / 'copied-wpe' / relative, 2 * 1024 * 1024 * 1024) == digest,
                            'COPY payload changed')
            print(json.dumps(dict(result='pass', proof='synthetic WPE artifact transport and stage reuse only',
                real_webkit_built=False, architecture=architecture,
                podman_version=podman('--version').decode().strip(),
                skopeo_version=run('skopeo', '--version').decode().strip(),
                original_image_id=original, projected_image_id=projected,
                imported_image_id=imported, final_image_id=final, unrelated_toolchain_excluded=True,
                exported_image_removed_before_import=True, cold_registry_restore_verified=True,
                registry_manifest=receipt['reference'], config_digest=receipt['config_digest'],
                rejected=rejected, original_stage_executed=False), sort_keys=True))
        finally:
            primary_failure = sys.exc_info()[0] is not None
            failed = False
            for identity in dict.fromkeys(reversed(images)):
                result = subprocess.run(['podman', '--remote=false', 'image', 'rm', '--no-prune', identity],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                failed |= result.returncode != 0
            if failed:
                if not primary_failure:
                    raise RuntimeError('synthetic fixture image cleanup failed')
                print('Additional synthetic fixture image cleanup failed', file=sys.stderr)


if __name__ == '__main__':
    main()
