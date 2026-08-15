# PATH entries for commands published by installed qOS packages.
#
# `qos-install` drops one small shim per `[cli].commands` entry into
# <store>/bin. Without those directories on PATH a package can declare a
# command, install cleanly, and still not be runnable by name — which is the
# only way a CLI package is meant to be run.
#
# Two stores, user first: a package the user installed shadows a preinstalled
# one of the same id in the launcher's registry, and PATH has to agree.
_qos_user_apps="${XDG_DATA_HOME:-$HOME/.local/share}/qos-apps/bin"
case ":$PATH:" in
    *":$_qos_user_apps:"*) ;;
    *) PATH="$_qos_user_apps:$PATH" ;;
esac
case ":$PATH:" in
    *":/usr/share/qos-apps/bin:"*) ;;
    *) PATH="$PATH:/usr/share/qos-apps/bin" ;;
esac
unset _qos_user_apps
export PATH
