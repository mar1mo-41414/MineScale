#!/usr/bin/env bash
# build-all.sh — MineScale-Java 全プラットフォームビルド
#
# 対象:
#   Server : Linux x64, Linux arm64
#   Client : Windows x64, macOS Universal (x64+arm64), Linux x64, Linux arm64
#
# 前提 (初回のみ):
#   brew install mingw-w64 FiloSottile/musl-cross/musl-cross
#   rustup target add x86_64-pc-windows-gnu \
#                     x86_64-unknown-linux-musl \
#                     aarch64-unknown-linux-musl \
#                     x86_64-apple-darwin

set -euo pipefail

cd "$(dirname "$0")/.."

BOLD='\033[1m'
GREEN='\033[0;32m'
RESET='\033[0m'

step() { echo -e "\n${BOLD}▶ $*${RESET}"; }
ok()   { echo -e "${GREEN}  ✓ $*${RESET}"; }

# ── Build ──────────────────────────────────────────────────────────────────────

step "Server — Linux x64 (musl)"
cargo build --release --target x86_64-unknown-linux-musl  -p mc-share-server
ok "server linux-x64"

step "Server — Linux arm64 (musl)"
cargo build --release --target aarch64-unknown-linux-musl -p mc-share-server
ok "server linux-arm64"

step "Client — Linux x64 (musl)"
cargo build --release --target x86_64-unknown-linux-musl  -p mc-share -p mc-share-gui
ok "client linux-x64"

step "Client — Linux arm64 (musl)"
cargo build --release --target aarch64-unknown-linux-musl -p mc-share -p mc-share-gui
ok "client linux-arm64"

step "Client — Windows x64 (MinGW)"
cargo build --release --target x86_64-pc-windows-gnu      -p mc-share -p mc-share-gui
ok "client windows-x64"

step "Client — macOS arm64 (native)"
cargo build --release --target aarch64-apple-darwin        -p mc-share -p mc-share-gui
ok "client macos-arm64"

step "Client — macOS x64"
cargo build --release --target x86_64-apple-darwin         -p mc-share -p mc-share-gui
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

# ── Assemble dist/ ─────────────────────────────────────────────────────────────

step "Assembling dist/"
rm -rf dist && mkdir -p \
  dist/server-linux-x64 \
  dist/server-linux-arm64 \
  dist/client-linux-x64 \
  dist/client-linux-arm64 \
  dist/client-windows-x64 \
  dist/client-macos

# Servers (CLI only)
cp target/x86_64-unknown-linux-musl/release/mc-share-server  dist/server-linux-x64/mc-share-server
cp target/aarch64-unknown-linux-musl/release/mc-share-server dist/server-linux-arm64/mc-share-server

# Linux clients
cp target/x86_64-unknown-linux-musl/release/mc-share         dist/client-linux-x64/mc-share
cp target/x86_64-unknown-linux-musl/release/mc-share-gui     dist/client-linux-x64/mc-share-gui
cp target/aarch64-unknown-linux-musl/release/mc-share        dist/client-linux-arm64/mc-share
cp target/aarch64-unknown-linux-musl/release/mc-share-gui    dist/client-linux-arm64/mc-share-gui

# Windows clients
cp target/x86_64-pc-windows-gnu/release/mc-share.exe         dist/client-windows-x64/mc-share.exe
cp target/x86_64-pc-windows-gnu/release/mc-share-gui.exe     dist/client-windows-x64/mc-share-gui.exe

# macOS universal clients
cp target/mc-share-macos-universal                            dist/client-macos/mc-share
cp target/mc-share-gui-macos-universal                        dist/client-macos/mc-share-gui
chmod +x dist/client-macos/mc-share dist/client-macos/mc-share-gui

# Spec
cp SPEC.md dist/

# ── Summary ────────────────────────────────────────────────────────────────────

step "dist/ contents"
find dist -type f | sort | while read f; do
  printf "  %-48s %s\n" "$f" "$(du -sh "$f" | cut -f1)"
done

echo -e "\n${BOLD}All platforms built successfully.${RESET}"
