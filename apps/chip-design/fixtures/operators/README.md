# Synthetic CPU operator contracts

These authored JSON fixtures exercise typed, bounded CPU receipt imports.
Digests and compiler identities are synthetic placeholders, not identities of
observed runs. Comparison counts test declared dimensions and scope consistency.
No fixture establishes model quality, primary-kernel execution, RTL, physical
PPA or manufacturability. Real project receipts live outside Tilo.

`gated-delta-geometry.json` exercises an additional generic 8-head, 16-wide
case. The importer does not infer whole-block coverage from any fixed geometry.
