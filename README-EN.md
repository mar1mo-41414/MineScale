# MineScale-Java

[← 日本語版 README](README.md)

**Play your Minecraft (Java Edition) world with friends. That's it.**

Send one link. Your friend's Minecraft shows your world.

---

## What you do NOT need

❌ Router setup (port forwarding)
❌ An account
❌ A server contract
❌ A monthly fee
❌ A VPN app
❌ Networking knowledge
❌ Realms or any hosting purchase

✅ Open the app
✅ Press a button
✅ Send the link it gives you to a friend

That's all.

Normally you'd need port forwarding, a fixed global IP, a Realms subscription, a VPN app and an
account for it, or your own server to run… which is a lot of hassle.
MineScale-Java does all of that **automatically, behind the scenes**.

---

## How to use (GUI — recommended)

### Host (the person sharing the world)

1. In Minecraft, open your world, press ESC → "Open to LAN"
2. Double-click `mc-share-gui`
3. Press "Host"
4. Copy the link and send it to your friend (chat app of your choice)

### Join (the person joining)

1. Start `mc-share-gui`
2. Open the "Join" tab and paste the link
3. Press "Connect"
4. Open Minecraft → Multiplayer — your friend's world appears by itself
5. Click it to join. Done.

> If it doesn't show up, the "enter the direct address" method in
> [Troubleshooting](docs/TROUBLESHOOTING.md) (Japanese) always works.

---

## Download

👉 **[Download the latest version from the Releases page](https://github.com/mar1mo-41414/MineScale/releases/latest)**

Pick the file for your OS.

| OS | File |
|----|------|
| Windows | `mc-share-gui-windows-x64.exe` |
| Mac (Intel / Apple Silicon) | `mc-share-gui-macos` |
| Linux (x64) | `mc-share-gui-linux-x64` |
| Linux (arm64) | `mc-share-gui-linux-arm64` |

Double-click to run. No installation.

> On Mac / Linux, make it executable once: `chmod +x mc-share-gui-macos`

### On Windows you can also use winget

Open PowerShell and run:

```powershell
winget install minescale
```

That's it — `mc-share-gui` is ready to use.

> If Windows warns that the file is "not commonly downloaded", see
> [Troubleshooting](docs/TROUBLESHOOTING.md) (Japanese). winget avoids the warning.

---

## CLI (for terminal lovers)

```bash
mc-share host            # host
mc-share join <URL>      # join
```

Same features as the GUI.

---

## FAQ

**Q. Is it safe? Doesn't it go through someone's server?**
A. Traffic takes one of two paths, and the app shows **which one is in use**.

- **Direct (P2P)**: a direct connection between you and your friend, encrypted by MineScale.
  The server in the middle only exchanges addresses; it is not part of the game traffic.
- **Relay (via server)**: on networks where a direct connection isn't possible (e.g. some corporate
  networks), traffic goes **through** the server. On this path MineScale does **not** encrypt it
  (the server is built to forward bytes, not store or inspect them, but it does see them pass).
  This path assumes you trust the server's operator.

**Q. Can someone take over my world?**
A. **The link is the only key.** Anyone who has it can join for about 15 minutes after it was created
(there is no approval step, player cap, or way to revoke a link). Don't post it publicly.
The world is visible **only on the joining computer itself** — other devices on the same network
don't see it and their connections are refused.

**Q. A friend couldn't join later.**
A. New players can join only during the first ~15 minutes after the link was created. After that,
press "Share World" again to get a new link (players already connected keep playing).

**Q. Is there lag?**
A. On Direct (P2P) there is almost no added latency. On Relay, traffic passes through the server once,
which adds some latency.

**Q. Does it cost money? Do I need an account?**
A. No. No email address or phone number is needed either.

**Q. What happens when the host closes Minecraft?**
A. Same as a normal LAN game: everyone is disconnected.

**Q. How many players?**
A. The same limit as Minecraft's "Open to LAN" (depends on the host PC and connection).

---

## Supported OS

- Windows 10 / 11
- macOS (Intel / Apple Silicon)
- Linux (x64 / arm64)

Tested combinations are listed in [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) (Japanese).

---

## More

- [Troubleshooting](docs/TROUBLESHOOTING.md) (Japanese)
- [Compatibility](docs/COMPATIBILITY.md) (Japanese)
- [Technical spec (SPEC.md)](SPEC.md) (Japanese)
- [Build instructions (BUILD.md)](BUILD.md) (Japanese)
- [Development history (HISTORY.md)](HISTORY.md) (Japanese)

## License

MIT

---

## Helping with connection research (optional, off by default)

> This section is **never shown inside the app**. The app never asks you to take part.

To improve which NAT setups and OSes connect successfully, MineScale-Java can send
connection diagnostics — **only if you opt in**. It is **off by default**, and it can't be
turned on from a button or notification inside the app.

If you'd like to help, turn it on at launch:

```bash
# CLI
mc-share host --telemetry
mc-share join <URL> --telemetry

# GUI (flag or environment variable)
./mc-share-gui --telemetry
MC_SHARE_TELEMETRY=1 ./mc-share-gui      # not on Windows
```

> On Windows, if you launch by double-clicking, the easiest way is to create a shortcut and add
> `--telemetry` to the "Target" field.

### What is sent (this and nothing else)

| Field | Example | Purpose |
|-------|---------|---------|
| `session_id` | random 16 chars | ties start/result events together within one session (not persisted) |
| `room_id` | end of the share link | pairs host and join (rooms vanish after 15 minutes) |
| `role` | `host` / `join` | which side |
| `phase` | `start` / `registered` / `connected` / `result` | progress milestones (sent right when reached) |
| `outcome` | `success` / `cancelled` / `stun_failed` / `punch_failed` / `tls_failed` / ... | success/failure category (`result` only) |
| `transport` | `quic` / `relay` | the path actually used (once connected) |
| `duration_ms` | `3421` | time from start to result |
| `nat_type` | `Cone NAT (P2P ready)` etc. | NAT type (same judgement as the Diag tab) |
| `ipv6_available` | `true` / `false` | whether an IPv6 route exists |
| `os` / `os_detail` | `macos` / `macOS 14` | OS family and major version |
| `arch` | `aarch64` / `x86_64` | CPU architecture |
| `app_version` | `1.1.0` | app version |
| `app_kind` | `cli` / `gui` | front-end |

### What is NOT sent (deliberately excluded)

- ❌ User name, host name, MAC address, email address
- ❌ Your global IP / local IP / external address (including STUN results)
- ❌ The list of installed Minecraft versions
- ❌ OS minor / build numbers (e.g. "14.5.1 (23F79)" is rounded to "14")
- ❌ The content of game data
- ❌ Any persistent device identifier

### How the server handles it

- The **source IP address of the request is not recorded**
- HTTP request headers are not recorded either
- The server only adds a receive timestamp to the JSON body and appends it to a JSONL file
- Room IDs expire after 15 minutes, so there is no long-term way to re-identify you

### Purpose

- Gather statistics on which NAT type × OS combinations connect successfully
- Narrow down the conditions that reproduce failures
- Help improve stability (hole-punching logic, fallback tuning)

### No conflict of interest

- Not used for advertising or marketing
- Not shared with third parties
- Data is deleted after aggregation

### How to stop

- CLI: just drop the flag or environment variable
- GUI: launch without setting `MC_SHARE_TELEMETRY`
- To have data you already sent deleted, open a GitHub Issue with your `session_id`
  (it appears in the app log, e.g. with `RUST_LOG=debug`)
