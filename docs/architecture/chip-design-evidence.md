# Chip Design evidence inspection

Chip Design imports detached producer reports through the project resource's
`import-verification` action. A report's child resource exposes `inspect` and
`read`; people, accessibility clients and agents use the same retained state.
Import is undoable. The app starts empty and neither runs imported artifacts nor
authenticates their declared hashes.

Architecture-study synthesis observations can include a `memories` array. It
describes selected reported logical memories; omission preserves older reports.
Each entry has a unique bounded name, `width_bits`, `depth_words`, `bits`,
`read_ports`, `clocked_read_ports`, `write_ports` and `fully_initialized`.
Storage must equal width multiplied by depth without overflow, clocked reads
cannot exceed reads, and a memory must have at least one read or write port.
The importer accepts at most 32 memories per variant and 65,536 ports of each
direction. Empty dimensions, duplicate names, control characters and inconsistent
geometry refuse the whole import before shared-state mutation.

The existing inspector presents one section per reported memory, with dimensions,
ports and initialization in the shared PropertyList. These fields also survive
public `read`. Zero write ports and asynchronous reads are valid for ROM-like
logical structures. `fully_initialized` means all initial bits are declared
known; false also covers partially known initial contents.

These observations describe generic synthesis structures. They do not identify
a licensed SRAM or ROM, physical port implementation, timing, area or energy.
Existing architecture-study exclusions and the refusal of physical-memory or
netlist-equivalence qualification remain in effect. Integrity checks against
retained local artifacts belong to the producing project, outside this app.
