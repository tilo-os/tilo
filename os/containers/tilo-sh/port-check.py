#!/usr/bin/env python3
"""Which of a stack's ports this machine would not give it, and where its
temporary ports begin.

stack.sh checks this here and, through its cargo drop-in, on the build server: a
port base whose ports reach into the machine's temporary-port range is one
outgoing connections can take at any moment, and a base another stack already
holds is not this stack's. It reads what the kernel says (/proc/net/tcp,
/proc/net/udp) instead of binding, so it names the holder and leaves every
port as it found it.

usage: port-check.py [--range] [--tcp PORTS] [--udp PORTS]

PORTS are comma-separated numbers. Prints, one per line:

  range LOW HIGH            the ports this machine hands to outgoing connections
                            ("range unknown" where its kernel does not say)
  held PROTO PORT by PID (CMD)
                            a socket already on PORT of that protocol, and the
                            process holding it ("by a process this account
                            cannot see" where it cannot be named)
"""

import os


def temporary_range():
    """The ports this machine hands to outgoing connections, or None."""
    try:
        with open("/proc/sys/net/ipv4/ip_local_port_range") as kernel:
            low, high = (int(part) for part in kernel.read().split())
        return low, high
    except (OSError, ValueError):
        pass
    # macOS and the BSDs keep it in sysctl.
    try:
        import subprocess

        low = int(
            subprocess.check_output(
                ["sysctl", "-n", "net.inet.ip.portrange.first"], text=True
            ).strip()
        )
        high = int(
            subprocess.check_output(
                ["sysctl", "-n", "net.inet.ip.portrange.last"], text=True
            ).strip()
        )
        return low, high
    except Exception:
        return None


def holding_inodes(port, proto):
    """The sockets that hold PORT of PROTO as a local port."""
    if proto == "tcp":
        paths = ("/proc/net/tcp", "/proc/net/tcp6")
        states = ("0A",)  # listening
    else:
        paths = ("/proc/net/udp", "/proc/net/udp6")
        states = ("07", "01")  # unconnected and connected both hold the port
    inodes = set()
    for path in paths:
        try:
            with open(path) as table:
                next(table, None)  # the column names
                for line in table:
                    fields = line.split()
                    if len(fields) < 10:
                        continue
                    local, state, inode = fields[1], fields[3], fields[9]
                    if state not in states or ":" not in local:
                        continue
                    try:
                        if int(local.rsplit(":", 1)[1], 16) != port:
                            continue
                    except ValueError:
                        continue
                    inodes.add(inode)
        except OSError:
            continue
    return inodes


def holder(inodes):
    """The process holding one of these sockets, as (pid, command)."""
    if not inodes:
        return None
    targets = {"socket:[%s]" % inode for inode in inodes}
    for entry in sorted((e for e in os.listdir("/proc") if e.isdigit()), key=int):
        directory = "/proc/%s/fd" % entry
        try:
            fds = os.listdir(directory)
        except OSError:
            continue
        for fd in fds:
            try:
                if os.readlink(os.path.join(directory, fd)) not in targets:
                    continue
                with open("/proc/%s/cmdline" % entry, "rb") as cmdline:
                    command = cmdline.read().replace(b"\0", b" ").decode(
                        "utf-8", "replace"
                    )
            except OSError:
                continue
            command = command.strip() or "?"
            # A forward's command line names every port it carries: enough to
            # know it, not a paragraph in an error message.
            if len(command) > 120:
                command = command[:117] + "..."
            return entry, command
    return None


def main(argv):
    want_range = False
    ports = {"tcp": [], "udp": []}
    args = iter(argv)
    for arg in args:
        if arg == "--range":
            want_range = True
        elif arg in ("--tcp", "--udp"):
            try:
                value = next(args)
            except StopIteration:
                return "port-check.py: %s needs a comma-separated list" % arg
            for item in value.split(","):
                item = item.strip()
                if not item:
                    continue
                try:
                    port = int(item)
                except ValueError:
                    return "port-check.py: %r is not a port" % item
                if not 1 <= port <= 65535:
                    return "port-check.py: %r is not a port" % item
                ports[arg[2:]].append(port)
        else:
            return "port-check.py: unknown option %r" % arg
    if want_range:
        span = temporary_range()
        if span is None:
            print("range unknown")
        else:
            print("range %d %d" % span)
    for proto in ("tcp", "udp"):
        for port in ports[proto]:
            inodes = holding_inodes(port, proto)
            if not inodes:
                continue
            named = holder(inodes)
            if named is None:
                print(
                    "held %s %d by a process this account cannot see" % (proto, port)
                )
            else:
                print("held %s %d by pid %s (%s)" % (proto, port, named[0], named[1]))
    return 0


if __name__ == "__main__":
    import sys

    sys.exit(main(sys.argv[1:]))
