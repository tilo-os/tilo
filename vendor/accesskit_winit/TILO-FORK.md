# tilo-accesskit-winit

The crates.io `accesskit_winit` 0.34.0, unchanged except that it depends on
`tilo-winit` (Tilo's winit fork) instead of `winit`, published as
`tilo-accesskit-winit` so the published Tilo SDK hands it the same winit types
it uses itself. The library is still named `accesskit_winit`. Upstream:
https://github.com/AccessKit/accesskit (Apache-2.0, unchanged).

It ends with tilo-winit: when a winit release carries Tilo's additions, the SDK
returns to the upstream `accesskit_winit`.
