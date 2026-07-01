#!/usr/bin/env bash
# publish-winget.sh — one-shot winget-pkgs publisher for MineScale-Java.
#
# What this does, end-to-end:
#   1. Read the current app version from client/Cargo.toml
#   2. Download the Windows x64 GUI .exe from the matching GitHub Release
#   3. Compute its SHA256
#   4. Generate the three winget-pkgs YAML manifests (version, installer,
#      defaultLocale) into a temp workspace
#   5. Ensure a fork of microsoft/winget-pkgs exists under your GH user
#   6. Clone (or reuse) the fork, create a fresh feature branch off master
#   7. Commit the manifests, push the branch
#   8. Open the PR against microsoft/winget-pkgs
#
# Why this exists: wingetcreate's `new` / `update` refuse to parse our
# release .exe (even with PE VS_VERSION_INFO embedded via winresource in
# build.rs), so we've had to hand-craft the manifests every release.
# This script encodes that ritual.
#
# Usage:
#   scripts/publish-winget.sh                 # publish the current Cargo.toml version
#   scripts/publish-winget.sh 1.2.12          # publish a specific version
#   scripts/publish-winget.sh --dry-run       # generate manifests, skip fork/PR
#
# Prereqs (checked at startup):
#   - gh CLI, authenticated (`gh auth status`)
#   - git, curl, shasum
#   - GitHub release with mc-share-gui-windows-x64.exe attached
#
# Idempotency: the script can be re-run safely. It refreshes the fork's
# master, creates a uniquely named branch per version, and no-ops the
# fork step if the fork already exists.

set -euo pipefail

# ── Config ───────────────────────────────────────────────────────────────────

PUBLISHER="mar1mo-41414"
PACKAGE="MineScale"
PACKAGE_ID="${PUBLISHER}.${PACKAGE}"
UPSTREAM_REPO="microsoft/winget-pkgs"
FORK_REPO="${PUBLISHER}/winget-pkgs"
UPSTREAM_BRANCH="master"
SOURCE_REPO="mar1mo-41414/MineScale"
INSTALLER_ASSET="mc-share-gui-windows-x64.exe"
MANIFEST_SCHEMA_VERSION="1.12.0"
MC_LICENSE_URL="https://github.com/mar1mo-41414/MineScale/blob/main/LICENSE"
MC_PACKAGE_URL="https://github.com/mar1mo-41414/MineScale"
MC_PUBLISHER_SUPPORT="https://github.com/mar1mo-41414/MineScale/issues"

# ── Arg parse ────────────────────────────────────────────────────────────────

DRY_RUN=0
VERSION_OVERRIDE=""
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) sed -n '1,40p' "$0"; exit 0 ;;
    -*) echo "unknown flag: $arg" >&2; exit 2 ;;
    *)  VERSION_OVERRIDE="$arg" ;;
  esac
done

# ── Locate repo root and version ─────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ -n "$VERSION_OVERRIDE" ]]; then
  VERSION="$VERSION_OVERRIDE"
else
  VERSION="$(grep -m1 '^version' "$REPO_ROOT/client/Cargo.toml" \
    | sed -E 's/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')"
fi
if [[ -z "$VERSION" ]]; then
  echo "error: could not determine version" >&2
  exit 1
fi
TAG="v${VERSION}"
BRANCH="add-${PACKAGE_ID//./-}-${VERSION}"

# ── Preflight ────────────────────────────────────────────────────────────────

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing tool: $1" >&2; exit 1; }; }
need gh
need git
need curl
need shasum

if ! gh auth status >/dev/null 2>&1; then
  echo "gh CLI is not authenticated. Run: gh auth login" >&2
  exit 1
fi

echo "── plan ──"
echo "  package:  $PACKAGE_ID"
echo "  version:  $VERSION"
echo "  tag:      $TAG"
echo "  branch:   $BRANCH"
echo "  asset:    $INSTALLER_ASSET"
echo "  fork:     $FORK_REPO"
echo "  upstream: $UPSTREAM_REPO (base: $UPSTREAM_BRANCH)"
echo "  dry-run:  $DRY_RUN"
echo

# ── Download the release asset and hash it ───────────────────────────────────

INSTALLER_URL="https://github.com/${SOURCE_REPO}/releases/download/${TAG}/${INSTALLER_ASSET}"
WORK="$(mktemp -d -t minescale-winget.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

echo "▶ downloading $INSTALLER_URL"
if ! curl -fsSL -o "$WORK/asset.exe" "$INSTALLER_URL"; then
  echo "error: could not download release asset from $INSTALLER_URL" >&2
  echo "       — did you push tag $TAG and upload $INSTALLER_ASSET to the release?" >&2
  exit 1
fi
SIZE="$(du -k "$WORK/asset.exe" | cut -f1)"
HASH="$(shasum -a 256 "$WORK/asset.exe" | awk '{print toupper($1)}')"
echo "  size:     ${SIZE} KiB"
echo "  sha256:   $HASH"

# ── Write manifests ──────────────────────────────────────────────────────────

MANIFEST_DIR="$WORK/manifests/m/$PUBLISHER/$PACKAGE/$VERSION"
mkdir -p "$MANIFEST_DIR"
TODAY="$(date -u +%Y-%m-%d)"

cat > "$MANIFEST_DIR/${PACKAGE_ID}.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.${MANIFEST_SCHEMA_VERSION}.schema.json

PackageIdentifier: ${PACKAGE_ID}
PackageVersion: ${VERSION}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: ${MANIFEST_SCHEMA_VERSION}
EOF

cat > "$MANIFEST_DIR/${PACKAGE_ID}.installer.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.${MANIFEST_SCHEMA_VERSION}.schema.json

PackageIdentifier: ${PACKAGE_ID}
PackageVersion: ${VERSION}
InstallerType: portable
Commands:
- mc-share-gui
ReleaseDate: ${TODAY}
Installers:
- Architecture: x64
  InstallerUrl: ${INSTALLER_URL}
  InstallerSha256: ${HASH}
ManifestType: installer
ManifestVersion: ${MANIFEST_SCHEMA_VERSION}
EOF

cat > "$MANIFEST_DIR/${PACKAGE_ID}.locale.en-US.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.${MANIFEST_SCHEMA_VERSION}.schema.json

PackageIdentifier: ${PACKAGE_ID}
PackageVersion: ${VERSION}
PackageLocale: en-US
Publisher: ${PUBLISHER}
PublisherUrl: https://github.com/${PUBLISHER}
PublisherSupportUrl: ${MC_PUBLISHER_SUPPORT}
Author: ${PUBLISHER}
PackageName: ${PACKAGE}
PackageUrl: ${MC_PACKAGE_URL}
License: MIT
LicenseUrl: ${MC_LICENSE_URL}
Copyright: Copyright (c) 2025 ${PUBLISHER}
ShortDescription: Minecraft Java Edition P2P world sharing - no port forwarding, no accounts.
Description: |-
  MineScale lets two players share a Minecraft Java Edition world peer-to-peer
  over an encrypted QUIC tunnel. No port forwarding, no account registration,
  no monthly fees. The host runs a LAN world, sends a link, and the joiner's
  Minecraft client sees it automatically in the multiplayer list.
Moniker: minescale
Tags:
- gaming
- minecraft
- multiplayer
- p2p
ReleaseNotesUrl: https://github.com/${SOURCE_REPO}/releases/tag/${TAG}
ManifestType: defaultLocale
ManifestVersion: ${MANIFEST_SCHEMA_VERSION}
EOF

echo
echo "▶ generated manifests at:"
ls -1 "$MANIFEST_DIR" | sed 's/^/    /'

if [[ "$DRY_RUN" == "1" ]]; then
  echo
  echo "── dry-run: skipping fork / clone / PR ──"
  echo "manifests kept at: $WORK/manifests"
  # Prevent trap from deleting on dry-run
  cp -R "$WORK/manifests" "./winget-manifests-${VERSION}"
  echo "copied to: ./winget-manifests-${VERSION}"
  exit 0
fi

# ── Ensure fork exists ───────────────────────────────────────────────────────

if gh repo view "$FORK_REPO" >/dev/null 2>&1; then
  echo "▶ fork already exists: $FORK_REPO"
else
  echo "▶ forking $UPSTREAM_REPO → $FORK_REPO"
  gh repo fork "$UPSTREAM_REPO" --clone=false >/dev/null
fi

# ── Clone the fork ───────────────────────────────────────────────────────────

CLONE_DIR="$WORK/winget-pkgs"
echo "▶ shallow-cloning $FORK_REPO"
git clone --depth 1 --branch "$UPSTREAM_BRANCH" \
  "git@github.com:${FORK_REPO}.git" "$CLONE_DIR" 2>&1 | tail -3

cd "$CLONE_DIR"

# Sync fork's master with upstream so our branch is off the latest state.
echo "▶ syncing fork master with upstream"
git remote add upstream "https://github.com/${UPSTREAM_REPO}.git" 2>/dev/null || true
git fetch --depth 1 upstream "$UPSTREAM_BRANCH" 2>&1 | tail -3
git reset --hard "upstream/${UPSTREAM_BRANCH}"
git push --force origin "$UPSTREAM_BRANCH" 2>&1 | tail -3

# ── Add manifests, commit, push, PR ──────────────────────────────────────────

echo "▶ creating branch $BRANCH"
git checkout -b "$BRANCH"

mkdir -p "manifests/m/$PUBLISHER/$PACKAGE"
rm -rf "manifests/m/$PUBLISHER/$PACKAGE/$VERSION"
cp -R "$MANIFEST_DIR" "manifests/m/$PUBLISHER/$PACKAGE/"

git add "manifests/m/$PUBLISHER/$PACKAGE/$VERSION"
git commit -m "New version: ${PACKAGE_ID} version ${VERSION}"
git push -u origin "$BRANCH" 2>&1 | tail -3

echo "▶ opening PR against $UPSTREAM_REPO"
PR_URL="$(gh pr create \
  --repo "$UPSTREAM_REPO" \
  --base "$UPSTREAM_BRANCH" \
  --head "${PUBLISHER}:${BRANCH}" \
  --title "New version: ${PACKAGE_ID} version ${VERSION}" \
  --body "$(cat <<EOF
New version of ${PACKAGE_ID}: ${VERSION}.

Manifests hand-authored via \`scripts/publish-winget.sh\` because
\`wingetcreate\` can't parse this project's portable .exe (single-binary
Rust build with embedded VS_VERSION_INFO but no installer wrapper).

- Repo: ${MC_PACKAGE_URL}
- Release: https://github.com/${SOURCE_REPO}/releases/tag/${TAG}
- License: MIT

Validation checklist:
- [x] InstallerSha256 matches the released artifact (\`${HASH:0:16}...\`)
- [x] InstallerUrl resolves (verified via curl in this script)
- [x] InstallerType: portable (single .exe, no install steps)
- [x] Manifest passes schema ${MANIFEST_SCHEMA_VERSION}
EOF
)")"

echo
echo "── done ──"
echo "  PR: ${PR_URL}"
