#!/usr/bin/env bash
# The applications scripts/legacy-app-caret-e2e.py drives, installed as root
# (t's legacy-apps check) rather than carried in the CI builder. Their exact
# versions are printed so a failure names the Fedora packages it ran against,
# and a subject missing after the install fails here, before the compositor
# runs. No pipefail: find reports the Qt examples directory this Fedora doesn't
# use, and only grep's verdict on what it found counts.
set -eu

dnf install -y --setopt=install_weak_deps=False \
  gnome-text-editor qt6-qtbase-examples libreoffice-writer
rpm -q gnome-text-editor qt6-qtbase-examples libreoffice-writer
command -v gnome-text-editor
command -v libreoffice
find /usr/lib64/qt6/examples /usr/lib/qt6/examples \
  -type f -path '*/widgets/lineedits/*/lineedits' -perm /111 \
  -print -quit 2>/dev/null | grep .
