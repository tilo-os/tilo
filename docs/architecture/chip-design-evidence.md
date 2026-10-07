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

## Reported physical macro placement

A library-physical evidence-flow stage can include optional `placement` data.
The importer retains exact integer database units, a die rectangle, selected macro
rectangles, orientation and placement status. It requires both the database and
observation artifact references to exist in the report, unique names and resource
IDs, nonempty bounded rectangles inside the die, and an explicit count of omitted
instances. At most 256 macros and one million total instances are admitted.
Malformed geometry refuses the complete import before shared-state mutation.

Each macro is a child of its existing evidence-stage resource. System search and
the existing `inspect-evidence` action select the same macro; public project
`read` with its item anchor returns exact geometry and reported custody. The view
uses shared Manual layout and named action-bound Cards, a uniform display scale,
a lower-left physical origin, and the system theme. Macro selection is local view
state; the imported report uses existing shared history and undo.

This is a macro-footprint inspector. It explicitly counts omitted instances and
does not authenticate detached reports or establish routing, signoff, manufactured
silicon, or model quality. Existing reports without placement remain compatible.

## Existing file handoff

Chip Design consumes files through the framework's `LaunchInput::file`. Ordinary
JSON files and the binary-named files staged by existing content-collecting
launches are admitted as possible reports; the complete bytes must be UTF-8,
within the existing 1 MiB limit, and pass the existing storage or verification
importer before any action is submitted. Unsupported or malformed input shows a
localized refusal and creates no report or report history. Opening without a
file still starts empty.

A successful handoff invokes the same typed import and inspection actions as the
file chooser and semantic API, preserving permissions, attribution, history and
undo. The host supplies bytes; the app does not fetch a caller-provided hash,
resolve ambient paths, execute artifacts or create another content service.
`application/octet-stream` is required because the current platform stages
non-media content transfers as `.bin`; admission is not a declaration that
arbitrary binary formats are supported.

The authored package manifest advertises the same JSON and octet-stream inputs
and the JSON extension, so file discovery sees the implemented handoff. Provider
registration does not bypass report validation or make a malformed binary input
a report.
