# MineScale-Java — Claude Instructions

## ビルドルール (重要)

**変更を加えて再ビルドする際は、必ず全プラットフォーム向けに再ビルドすること。**

```bash
./scripts/build-all.sh
```

### ターゲット一覧

| 成果物 | ターゲット |
|--------|-----------|
| Server | `x86_64-unknown-linux-musl` |
| Server | `aarch64-unknown-linux-musl` |
| Client (GUI + CLI) | `x86_64-pc-windows-gnu` |
| Client (GUI + CLI) | macOS Universal (`aarch64` + `x86_64-apple-darwin` → lipo) |
| Client (GUI + CLI) | `x86_64-unknown-linux-musl` |
| Client (GUI + CLI) | `aarch64-unknown-linux-musl` |

### 必要なツール (初回のみ)

```bash
brew install mingw-w64 FiloSottile/musl-cross/musl-cross
rustup target add \
  x86_64-pc-windows-gnu \
  x86_64-unknown-linux-musl \
  aarch64-unknown-linux-musl \
  x86_64-apple-darwin
```

`~/.cargo/config.toml` にリンカ設定が必要 (設定済み):
- `aarch64-unknown-linux-musl` → `aarch64-linux-musl-gcc`
- `x86_64-unknown-linux-musl`  → `x86_64-linux-musl-gcc`
- `x86_64-pc-windows-gnu`      → `x86_64-w64-mingw32-gcc`

## dist/ 構成

```
dist/
├── server-linux-x64/      mc-share-server
├── server-linux-arm64/    mc-share-server
├── client-linux-x64/      mc-share  mc-share-gui
├── client-linux-arm64/    mc-share  mc-share-gui
├── client-windows-x64/    mc-share.exe  mc-share-gui.exe
├── client-macos/          mc-share  mc-share-gui  (Universal binary)
└── SPEC.md
```

## プロジェクト概要

Minecraft Java Edition 専用 P2P ワールド共有ツール。
- ポート開放不要 / アカウント不要
- UDP ホールパンチング + QUIC/TLS 1.3
- Coordination Server: `https://mcs.markund.f5.si`
- 詳細: `SPEC.md`
