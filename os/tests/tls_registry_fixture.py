"""Loopback TLS registry for the disposable native CI container only."""
from contextlib import contextmanager
import os
from pathlib import Path
import shutil
import socket
import ssl
import subprocess
import tempfile
import time
import urllib.request
import uuid

REGISTRY = 'docker.io/library/registry@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373'


def run(*argv):
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        raise RuntimeError(f'{Path(argv[0]).name} failed with exit {result.returncode}')
    return result.stdout


@contextmanager
def tls_registry():
    """Install/remove one public CA inside this disposable container's trust store.

    No host directories are mounted. The private server/CA keys stay in the
    temporary tree, and the registry sees only its own server credentials.
    """
    if (os.getuid() != 0 or os.environ.get('TILO_REQUIRE_BOOT_SIGNATURE_FIXTURE') != '1'
            or not any(Path(p).exists() for p in ('/.dockerenv', '/run/.containerenv'))):
        raise ValueError('TLS fixture requires the disposable root CI container')
    name = 'tilo-tls-fixture-' + uuid.uuid4().hex
    anchor = Path('/etc/pki/ca-trust/source/anchors') / (name + '.crt')
    with tempfile.TemporaryDirectory(prefix=name + '-', dir='/var/lib') as temporary:
        root = Path(temporary)
        server = root / 'server'
        server.mkdir(mode=0o700)
        ca = root / 'ca.crt'
        run('openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
            '-subj', '/CN=Tilo Disposable Registry CA/', '-addext', 'basicConstraints=critical,CA:TRUE',
            '-addext', 'keyUsage=critical,keyCertSign,cRLSign', '-keyout', str(root / 'ca.key'), '-out', str(ca))
        run('openssl', 'req', '-new', '-newkey', 'rsa:2048', '-nodes',
            '-subj', '/CN=Tilo Loopback Registry/', '-keyout', str(server / 'key.pem'), '-out', str(root / 'server.csr'))
        extensions = root / 'extensions'
        extensions.write_text('subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\n'
                              'keyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n')
        run('openssl', 'x509', '-req', '-in', str(root / 'server.csr'), '-CA', str(ca),
            '-CAkey', str(root / 'ca.key'), '-CAcreateserial', '-days', '1',
            '-extfile', str(extensions), '-out', str(server / 'cert.pem'))
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        authority = f'127.0.0.1:{port}'
        cert_directory = Path('/etc/containers/certs.d') / authority
        cert_directory.parent.mkdir(parents=True, exist_ok=True)
        cert_directory.mkdir(mode=0o700)  # Never overwrite a pre-existing trust directory.
        anchor_created = False
        try:
            with anchor.open('xb') as output:
                output.write(ca.read_bytes())
            anchor_created = True
            shutil.copyfile(ca, cert_directory / 'ca.crt')
            run('update-ca-trust', 'extract')
            run('podman', '--remote=false', 'run', '--detach', '--name', name, '--network=host',
                '--volume', f'{server}:/tls:ro,Z', '--env', f'REGISTRY_HTTP_ADDR={authority}',
                '--env', 'REGISTRY_HTTP_TLS_CERTIFICATE=/tls/cert.pem',
                '--env', 'REGISTRY_HTTP_TLS_KEY=/tls/key.pem', REGISTRY)
            context = ssl.create_default_context(cafile=str(ca))
            for _ in range(60):
                try:
                    with urllib.request.urlopen(f'https://{authority}/v2/', context=context, timeout=1) as response:
                        if response.status == 200:
                            break
                except OSError:
                    time.sleep(0.25)
            else:
                raise RuntimeError('disposable TLS registry did not become ready')
            yield authority
        finally:
            subprocess.run(['podman', '--remote=false', 'rm', '--force', name],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            (cert_directory / 'ca.crt').unlink(missing_ok=True)
            cert_directory.rmdir()
            if anchor_created:
                anchor.unlink()
                run('update-ca-trust', 'extract')
