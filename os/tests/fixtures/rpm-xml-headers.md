# Recorded RPM XML fixtures

Captured read-only from native Fedora Linux 42 in WSL using RPM's installed
database. They are format fixtures, not frozen Wine build inputs or signature
attestations. The complete headers are retained without invented query tags.

- `rpm-nano-default-editor.xml`: `rpm -q --xml nano-default-editor`, package
  `nano-default-editor-8.3-3.fc42.noarch`, five installed paths. RPM stores
  `Basenames`, `Dirnames`, and `Dirindexes`; `Filenames` is a computed query
  tag and is absent from XML.
- `rpm-default-editor-empty.xml`: `rpm -q --xml default-editor`, package
  `default-editor-8.3-3.fc42.noarch`, no installed paths. This exercises real
  payload-free metapackages, like the signed Wine spec's `wine-winefonts`.

Both fixtures keep dependencies, scripts and installed-package bookkeeping.
The merger compares unknown semantic fields conservatively. Production merge
reads package files using `rpm -qp --xml`, not the installed database.
