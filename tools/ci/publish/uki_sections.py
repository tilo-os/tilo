#!/usr/bin/env python3
"""Write a UKI's PE sections (.linux, .initrd, .cmdline, .dtb, ...) into OUTDIR,
and report whether it carries an Authenticode signature.   uki_sections.py UKI OUTDIR"""
import struct, sys, os
data = open(sys.argv[1], "rb").read()
out = sys.argv[2]; os.makedirs(out, exist_ok=True)
pe = struct.unpack_from("<I", data, 0x3c)[0]
nsec = struct.unpack_from("<H", data, pe + 6)[0]
optsize = struct.unpack_from("<H", data, pe + 20)[0]
magic = struct.unpack_from("<H", data, pe + 24)[0]
va, size = struct.unpack_from("<II", data, pe + 24 + (144 if magic == 0x20b else 128))
print("authenticode-bytes", size)
sec = pe + 24 + optsize
for i in range(nsec):
    name, vsize, vaddr, rawsize, rawptr = struct.unpack_from("<8sIIII", data, sec + 40 * i)
    name = name.rstrip(b"\0").decode()
    blob = data[rawptr:rawptr + min(vsize, rawsize) if vsize else rawsize]
    open(os.path.join(out, name.lstrip(".")), "wb").write(blob)
    print(name, len(blob))
