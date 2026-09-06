# Initial snapshot output correction

The first read-only inventory call printed full hashes for every historical generated artifact and exceeded its output limit. Parsing that truncated output failed before any snapshot was saved. The corrected call returns full hashes only for the selected precompile release baseline, plus compact metadata for the inventory. No Cargo command or shader validation ran during this correction.
