# qOS PATH shim — surface CLI commands from installed .qpkg packages.
#
# `qos-install install <pkg.qpkg>` writes PATH-visible command shims
# into `$HOME/.local/share/qos-apps/bin/` (see the `[cli]` block of
# the manifest). This file adds that directory to $PATH at login so
# the shims Just Work in interactive shells and TTYs.
#
# Sourced by /etc/profile for login shells (bash, zsh, dash, sh).
# Non-login interactive shells rely on the sibling drop-in in
# /etc/bashrc.d/qos-apps-path.sh — kept identical so both flows agree.

if [ -d "$HOME/.local/share/qos-apps/bin" ]; then
    case ":$PATH:" in
        *":$HOME/.local/share/qos-apps/bin:"*) ;;
        *) PATH="$HOME/.local/share/qos-apps/bin:$PATH" ;;
    esac
    export PATH
fi
