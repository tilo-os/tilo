# Preserved candidate executables

Built from `ec4c2127a06ed05815fcbeda049118e1b0c4e0f8` plus the frozen
active-native-bin worktree changes, using unchanged ordinary release settings.
The locked combined build completed successfully in 14m 01s. No inherited
release-profile/RUSTFLAGS/compiler-wrapper overrides were present when checked.

| Executable under `target/ui-perf-comparison/` | SHA-256 |
| --- | --- |
| `ui-perf-active-native-coarse-bins-20260905` | `6e24e4c70434e9b1c08e9da698b6e51b5c1a650c3b5060377f52da191afeb0a4` |
| `shell-redraw-active-native-coarse-bins-20260905` | `58ec00cb2fee7c95a453fe5398319402871dc655ddf142324ac871491bec2d88` |

The additional historical-drift control is the unchanged
`ui-perf-zero-gradient-default-release-20260905`, SHA-256
`11eecf86f82facc7c5002687f7398b4a4c1dc50b5e953554ad3762b243c3a1e0`.
The following committed UI control retains SHA-256
`e3bf83357fbf4ac2554adc3efe366337aaa1dfed9ed06a3bf5640cd08059385d`.

Compiler and test processes were checked absent before candidate timing.
All experimental active-bin invocations remain comparative diagnostics, not
claims that production defaults meet the objective.
