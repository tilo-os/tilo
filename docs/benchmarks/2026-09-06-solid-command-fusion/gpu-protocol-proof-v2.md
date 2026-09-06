# GPU protocol proof from validation v2

The vendor suite passed82 tests with0 failures/ignored tests. The SOLID-fuse
fixture reached the GPU and emitted its proof summary (not the no-device skip).
It compared dense, original sparse and fused requested pixels exactly at49x17,
including disjoint tiles, a final1x1 native edge, depth9 clips and Multiply.

Its captured original stream had123 reachable commands/196 words; the fused
stream had68 commands/141 words. Flagged successors included COLOR, all three
gradients, IMAGE, BLUR and END_CLIP. Both streams had reachable JUMPs; FILL count
was preserved and structural tags were unflagged. Captured PTCL was12582912bytes,
below the64MiB cfg(test) cap. The production path has no such capture/readback.

These fixture counts intentionally stress full-cover commands. They differ
from the representative shell scenes'29%command/~15%payload-word potential,
and neither constitutes a measured latency, VRAM or copy improvement. Native4K
framework validation and release timing remain separate gates.

Source/input: validation-inputs-v2.sha256. Full command/output: vendor-tests-v2.log.
The earlier compile failure is separately retained in vendor-tests.log.
