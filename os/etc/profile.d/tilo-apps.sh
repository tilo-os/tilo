# PATH entries for commands published by installed Tilo packages.
#
# `tilo-install` drops one small shim per `[cli].commands` entry into
# <store>/bin. Without those directories on PATH a package can declare a
# command, install cleanly, and still not be runnable by name — which is the
# only way a CLI package is meant to be run.
#
# Two stores, user first: a package the user installed shadows a preinstalled
# one of the same id in the launcher's registry, and PATH has to agree.
_tilo_user_apps="${XDG_DATA_HOME:-$HOME/.local/share}/tilo-apps/bin"
case ":$PATH:" in
    *":$_tilo_user_apps:"*) ;;
    *) PATH="$_tilo_user_apps:$PATH" ;;
esac
case ":$PATH:" in
    *":/usr/lib/tilo-apps/bin:"*) ;;
    *) PATH="$PATH:/usr/lib/tilo-apps/bin" ;;
esac
# Full Tilo images use /usr/share for the immutable store; the host package
# keeps architecture-dependent executable payloads under /usr/lib.
case ":$PATH:" in
    *":/usr/share/tilo-apps/bin:"*) ;;
    *) PATH="$PATH:/usr/share/tilo-apps/bin" ;;
esac
unset _tilo_user_apps
export PATH
