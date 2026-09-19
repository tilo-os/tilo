#!/usr/bin/env python3
"""Real runtime regression cases: EOF, truncated and malformed framed input."""
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

host = Path(sys.argv[1]).resolve()
cases = {
    'EOF': b'',
    'truncated-header': b'\x01\x00',
    'oversized': struct.pack('<I', 64 * 1024 * 1024 + 1),
    'zero-length': b'\0\0\0\0',
    'truncated-body': struct.pack('<I', 10) + b'\x01{}',
    'invalid-json': struct.pack('<I', 3) + b'\x01{x',
    'missing-op': struct.pack('<I', 3) + b'\x01{}',
    'wrong-kind': struct.pack('<I', 1) + b'\xff',
    'wrong-command-field': None,
}
bad = b'\x01' + json.dumps({'op':'focus', 'focused':'yes'}).encode()
cases['wrong-command-field'] = struct.pack('<I', len(bad)) + bad
for name, data in cases.items():
    with tempfile.TemporaryDirectory(prefix='qos-cef-lifecycle-') as profile:
        out_file, err_file = tempfile.TemporaryFile(), tempfile.TemporaryFile()
        p = subprocess.Popen([str(host), '--profile='+profile], stdin=subprocess.PIPE, stdout=out_file, stderr=err_file)
        try:
            p.stdin.write(data)
            p.stdin.flush()
            if name in ('EOF', 'truncated-header', 'truncated-body'):
                p.stdin.close()
            p.wait(timeout=35)
        finally:
            if p.poll() is None:
                p.kill()
                p.wait()
        out_file.seek(0)
        err_file.seek(0)
        out, err = out_file.read(), err_file.read()
        assert p.returncode == 0, (name, p.returncode, err.decode(errors='replace'))
        events = []
        while out:
            assert len(out) >= 4
            size, = struct.unpack_from('<I', out)
            assert len(out) >= 4 + size
            payload, out = out[4:4+size], out[4+size:]
            if payload[0] == 1:
                events.append(json.loads(payload[1:]))
        assert any(e['event'] == 'closed' for e in events), (name, events)
        print('PASS:', name, flush=True)
