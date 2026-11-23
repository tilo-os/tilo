"""Copy a linked WebAssembly module without its `linking` and `reloc.*` custom
sections (the object-file metadata `--emit-relocs` keeps).

usage: without-link-sections.py LINKED OUT

The splitter (split.sh) reads the call graph and the static data's symbols
from those sections of the linked module itself; the module wasm-bindgen binds
for it needs none of them, and without them it is about a third smaller, so
both wasm-bindgen and every parse the splitter makes of the bound module do
less (crates/core/tilo-browser-runtime/scripts/build.sh, profile `local`).
"""

import sys


def uleb(data, at):
    value = shift = 0
    while True:
        byte = data[at]
        at += 1
        value |= (byte & 0x7F) << shift
        shift += 7
        if byte < 0x80:
            return value, at


def main():
    linked, out = sys.argv[1:]
    data = open(linked, 'rb').read()
    if data[:4] != b'\0asm':
        sys.exit(f'{linked} is not a WebAssembly module')
    parts = [data[:8]]
    at = 8
    while at < len(data):
        start = at
        size, at = uleb(data, at + 1)
        end = at + size
        if data[start] == 0:
            length, name_at = uleb(data, at)
            name = data[name_at:name_at + length]
            if name == b'linking' or name.startswith(b'reloc.'):
                at = end
                continue
        parts.append(data[start:end])
        at = end
    with open(out, 'wb') as file:
        file.write(b''.join(parts))


if __name__ == '__main__':
    main()
