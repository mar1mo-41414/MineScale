# Build Guide

開発者向けのビルド手順です。配布バイナリを使うだけの人は [README.md](README.md) を読んでください。

---

## 前提

- Rust 1.76+
- 全プラットフォームを一括ビルドする場合: macOS 上で以下が必要

```bash
brew install mingw-w64 FiloSottile/musl-cross/musl-cross zig
cargo install cargo-zigbuild
rustup target add \
  x86_64-pc-windows-gnu \
  x86_64-unknown-linux-musl  aarch64-unknown-linux-musl \
  x86_64-unknown-linux-gnu   aarch64-unknown-linux-gnu \
  x86_64-apple-darwin
```

`~/.cargo/config.toml` のリンカ設定:
- `aarch64-unknown-linux-musl` → `aarch64-linux-musl-gcc`
- `x86_64-unknown-linux-musl`  → `x86_64-linux-musl-gcc`
- `x86_64-pc-windows-gnu`      → `x86_64-w64-mingw32-gcc`

---

## 単一プラットフォームビルド

```bash
# クライアント（CLI）
cargo build --release -p mc-share

# クライアント（GUI）
cargo build --release -p mc-share-gui

# サーバー
cargo build --release -p mc-share-server
```

---

## 全プラットフォーム一括ビルド

```bash
./scripts/build-all.sh
```

成果物は `dist/` 以下にまとまります:

```
dist/
├── server-linux-x64/      mc-share-server
├── server-linux-arm64/    mc-share-server
├── client-linux-x64/      mc-share  mc-share-gui
├── client-linux-arm64/    mc-share  mc-share-gui
├── client-windows-x64/    mc-share.exe  mc-share-gui.exe
├── client-macos/          mc-share  mc-share-gui   (Universal)
└── SPEC.md
```

### ターゲット一覧

| 成果物 | ターゲット | 備考 |
|--------|-----------|------|
| Server | `x86_64-unknown-linux-musl` | 静的 |
| Server | `aarch64-unknown-linux-musl` | 静的 |
| Client CLI | 上記 + Windows MinGW + macOS Universal | 静的 |
| Client GUI | `x86_64-unknown-linux-gnu.2.17` (zigbuild) | glibc 2.17+ |
| Client GUI | `aarch64-unknown-linux-gnu.2.17` (zigbuild) | glibc 2.17+ |
| Client GUI | Windows MinGW / macOS Universal | — |

> Linux GUI は musl ではなく glibc ターゲット (cargo-zigbuild) でビルドする。
> musl 版は X11/Wayland の dlopen に失敗して起動できないため。

---

## サーバーのデプロイ

```bash
export BASE_URL="https://mcs.example.com"
export RELAY_ADDR="203.0.113.1:9090"
export LISTEN_HTTP="0.0.0.0:8080"
export LISTEN_RELAY="0.0.0.0:9090"

./mc-share-server
```

nginx / Pangolin / Traefik などのリバースプロキシ経由で 443 に出します。

### nginx 例

```nginx
server {
    listen 443 ssl;
    server_name mcs.example.com;
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header X-Forwarded-For $remote_addr;
    }
}
```

---

## CLI の使い方

```bash
# ホスト（LAN 公開中のワールドを自動検出）
mc-share host

# ポート直接指定
mc-share host --port 25565

# 参加
mc-share join https://mcs.example.com/8fk2lm
mc-share join 8fk2lm                            # ショート URL
```
