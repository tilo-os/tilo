# Development reference

Making a contribution? [Start here](../../CONTRIBUTING.md): get a checkout, run
and change one app or crate, then open a PR. The instructions are together on
that page.

These pages cover specific development tasks when you need more detail.

| I want to…                         | Start here                                                                |
| ---------------------------------- | ------------------------------------------------------------------------- |
| Make my first code change          | [Contribute an app or crate change](../../CONTRIBUTING.md#get-a-checkout) |
| Check a change                     | [Run the relevant tests](testing.md)                                      |
| Run the desktop in a window        | [Run a development desktop](run-desktop.md)                               |
| Try my change in the complete OS   | [Use a development VM](dev-vms.md)                                        |
| Work on an installed Linux session | [Develop a host session](host-session.md)                                 |
| Put my build on a device or board  | [Development devices](devices.md)                                         |
| Fix a setup or build problem       | [Troubleshooting](troubleshooting.md)                                     |
| Write an app of my own             | [App SDK guide](../developers/getting-started.md)                         |

[CONTRIBUTING.md](../../CONTRIBUTING.md) covers reporting problems and
submitting changes. The [code map](../codemap.md) helps find a component.

## Platform setup

[Linux](platforms/linux.md) · [macOS](platforms/macos.md) ·
[Windows](platforms/windows.md) · [Android](platforms/android.md)

Tilo can run in a window on macOS; replacing your login desktop or booting the
OS is a different, Linux-based workflow. Each platform page explains its own
prerequisites. A small library change does not require the full desktop test
environment.

## Look up a tool or a specific detail

- [t: builds, checks and integration](../t.md)
- [Nested-session tool](tools/nested-session.md)
- [Test environments and headless tools](tools/testing.md)
- [Git hooks and generated files](tools/git.md)
- [Dependencies and build features](dependencies.md)
- [Writing isolated tests](test-isolation.md)

These are references, not a reading list to finish before contributing.
