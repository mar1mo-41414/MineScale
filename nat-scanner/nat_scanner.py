#!/usr/bin/env python3
"""
nat-scanner — measure NAT behaviour and score P2P suitability.

The tool does NOT try to classify your NAT into a canonical bucket
(full-cone, restricted-cone, port-restricted, symmetric). Instead it
runs two empirical tests and reports three numeric scores plus an
overall P2P-suitability rating:

  TEST A — peer dependence
      One UDP socket, sequential STUN bindings against several public
      STUN servers (different IPs). If the NAT reports the same
      external port to every server, the mapping is independent of the
      destination — hole-punching has a chance. If the port changes
      per destination, this is symmetric-NAT behaviour and direct P2P
      is essentially impossible.

  TEST B — temporal stability
      The same UDP socket queries one STUN server repeatedly with a
      small gap. If the external port stays put, an out-of-band peer
      who learns the port (e.g. via a coordination server) can connect
      to it for at least the duration of the experiment.

Outputs both a JSON report (for machine consumption / data collection)
and a human summary.

Pure Python 3 stdlib — no external dependencies.

Usage:
    python3 nat_scanner.py [options]

Options:
    --servers HOST:PORT,HOST:PORT,...
        Comma-separated STUN servers for TEST A. At least two should be
        on distinct IPs for the test to be meaningful. Defaults to a
        mixed set of Google, Cloudflare, Nextcloud, etc.
    --samples N
        Number of stability samples for TEST B (default: 10).
    --interval-ms MS
        Gap between stability samples (default: 500).
    --timeout-s SEC
        Per-STUN-query timeout (default: 2.5).
    --json [PATH]
        Emit JSON report. If PATH is given, write to file; otherwise
        print to stdout in addition to the human summary.
    --quiet
        Suppress the human summary; only emit JSON.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import socket
import struct
import sys
import time
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from typing import List, Optional, Tuple


# ── STUN constants (RFC 5389) ────────────────────────────────────────────────

STUN_MAGIC_COOKIE = 0x2112_A442
STUN_BINDING_REQUEST = b"\x00\x01"
STUN_BINDING_SUCCESS = b"\x01\x01"
ATTR_XOR_MAPPED_ADDRESS = 0x0020
ATTR_MAPPED_ADDRESS = 0x0001  # legacy, RFC 3489

DEFAULT_STUN_SERVERS = [
    "stun.l.google.com:19302",
    "stun1.l.google.com:19302",
    "stun.cloudflare.com:3478",
    "stun.nextcloud.com:443",
    "stun.miwifi.com:3478",
]


# ── Result types ─────────────────────────────────────────────────────────────

@dataclass
class Probe:
    server: str
    local_port: int
    external_ip: Optional[str]
    external_port: Optional[int]
    rtt_ms: float
    error: Optional[str] = None

    def to_dict(self) -> dict:
        return asdict(self)


@dataclass
class TestResult:
    name: str
    probes: List[Probe] = field(default_factory=list)
    unique_external_ports: int = 0
    unique_external_ips: int = 0
    successful_probes: int = 0

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "probes": [p.to_dict() for p in self.probes],
            "unique_external_ports": self.unique_external_ports,
            "unique_external_ips": self.unique_external_ips,
            "successful_probes": self.successful_probes,
        }


# ── Raw STUN client ──────────────────────────────────────────────────────────

def _build_binding_request() -> Tuple[bytes, bytes]:
    """Return (request_bytes, transaction_id)."""
    tid = os.urandom(12)
    pkt = bytearray(20)
    pkt[0:2] = STUN_BINDING_REQUEST
    pkt[2:4] = (0).to_bytes(2, "big")  # message length (no attributes)
    pkt[4:8] = STUN_MAGIC_COOKIE.to_bytes(4, "big")
    pkt[8:20] = tid
    return bytes(pkt), tid


def _parse_xor_mapped(value: bytes) -> Optional[Tuple[str, int]]:
    if len(value) < 8:
        return None
    family = value[1]
    if family != 0x01:  # only IPv4
        return None
    xport = int.from_bytes(value[2:4], "big")
    port = xport ^ (STUN_MAGIC_COOKIE >> 16)
    xip = int.from_bytes(value[4:8], "big")
    ip_int = xip ^ STUN_MAGIC_COOKIE
    ip = ".".join(str((ip_int >> (24 - 8 * i)) & 0xFF) for i in range(4))
    return ip, port


def _parse_mapped(value: bytes) -> Optional[Tuple[str, int]]:
    if len(value) < 8:
        return None
    family = value[1]
    if family != 0x01:
        return None
    port = int.from_bytes(value[2:4], "big")
    ip = ".".join(str(value[4 + i]) for i in range(4))
    return ip, port


def _parse_response(data: bytes, expected_tid: bytes) -> Tuple[str, int]:
    if len(data) < 20:
        raise RuntimeError(f"STUN response too short ({len(data)} bytes)")
    if data[0:2] != STUN_BINDING_SUCCESS:
        raise RuntimeError(f"unexpected STUN message type {data[0:2].hex()}")
    cookie = int.from_bytes(data[4:8], "big")
    if cookie != STUN_MAGIC_COOKIE:
        raise RuntimeError("bad magic cookie")
    if data[8:20] != expected_tid:
        raise RuntimeError("transaction id mismatch")

    msg_len = int.from_bytes(data[2:4], "big")
    end = min(20 + msg_len, len(data))
    i = 20
    while i + 4 <= end:
        atype = int.from_bytes(data[i:i + 2], "big")
        alen = int.from_bytes(data[i + 2:i + 4], "big")
        i += 4
        if i + alen > end:
            break
        value = bytes(data[i:i + alen])
        i += (alen + 3) & ~3  # 4-byte aligned

        if atype == ATTR_XOR_MAPPED_ADDRESS:
            r = _parse_xor_mapped(value)
            if r is not None:
                return r
        elif atype == ATTR_MAPPED_ADDRESS:
            r = _parse_mapped(value)
            if r is not None:
                return r
    raise RuntimeError("no MAPPED-ADDRESS attribute in response")


def stun_query(sock: socket.socket, server: str, timeout_s: float) -> Tuple[str, int, float]:
    """Send a STUN Binding Request on `sock` to `server` ("host:port").
    Returns (external_ip, external_port, rtt_ms).
    Raises RuntimeError on failure.
    """
    host, _, port_s = server.rpartition(":")
    if not port_s:
        raise RuntimeError(f"invalid STUN server: {server!r}")
    try:
        port = int(port_s)
    except ValueError:
        raise RuntimeError(f"invalid port in STUN server: {server!r}")

    try:
        addrs = socket.getaddrinfo(host, port, socket.AF_INET, socket.SOCK_DGRAM)
    except socket.gaierror as e:
        raise RuntimeError(f"DNS resolution failed: {e}")
    if not addrs:
        raise RuntimeError("no IPv4 address for STUN server")
    target = addrs[0][4]

    req, tid = _build_binding_request()
    sock.settimeout(timeout_s)

    # Retry up to 3 times — UDP is lossy.
    last_err: Optional[Exception] = None
    for attempt in range(3):
        t0 = time.monotonic()
        try:
            sock.sendto(req, target)
            while True:
                data, src = sock.recvfrom(2048)
                # Accept only responses from the target host (port can drift
                # across STUN load balancers in some deployments, but the IP
                # should match the one we sent to).
                if src[0] != target[0]:
                    continue
                rtt_ms = (time.monotonic() - t0) * 1000.0
                ip, port_out = _parse_response(data, tid)
                return ip, port_out, rtt_ms
        except socket.timeout as e:
            last_err = e
            continue
        except Exception as e:
            last_err = e
            break
    raise RuntimeError(f"STUN query failed after retries: {last_err}")


# ── Tests ────────────────────────────────────────────────────────────────────

def _new_socket(bind_port: int = 0) -> socket.socket:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", bind_port))
    return s


def test_peer_dependence(servers: List[str], timeout_s: float) -> TestResult:
    """One socket, query every server. Same external port = peer-independent."""
    res = TestResult(name="peer_dependence")
    sock = _new_socket()
    local_port = sock.getsockname()[1]
    try:
        for srv in servers:
            try:
                ip, port, rtt = stun_query(sock, srv, timeout_s)
                res.probes.append(Probe(server=srv, local_port=local_port,
                                        external_ip=ip, external_port=port,
                                        rtt_ms=rtt))
            except Exception as e:
                res.probes.append(Probe(server=srv, local_port=local_port,
                                        external_ip=None, external_port=None,
                                        rtt_ms=0.0, error=str(e)))
    finally:
        sock.close()

    ok = [p for p in res.probes if p.external_port is not None]
    res.successful_probes = len(ok)
    res.unique_external_ports = len({p.external_port for p in ok})
    res.unique_external_ips = len({p.external_ip for p in ok})
    return res


def test_temporal_stability(server: str, samples: int, interval_ms: int,
                            timeout_s: float) -> TestResult:
    """Same socket, same server, repeated queries spaced by interval_ms."""
    res = TestResult(name="temporal_stability")
    sock = _new_socket()
    local_port = sock.getsockname()[1]
    try:
        for _ in range(samples):
            try:
                ip, port, rtt = stun_query(sock, server, timeout_s)
                res.probes.append(Probe(server=server, local_port=local_port,
                                        external_ip=ip, external_port=port,
                                        rtt_ms=rtt))
            except Exception as e:
                res.probes.append(Probe(server=server, local_port=local_port,
                                        external_ip=None, external_port=None,
                                        rtt_ms=0.0, error=str(e)))
            time.sleep(interval_ms / 1000.0)
    finally:
        sock.close()

    ok = [p for p in res.probes if p.external_port is not None]
    res.successful_probes = len(ok)
    res.unique_external_ports = len({p.external_port for p in ok})
    res.unique_external_ips = len({p.external_ip for p in ok})
    return res


# ── Scoring ──────────────────────────────────────────────────────────────────

def score_consistency(test_a: TestResult) -> int:
    """100 = all external ports the same across distinct STUN servers.
    0   = every server saw a different port.
    """
    n = test_a.successful_probes
    if n <= 0:
        return 0
    if n == 1:
        # Only one data point — we can't tell. Be optimistic.
        return 100
    u = test_a.unique_external_ports
    # Linear: 1 unique → 100, n unique → 0.
    return round(100 * (n - u) / (n - 1))


def score_peer_dependence(test_a: TestResult) -> int:
    """Inverse of consistency — high means port heavily depends on peer."""
    return 100 - score_consistency(test_a)


def score_stability(test_b: TestResult) -> int:
    """100 = same external port observed every sample. 0 = every sample
    yielded a different port (binding flapping)."""
    n = test_b.successful_probes
    if n <= 0:
        return 0
    if n == 1:
        return 100
    u = test_b.unique_external_ports
    return round(100 * (n - u) / (n - 1))


def overall_p2p_score(consistency: int, stability: int) -> int:
    """Weighted average. Consistency dominates — symmetric NAT is the
    single biggest killer of P2P. Stability matters less but rapid
    rebinding still hurts hole-punch reliability."""
    return round(0.7 * consistency + 0.3 * stability)


def verdict(p2p_score: int, consistency: int, stability: int) -> str:
    if consistency < 50:
        return ("Symmetric-NAT-like behaviour. Direct P2P is essentially "
                "impossible from this network — applications will need to "
                "fall back to a relay.")
    if stability < 50:
        return ("Cone-like mapping but the binding flaps over time. "
                "Hole-punching may succeed initially and then break "
                "mid-session; a relay fallback is recommended.")
    if p2p_score >= 90:
        return ("Excellent. The external port is stable and independent "
                "of the destination. Direct P2P hole-punching is "
                "expected to succeed reliably.")
    if p2p_score >= 70:
        return ("Good. Most P2P attempts should succeed. Occasional "
                "fallback to a relay is possible under load.")
    return ("Marginal. P2P may work intermittently. Plan for relay "
            "fallback for reliability.")


# ── Report assembly ──────────────────────────────────────────────────────────

@dataclass
class Report:
    scanned_at: str
    hostname: str
    test_peer_dependence: TestResult
    test_temporal_stability: TestResult
    scores: dict
    verdict: str

    def to_dict(self) -> dict:
        return {
            "scanned_at": self.scanned_at,
            "hostname": self.hostname,
            "tests": {
                "peer_dependence": self.test_peer_dependence.to_dict(),
                "temporal_stability": self.test_temporal_stability.to_dict(),
            },
            "scores": self.scores,
            "verdict": self.verdict,
        }


def build_report(servers: List[str], samples: int, interval_ms: int,
                 timeout_s: float) -> Report:
    test_a = test_peer_dependence(servers, timeout_s)

    # Pick the first server that worked in test A as the stability target.
    primary = next((p.server for p in test_a.probes if p.external_port is not None),
                   servers[0])
    test_b = test_temporal_stability(primary, samples, interval_ms, timeout_s)

    consistency = score_consistency(test_a)
    peer_dep = score_peer_dependence(test_a)
    stability = score_stability(test_b)
    p2p = overall_p2p_score(consistency, stability)

    return Report(
        scanned_at=datetime.now(timezone.utc).isoformat(timespec="seconds"),
        hostname=socket.gethostname(),
        test_peer_dependence=test_a,
        test_temporal_stability=test_b,
        scores={
            "port_consistency": consistency,
            "peer_dependence": peer_dep,
            "stability": stability,
            "p2p_score": p2p,
        },
        verdict=verdict(p2p, consistency, stability),
    )


# ── Human-readable summary ───────────────────────────────────────────────────

def stars(score: int) -> str:
    filled = min(5, max(0, round(score / 20)))
    return "★" * filled + "☆" * (5 - filled)


def render_summary(r: Report) -> str:
    s = []
    s.append(f"NAT Behaviour Scanner — {r.scanned_at}")
    s.append(f"Host: {r.hostname}")
    s.append("")

    ext_ips = {p.external_ip for p in r.test_peer_dependence.probes
               if p.external_ip is not None}
    s.append(f"External IPv4 seen: {', '.join(sorted(ext_ips)) or '(none)'}")
    s.append("")

    s.append("TEST A — peer dependence  (one socket → many STUN servers)")
    s.append("-" * 70)
    for p in r.test_peer_dependence.probes:
        if p.error:
            s.append(f"  ✗ {p.server:36}  ERROR: {p.error}")
        else:
            s.append(f"  ✓ {p.server:36}  ext={p.external_ip}:{p.external_port}"
                     f"  (rtt {p.rtt_ms:.0f}ms)")
    a = r.test_peer_dependence
    s.append(f"  → {a.successful_probes} ok, "
             f"{a.unique_external_ports} unique external port(s), "
             f"{a.unique_external_ips} unique external IP(s)")
    s.append("")

    s.append("TEST B — temporal stability  (same server, repeated)")
    s.append("-" * 70)
    for p in r.test_temporal_stability.probes:
        if p.error:
            s.append(f"  ✗ {p.server:36}  ERROR: {p.error}")
        else:
            s.append(f"  ✓ {p.server:36}  ext={p.external_ip}:{p.external_port}"
                     f"  (rtt {p.rtt_ms:.0f}ms)")
    b = r.test_temporal_stability
    s.append(f"  → {b.successful_probes} ok, "
             f"{b.unique_external_ports} unique external port(s)")
    s.append("")

    s.append("Scores (0 = worst for P2P, 100 = best)")
    s.append("-" * 70)
    sc = r.scores
    s.append(f"  port_consistency  : {sc['port_consistency']:3}  {stars(sc['port_consistency'])}")
    s.append(f"  peer_dependence   : {sc['peer_dependence']:3}  {stars(100 - sc['peer_dependence'])}"
             f"  (lower is better)")
    s.append(f"  stability         : {sc['stability']:3}  {stars(sc['stability'])}")
    s.append(f"  ───────────────────")
    s.append(f"  p2p_score         : {sc['p2p_score']:3}  {stars(sc['p2p_score'])}")
    s.append("")
    s.append(f"Verdict: {r.verdict}")
    return "\n".join(s)


# ── CLI ──────────────────────────────────────────────────────────────────────

def main(argv: Optional[List[str]] = None) -> int:
    p = argparse.ArgumentParser(
        prog="nat-scanner",
        description="Measure NAT behaviour and score P2P suitability.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    p.add_argument("--servers", default=",".join(DEFAULT_STUN_SERVERS),
                   help="Comma-separated STUN servers (host:port,...)")
    p.add_argument("--samples", type=int, default=10,
                   help="Number of stability samples for TEST B (default: 10)")
    p.add_argument("--interval-ms", type=int, default=500,
                   help="Gap between stability samples in ms (default: 500)")
    p.add_argument("--timeout-s", type=float, default=2.5,
                   help="Per-STUN-query timeout in seconds (default: 2.5)")
    p.add_argument("--json", nargs="?", const="-", default=None, metavar="PATH",
                   help="Emit JSON report (to stdout if no path; to file otherwise)")
    p.add_argument("--quiet", action="store_true",
                   help="Suppress the human summary; only emit JSON.")
    args = p.parse_args(argv)

    servers = [s.strip() for s in args.servers.split(",") if s.strip()]
    if len(servers) < 2:
        print("nat-scanner: need at least 2 STUN servers for peer-dependence "
              "test (got {}). See --servers."
              .format(len(servers)), file=sys.stderr)
        return 2

    report = build_report(
        servers=servers,
        samples=args.samples,
        interval_ms=args.interval_ms,
        timeout_s=args.timeout_s,
    )

    if not args.quiet:
        print(render_summary(report))

    if args.json is not None:
        payload = json.dumps(report.to_dict(), indent=2, ensure_ascii=False)
        if args.json == "-":
            if not args.quiet:
                print()  # separator
            print(payload)
        else:
            with open(args.json, "w", encoding="utf-8") as fh:
                fh.write(payload + "\n")
            if not args.quiet:
                print(f"\nJSON report written to: {args.json}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
