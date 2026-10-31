#!/usr/bin/env python3
"""lan-announce-test.py — does Minecraft's multiplayer screen show a LAN world?

Sends the same multicast announcement a LAN-published Minecraft world (and
MineScale) sends, and nothing else: no tunnel, no MineScale. Open
Minecraft -> Multiplayer and watch for the entry while this runs.

  - entry shows up   -> Minecraft and the OS are fine; look at how MineScale
                        announces (compare its log line with the output here).
  - entry never shows -> the problem is between the OS and Minecraft
                        (macOS "Local Network" permission, VPN, firewall...),
                        not MineScale.

Standard library only (Python 3.8+). Usage:

    python3 scripts/lan-announce-test.py              # ~60 s, TTL 1
    python3 scripts/lan-announce-test.py --ttl 0      # host-local only
    python3 scripts/lan-announce-test.py --name Hello --seconds 120

The announced port is a dummy (it points at nothing); just look for the entry.
"""
import argparse
import re
import socket
import subprocess
import sys
import time

GROUP, PORT = "224.0.2.60", 4445


def local_ipv4_addrs():
    """Every local IPv4 address (loopback included), without third-party deps."""
    out = []
    try:
        if sys.platform == "win32":
            text = subprocess.run(["ipconfig"], capture_output=True, text=True).stdout
            out = re.findall(r"IPv4[^:\n]*:\s*(\d+\.\d+\.\d+\.\d+)", text)
        elif sys.platform == "darwin":
            text = subprocess.run(["ifconfig"], capture_output=True, text=True).stdout
            out = re.findall(r"\binet (\d+\.\d+\.\d+\.\d+)", text)
        else:
            text = subprocess.run(["ip", "-4", "-o", "addr"], capture_output=True, text=True).stdout
            out = re.findall(r"inet (\d+\.\d+\.\d+\.\d+)", text)
    except Exception as e:  # noqa: BLE001
        print(f"(could not list interfaces: {e})")
    return [a for a in dict.fromkeys(out) if not a.startswith("169.254.")]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--name", default="LAN-TEST", help="world name to announce (default: LAN-TEST)")
    ap.add_argument("--port", type=int, default=25565, help="dummy port in the announcement")
    ap.add_argument("--ttl", type=int, default=1, choices=(0, 1), help="multicast TTL (default 1)")
    ap.add_argument("--seconds", type=int, default=60, help="how long to keep announcing")
    ap.add_argument("--no-iface", action="store_true",
                    help="don't pin each interface; let the OS choose (like a plain Minecraft)")
    args = ap.parse_args()

    payload = f"[MOTD]{args.name}[/MOTD][AD]{args.port}[/AD]".encode()
    addrs = [None] if args.no_iface else (local_ipv4_addrs() or [None])

    socks = []
    for ip in addrs:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
        s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
        s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, args.ttl)
        if ip:
            try:
                s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton(ip))
            except OSError as e:
                print(f"  skip {ip}: {e}")
                s.close()
                continue
        socks.append((ip or "(OS default)", s))

    print(f'Announcing "{args.name}" (port {args.port}, TTL {args.ttl}) via: '
          + ", ".join(name for name, _ in socks))
    print(f"Open Minecraft -> Multiplayer now, and look for \"{args.name}\". "
          f"(Stops after {args.seconds}s; Ctrl-C to stop.)")
    end = time.time() + args.seconds
    sent = failed = 0
    try:
        while time.time() < end:
            for name, s in socks:
                try:
                    s.sendto(payload, (GROUP, PORT))
                    sent += 1
                except OSError as e:
                    failed += 1
                    if failed <= 3:
                        print(f"  send failed via {name}: {e}")
            time.sleep(1.5)
    except KeyboardInterrupt:
        pass
    print(f"done: {sent} packets sent, {failed} failed")


if __name__ == "__main__":
    main()
