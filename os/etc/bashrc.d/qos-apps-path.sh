# qOS PATH shim — non-login interactive bash sibling of
# /etc/profile.d/qos-apps-path.sh. Both files must stay in sync.
#
# Fedora's /etc/bashrc sources every /etc/bashrc.d/*.sh file for
# interactive non-login shells (the same VM/terminal case that
# /etc/profile.d/ does NOT catch). Users spawning a shell inside
# an already-logged-in session hit this path; login shells hit
# the profile.d twin.

if [ -d "$HOME/.local/share/qos-apps/bin" ]; then
    case ":$PATH:" in
        *":$HOME/.local/share/qos-apps/bin:"*) ;;
        *) PATH="$HOME/.local/share/qos-apps/bin:$PATH" ;;
    esac
    export PATH
fi
