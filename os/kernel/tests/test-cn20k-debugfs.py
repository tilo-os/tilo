#!/usr/bin/env python3
"""Exercise actual Kbuild object selection, without compiling the kernel."""
from pathlib import Path
import subprocess
import tempfile

kernel = Path(__file__).resolve().parents[1]
patch = kernel / "patches/0001-octeontx2-gate-cn20k-debugfs.patch"
original = (kernel / "tests/fixtures/octeontx2-af.Makefile").read_bytes()
with tempfile.TemporaryDirectory(prefix="tilo-cn20k-") as directory:
    root = Path(directory)
    makefile = root / "drivers/net/ethernet/marvell/octeontx2/af/Makefile"
    makefile.parent.mkdir(parents=True)
    makefile.write_bytes(original)
    def objects(enabled):
        result = subprocess.check_output([
            "make", "--no-print-directory", "-s", "-f", str(makefile), "-f", "-",
            "CONFIG_DEBUG_FS=" + enabled, "print-objects",
        ], input="print-objects:\n\t@printf '%s\\n' '$(rvu_af-y)'\n", text=True)
        return result.split()
    before = objects("")
    assert "cn20k/debugfs.o" in before  # Reproduces the broken disabled case.
    subprocess.run(["git", "apply", "--check", str(patch)], cwd=root, check=True)
    subprocess.run(["git", "apply", str(patch)], cwd=root, check=True)
    assert objects("") == [obj for obj in before if obj != "cn20k/debugfs.o"]
    enabled = objects("y")
    assert len(enabled) == len(set(enabled))
    assert set(enabled) == set(before)  # Every hardware/debug object retained.
    assert "cn20k/npc.o" in objects("")
print("PASS: CONFIG_DEBUG_FS=n omits only CN20K debugfs; y retains all driver objects")
