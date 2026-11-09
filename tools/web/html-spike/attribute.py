#!/usr/bin/env python3
"""Bytes of function bodies per crate in a wasm module that kept its name section.

usage: attribute.py MODULE.wasm [--top N]
Prints a JSON object {crate: bytes} and the renderer total (wgpu, naga, vello's
GPU side) that a page showing HTML would not need.
"""
import json, re, sys
from collections import Counter

def leb(data, i):
    result = shift = 0
    while True:
        b = data[i]; i += 1
        result |= (b & 0x7f) << shift; shift += 7
        if not b & 0x80:
            return result, i

def sections(data):
    i = 8
    while i < len(data):
        sid = data[i]; i += 1
        size, i = leb(data, i)
        yield sid, data[i:i + size]
        i += size

def main():
    data = open(sys.argv[1], 'rb').read()
    imports = 0
    bodies = []
    names = {}
    for sid, body in sections(data):
        if sid == 2:
            n, j = leb(body, 0)
            for _ in range(n):
                for _ in range(2):
                    l, j = leb(body, j); j += l
                kind = body[j]; j += 1
                if kind == 0:
                    _, j = leb(body, j); imports += 1
                elif kind == 1:
                    j += 1; flags, j = leb(body, j); _, j = leb(body, j)
                    if flags & 1: _, j = leb(body, j)
                elif kind == 2:
                    flags, j = leb(body, j); _, j = leb(body, j)
                    if flags & 1: _, j = leb(body, j)
                elif kind == 3:
                    j += 2
                else:
                    j += 2
        elif sid == 10:
            n, j = leb(body, 0)
            for _ in range(n):
                size, j = leb(body, j); bodies.append(size); j += size
        elif sid == 0:
            l, j = leb(body, 0)
            if body[j:j + l] != b'name':
                continue
            j += l
            while j < len(body):
                sub = body[j]; j += 1
                size, j = leb(body, j)
                end = j + size
                if sub == 1:
                    n, k = leb(body, j)
                    for _ in range(n):
                        idx, k = leb(body, k)
                        l, k = leb(body, k)
                        names[idx] = body[k:k + l].decode('utf-8', 'replace'); k += l
                j = end
    by_crate = Counter()
    for i, size in enumerate(bodies):
        name = names.get(imports + i, '?')
        by_crate[crate(name)] += size
    total = sum(bodies)
    renderer = {k: v for k, v in by_crate.items() if re.match(r'^(wgpu|wgpu_core|wgpu_hal|wgpu_types|naga|tilo_vello|vello|vello_shaders|tilo_vello_shaders|spirv)$', k)}
    text = {k: v for k, v in by_crate.items() if re.match(r'^(cosmic_text|harfrust|rustybuzz|skrifa|read_fonts|swash|fontdb|ttf_parser|unicode_bidi|unicode_linebreak|unicode_segmentation|hypher|icu_\w+)$', k)}
    print(json.dumps({'total': total, 'renderer': renderer, 'renderer_total': sum(renderer.values()),
                      'text': text, 'text_total': sum(text.values()),
                      'taffy': by_crate.get('taffy', 0), 'tilo_cpu_raster': by_crate.get('tilo_cpu_raster', 0), 'vello_cpu': by_crate.get('vello_cpu', 0),
                      'top': by_crate.most_common(int(sys.argv[3]) if len(sys.argv) > 3 else 40)}, indent=1))

def crate(name):
    # v0 mangling: the first crate root (`C`, an optional `s<hash>_`
    # disambiguator, then a length-prefixed identifier) is the crate that
    # defines the item; a generic's instantiations count for that crate.
    if name.startswith('_R'):
        m = re.search(r'C(?:s[0-9A-Za-z]*_)?(\d+)', name)
        if m:
            l = int(m.group(1))
            return name[m.end():m.end() + l]
    return legacy_crate(name)

def legacy_crate(name):
    # Mangled v0/legacy names carry the crate path; take the first path segment
    # that is not a generic wrapper.
    n = name
    m = re.match(r'^_ZN(\d+)', n)
    if m:
        l = int(m.group(1)); return n[len(m.group(0)):len(m.group(0)) + l]
    for prefix in ('<', '&', '*const ', '*mut ', 'dyn '):
        while n.startswith(prefix):
            n = n[len(prefix):]
    m = re.match(r'([A-Za-z_][A-Za-z0-9_]*)::', n)
    return m.group(1) if m else n.split('(')[0][:40]

main()
