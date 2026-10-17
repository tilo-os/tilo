# xwayland-satellite, patched

The CI builders run xwayland-satellite built from the pinned upstream source
with the patches here, instead of Fedora's package.
`tools/ci/install-xwayland-satellite.sh` builds and installs it everywhere:
`os/Containerfile.builder`, the developer distrobox
(`dist/dev/Containerfile.fedora44`) and, for CI containers whose builder
predates it, `tools/ci/ensure-x11-bridge-tools.sh`.

- Upstream: <https://github.com/Supreeeme/xwayland-satellite>, MPL-2.0.
- Base: v0.8.2, commit `8d135d3b2854b30fd01ea6cd6c27e523dd50a839`
  (`xwayland-satellite-8d135d3.tar.gz` in `../github-assets.tsv`).
- Features: `systemd,fontconfig`, as Fedora's package builds it; the decoration
  font comes from `open-sans-fonts`.

## 0001-drain-pending-conversions-before-takeover.patch

Upstream pull request:
<https://github.com/Supreeeme/xwayland-satellite/pull/527>.

When the compositor's clipboard manager adopts an X11 selection, satellite
replaced its current X11 selection with the manager's Wayland one. Conversions
a Wayland client had already requested through the original offer lived in
the old selection, so their pipes were closed empty: a paste racing the
takeover arrived as nothing. The patch keeps the original X11 owner, its
pending pipes and its event routing until every accepted conversion has
finished, and only then takes X11 ownership for the manager's selection. A new
X11 owner cancels the deferred takeover; replacing or clearing the Wayland
selection drops it.

`clipboard_e2e::x11_clipboard_mirror_handoff_*` in the shell's tests force
that interleaving; both fail with Fedora's 0.8.2 build.

Once a satellite release contains the upstream fix, install the distribution
package again and delete this directory and the install script.
