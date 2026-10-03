# Legacy application caret E2E

`scripts/legacy-app-caret-e2e.py` tests the compatibility boundary against
real libadwaita, Qt 6, and LibreOffice processes. It does not substitute a
mock client or infer success from a screenshot.

For each application the harness boots a fresh headless Tilo compositor and:

1. waits for the real Wayland toplevel to present a non-placeholder frame;
2. focuses a real text field and clears its existing value;
3. types a unique prefix;
4. moves the pointer outside the application without clicking elsewhere;
5. types a suffix, selects the field, and copies it through Wayland; and
6. requires Tilo's clipboard mirror to contain the exact combined payload.

Before booting Tilo, the Qt and LibreOffice cases briefly start the same
installed binary on its toolkit's compositor-independent backend. A crash
there fails with a distinct `preflight.log` diagnostic: that host cannot test
Tilo's Wayland compatibility, so the result must not be reported as a
compositor regression (or as a pass).

On VM hosts where `/usr` is virtiofs-backed, Qt can load shared objects but
cannot mmap their plugin metadata. The harness copies the installed Qt plugin
tree byte-for-byte into its private `/dev/shm` case directory and points Qt at
that ephemeral copy. This changes neither the tested application nor its
Wayland backend; it only makes the host's installed plugins loadable. Qt and
LibreOffice also keep their throwaway profiles in tmpfs so host-filesystem
quirks cannot masquerade as compositor failures.

This fails for lost keyboard focus, a caret cleared by pointer leave, dropped
or duplicated keys, input routed to the wrong surface, failure to adopt the
application, or a broken client-to-compositor clipboard path. Each case keeps
its compositor log, application log, result JSON, and a final screenshot.

## Running locally

Install `gnome-text-editor`, `qt6-qtbase-examples`, and `libreoffice-writer`,
build the shell, then run:

```sh
cargo build --locked -p shell --bin shell
python3 scripts/legacy-app-caret-e2e.py
```

Use `--case libadwaita`, `--case qt6`, or `--case libreoffice` while
investigating one toolkit. Missing application executables are failures, not
skips, because a silently reduced matrix would make this gate misleading.

## CI policy

`.forgejo/workflows/ci-legacy-apps.yml` is required on pull requests that
touch compositor input, legacy collaboration/streaming, peer-view input, the
harness, or its build environment. The same path filter runs after direct
pushes to `main`. A daily scheduled run catches regressions caused by builder
image and toolkit updates even when Tilo source did not change.

Running the job for every unrelated pull request is unnecessary: it launches
three GUI applications and builds the shell, while the ordinary affected-crate
workflow already covers changes outside this compatibility boundary. Daily is
frequent enough for dependency drift; path-gated pre-merge execution prevents
known caret regressions from landing.
