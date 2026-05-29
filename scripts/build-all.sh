#!/usr/bin/env bash
# build-all.sh — MineScale-Java 全プラットフォームビルド
#
# ターゲット
#   Server : Linux x64 (musl), Linux arm64 (musl)
#   Client CLI  : Linux x64 (musl), Linux arm64 (musl)
#                 Windows x64 (MinGW), macOS Universal
#   Client GUI  : Linux x64 (glibc 2.17+), Linux arm64 (glibc 2.17+)  ← zigbuild
#                 Windows x64 (MinGW), macOS Universal
#
# ※ Linux GUI は glibc ターゲット (x86_64-unknown-linux-gnu.2.17) でビルドする。
#    musl ビルドは dlopen 経由で glibc 製の libX11/libwayland を読み込めないため GUI が起動しない。
#
# 前提 (初回のみ):
#   brew install mingw-w64 FiloSottile/musl-cross/musl-cross zig
#   cargo install cargo-zigbuild
#   rustup target add x86_64-pc-windows-gnu \
#                     x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
#                     x86_64-unknown-linux-gnu  aarch64-unknown-linux-gnu \
#                     x86_64-apple-darwin

set -euo pipefail
cd "$(dirname "$0")/.."

BOLD='\033[1m'; GREEN='\033[0;32m'; RESET='\033[0m'
step() { echo -e "\n${BOLD}▶ $*${RESET}"; }
ok()   { echo -e "${GREEN}  ✓ $*${RESET}"; }

# ── サーバー ──────────────────────────────────────────────────────────────────

step "Server — Linux x64  (musl)"
cargo build --release --target x86_64-unknown-linux-musl  -p mc-share-server
ok "server linux-x64"

step "Server — Linux arm64 (musl)"
cargo build --release --target aarch64-unknown-linux-musl -p mc-share-server
ok "server linux-arm64"

# ── クライアント CLI (musl — 依存なし静的バイナリ) ─────────────────────────────

step "Client CLI — Linux x64  (musl)"
cargo build --release --target x86_64-unknown-linux-musl  -p mc-share
ok "cli linux-x64"

step "Client CLI — Linux arm64 (musl)"
cargo build --release --target aarch64-unknown-linux-musl -p mc-share
ok "cli linux-arm64"

# ── クライアント GUI Linux (glibc — X11/Wayland dlopen 互換) ─────────────────
# musl ビルドでは glibc 製 libX11/libwayland の dlopen が失敗するため
# GNU ターゲット + zigbuild で glibc 2.17 以上を要求するバイナリを生成する。

step "Client GUI — Linux x64  (glibc 2.17, zigbuild)"
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17  -p mc-share-gui
ok "gui linux-x64"

step "Client GUI — Linux arm64 (glibc 2.17, zigbuild)"
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.17 -p mc-share-gui
ok "gui linux-arm64"

# ── クライアント Windows x64 ──────────────────────────────────────────────────

step "Client — Windows x64 (MinGW)"
cargo build --release --target x86_64-pc-windows-gnu -p mc-share -p mc-share-gui
ok "client windows-x64"

# ── クライアント macOS ────────────────────────────────────────────────────────

step "Client — macOS arm64 (native)"
cargo build --release --target aarch64-apple-darwin  -p mc-share -p mc-share-gui
ok "client macos-arm64"

step "Client — macOS x64"
cargo build --release --target x86_64-apple-darwin   -p mc-share -p mc-share-gui
ok "client macos-x64"

step "Client — macOS Universal binary (lipo)"
lipo -create \
  target/aarch64-apple-darwin/release/mc-share-gui \
  target/x86_64-apple-darwin/release/mc-share-gui \
  -output target/mc-share-gui-macos-universal
lipo -create \
  target/aarch64-apple-darwin/release/mc-share \
  target/x86_64-apple-darwin/release/mc-share \
  -output target/mc-share-macos-universal
ok "macos universal"

# ── dist/ に集約 ──────────────────────────────────────────────────────────────

step "Assembling dist/"
rm -rf dist && mkdir -p \
  dist/server-linux-x64 \
  dist/server-linux-arm64 \
  dist/client-linux-x64 \
  dist/client-linux-arm64 \
  dist/client-windows-x64 \
  dist/client-macos

# Servers
cp target/x86_64-unknown-linux-musl/release/mc-share-server  dist/server-linux-x64/
cp target/aarch64-unknown-linux-musl/release/mc-share-server dist/server-linux-arm64/

# Linux CLI (musl) + GUI (glibc)
cp target/x86_64-unknown-linux-musl/release/mc-share         dist/client-linux-x64/mc-share
cp target/x86_64-unknown-linux-gnu/release/mc-share-gui      dist/client-linux-x64/mc-share-gui
cp target/aarch64-unknown-linux-musl/release/mc-share        dist/client-linux-arm64/mc-share
cp target/aarch64-unknown-linux-gnu/release/mc-share-gui     dist/client-linux-arm64/mc-share-gui

# Windows
cp target/x86_64-pc-windows-gnu/release/mc-share.exe         dist/client-windows-x64/
cp target/x86_64-pc-windows-gnu/release/mc-share-gui.exe     dist/client-windows-x64/

# macOS Universal
cp target/mc-share-macos-universal                            dist/client-macos/mc-share
cp target/mc-share-gui-macos-universal                        dist/client-macos/mc-share-gui
chmod +x dist/client-macos/mc-share dist/client-macos/mc-share-gui

cp SPEC.md dist/

# ── サマリ ────────────────────────────────────────────────────────────────────

step "dist/ contents"
find dist -type f | sort | while read f; do
  printf "  %-52s %s\n" "$f" "$(du -sh "$f" | cut -f1)"
done

echo -e "\n${BOLD}All platforms built successfully.${RESET}"
