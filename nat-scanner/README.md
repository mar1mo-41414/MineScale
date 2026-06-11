# nat-scanner

Measure NAT behaviour empirically and score how likely UDP P2P
hole-punching is to succeed from this network.

This is a standalone diagnostic tool, separate from MineScale-Java
itself. It does **not** call into the mc-share libraries; it only
talks to public STUN servers and reports what they observed.

## Approach

The tool deliberately does not try to map the result onto the
classical NAT taxonomy (full-cone, restricted-cone, port-restricted
cone, symmetric). That taxonomy was useful in 2003 but real-world
middleboxes are messier than the four buckets imply. Instead we run
two empirical tests and report three numeric scores plus an overall
P2P-suitability rating.

### TEST A — peer dependence

One UDP socket, sequential STUN Binding Requests against **several
distinct STUN servers** (different public IPs).

- If the NAT reports the **same external port** to every server, the
  mapping is **peer-independent**: a remote party who learns the port
  via an out-of-band channel (e.g. a coordination server) can attempt
  to send packets to it, and the NAT will accept them after a
  matching outbound packet (= hole punch).
- If the external port **differs per destination**, the mapping is
  **peer-dependent** (a.k.a. *symmetric NAT*). The remote party
  cannot guess which port to send to, and direct hole-punching
  becomes essentially impossible.

### TEST B — temporal stability

The same UDP socket queries one STUN server **repeatedly with a small
gap** (default 500 ms × 10 samples).

- If the external port stays put, the NAT mapping is alive for at
  least the duration of the experiment.
- If the port flaps between queries, the binding is being torn down
  faster than the next probe arrives — hole-punching may succeed
  briefly and then break.

## Scoring

Each score is in `[0, 100]` — higher is better for P2P.

| Score | Meaning |
|-------|---------|
| `port_consistency` | TEST A: how often the same external port was observed across different servers. 100 = identical for all. |
| `peer_dependence`  | `100 − port_consistency`. **Lower is better.** 0 = port independent of peer (great). 100 = port fully determined by peer (symmetric NAT). |
| `stability`        | TEST B: how often the same external port was observed across time. 100 = stable. |
| `p2p_score`        | Weighted overall: `0.7 × port_consistency + 0.3 × stability`. |

The verdict text mirrors the scores:

```
p2p_score ≥ 90   → "Excellent. Direct P2P expected to succeed reliably."
p2p_score ≥ 70   → "Good. Most P2P attempts should succeed."
consistency < 50 → "Symmetric-NAT-like. Direct P2P essentially impossible."
stability   < 50 → "Mapping flaps over time. Relay fallback recommended."
```

## Usage

Requires Python 3.8+ (Ubuntu 20.04 and newer ship it by default).
No dependencies — pure stdlib.

```bash
# Smoke test — human summary to stdout
python3 nat_scanner.py

# Faster run (fewer stability samples, shorter gap)
python3 nat_scanner.py --samples 5 --interval-ms 200

# Use a custom set of STUN servers (need at least two on distinct IPs)
python3 nat_scanner.py --servers stun.l.google.com:19302,stun.cloudflare.com:3478

# Machine-readable output
python3 nat_scanner.py --quiet --json report.json

# Both formats at once
python3 nat_scanner.py --json -
```

Full options:

```
--servers HOST:PORT,...   Comma-separated STUN servers for TEST A.
--samples N               Number of stability samples (default: 10).
--interval-ms MS          Gap between stability samples (default: 500).
--timeout-s SEC           Per-STUN-query timeout (default: 2.5).
--json [PATH]             Emit JSON; "-" or no arg = stdout.
--quiet                   Suppress the human summary.
```

## Sample output (this machine, currently on a NAT-heavy network)

```
NAT Behaviour Scanner — 2026-06-08T01:09:54+00:00
Host: MarkBookM4

External IPv4 seen: 183.76.205.128

TEST A — peer dependence  (one socket → many STUN servers)
----------------------------------------------------------------------
  ✓ stun.l.google.com:19302               ext=183.76.205.128:55956  (rtt 28ms)
  ✓ stun1.l.google.com:19302              ext=183.76.205.128:55956  (rtt 23ms)
  ✓ stun.cloudflare.com:3478              ext=183.76.205.128:31767  (rtt 30ms)
  ✓ stun.nextcloud.com:443                ext=183.76.205.128:14251  (rtt 276ms)
  ✓ stun.miwifi.com:3478                  ext=183.76.205.128:31769  (rtt 111ms)
  → 5 ok, 4 unique external port(s), 1 unique external IP(s)

TEST B — temporal stability  (same server, repeated)
----------------------------------------------------------------------
  ✓ stun.l.google.com:19302               ext=183.76.205.128:43823  (rtt 23ms)
  ✓ stun.l.google.com:19302               ext=183.76.205.128:43823  (rtt 94ms)
  ✓ stun.l.google.com:19302               ext=183.76.205.128:43823  (rtt 20ms)
  ✓ stun.l.google.com:19302               ext=183.76.205.128:43823  (rtt 16ms)
  ✓ stun.l.google.com:19302               ext=183.76.205.128:43823  (rtt 96ms)
  → 5 ok, 1 unique external port(s)

Scores (0 = worst for P2P, 100 = best)
----------------------------------------------------------------------
  port_consistency  :  25  ★☆☆☆☆
  peer_dependence   :  75  ★☆☆☆☆  (lower is better)
  stability         : 100  ★★★★★
  ───────────────────
  p2p_score         :  48  ★★☆☆☆

Verdict: Symmetric-NAT-like behaviour. Direct P2P is essentially impossible
from this network — applications will need to fall back to a relay.
```

Interpretation: the binding is rock-solid over time (stability=100), so
once a peer has the right port the connection survives. But the port
*changes per destination* (consistency=25), which is the textbook
signature of symmetric NAT — no off-net peer can predict the right
port without the cooperation of a third party (relay or coordination
server with bidirectional probing).

## Default STUN servers

The default set tries to mix providers and ASNs so any one outage
doesn't kill the test:

- `stun.l.google.com:19302`
- `stun1.l.google.com:19302`
- `stun.cloudflare.com:3478`
- `stun.nextcloud.com:443`
- `stun.miwifi.com:3478`

For peer-dependence to be meaningful, at least two of these must
resolve to distinct IPs. If a particular STUN server is blocked or
rate-limits you, swap it out with `--servers`.

## License

MIT (same as MineScale-Java).
