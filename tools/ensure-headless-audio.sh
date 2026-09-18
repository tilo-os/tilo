#!/usr/bin/env bash
# Supply the private PulseAudio test daemon inside an ephemeral CI container.
# Older builders contain the conflicting PipeWire PulseAudio service instead.
set -euo pipefail

if command -v pulseaudio >/dev/null; then exit 0; fi
if [[ ${GITHUB_ACTIONS:-} != true ]] || ! { test -f /.dockerenv || test -f /run/.containerenv; }; then
    echo 'audio dependency installation is restricted to CI containers' >&2
    exit 1
fi

# Keep every installed package pinned except the explicitly replaced daemon.
# In particular, do not update Mesa, fonts or libpulse in golden-image jobs.
installed=$(rpm -qa --qf '%{NAME}\n' | sort -u | grep -vx pipewire-pulseaudio | paste -sd, -)
options=(-y --setopt=install_weak_deps=False --setopt=obsoletes=False
    --setopt=best=False --setopt=clean_requirements_on_remove=False
    "--exclude=$installed")
if rpm -q pipewire-pulseaudio >/dev/null 2>&1; then
    dnf swap "${options[@]}" pipewire-pulseaudio pulseaudio
else
    dnf install "${options[@]}" pulseaudio
fi
command -v pulseaudio >/dev/null || {
    echo 'PulseAudio installation did not supply the required test daemon' >&2
    exit 1
}
