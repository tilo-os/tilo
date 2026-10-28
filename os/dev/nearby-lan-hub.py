#!/usr/bin/env python3
"""The nearby gate's shared Ethernet segment for more than two VMs.

Each VM's segment NIC is a QEMU UDP socket netdev on loopback
(os/dev/_common.sh, TILO_VM_LAN_*) whose peer is this hub: every frame one VM
sends reaches every other, broadcasts and ARP included, as on a home network's
switch flooding them. Runs until its stdin closes.

Usage: nearby-lan-hub.py HUB_PORT VM_PORT...
"""
import os
import select
import socket
import sys


def main():
    hub, *ports = (int(value) for value in sys.argv[1:])
    if len(ports) < 2:
        raise SystemExit(__doc__)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", hub))
    while True:
        ready, _, _ = select.select([sock, sys.stdin], [], [])
        if sys.stdin in ready and not os.read(sys.stdin.fileno(), 4096):
            return
        if sock in ready:
            frame, (host, port) = sock.recvfrom(65536)
            if host != "127.0.0.1" or port not in ports:
                continue
            for other in ports:
                if other != port:
                    sock.sendto(frame, ("127.0.0.1", other))


if __name__ == "__main__":
    main()
