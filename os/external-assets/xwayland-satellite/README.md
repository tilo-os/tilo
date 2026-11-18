# xwayland-satellite, patched

The OS image and the Linux VM guest run xwayland-satellite built from the
pinned upstream source with the patches here, instead of Fedora's package.
`tools/ci/install-xwayland-satellite.sh` builds and installs it everywhere: the
`xwayland-satellite-builder` stage of `os/Containerfile`, the Linux VM guest
(`os/Containerfile.linux-guest`) and the developer distrobox
(`dist/dev/Containerfile.fedora44`). The CI builder carries none: no test
there drives it since the shell's X11 clipboard tests went with the shell.

- Upstream: <https://github.com/Supreeeme/xwayland-satellite>, MPL-2.0.
- Base: upstream main at `779151fdfc034048c770edbfd2818c7a3389bd12`
  (v0.8.3-10-g779151f; `xwayland-satellite-779151f.tar.gz` in
  `../github-assets.tsv`), the first commit with "fix: drain X11 events
  buffered during Wayland dispatch". Up to v0.8.3, X events that arrive while
  satellite waits for an X reply stay in its X library's queue, and nothing
  wakes it for them: a new X window could exist on the X server and never be
  shown. No release contains the fix yet; move to the first one that does.
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

`clipboard_e2e::x11_clipboard_mirror_handoff_*` in the shell's tests forced
that interleaving, and both failed with Fedora's 0.8.2 build; they went with
the shell.

Once a satellite release contains the upstream fix, install the distribution
package again and delete this directory and the install script.
