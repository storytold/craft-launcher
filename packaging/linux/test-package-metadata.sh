#!/usr/bin/env bash
# Build fixture deb/rpm packages through the real Linux packager, without compiling the app.
# Needs Linux (GNU install), nfpm, dpkg-deb and rpm. Never installs a package or runs Rust.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
for tool in nfpm dpkg-deb rpm; do
  command -v "$tool" >/dev/null || { echo "error: $tool is required" >&2; exit 1; }
done
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

check_packages() {
  local arch="$1" target="$2" floor="$3" expected="$4" deb_arch="$5"
  local work="$TMP/$arch-${floor:-default}" deb actual_arch deb_requires rpm_requires
  mkdir -p "$work/target/$target/release"
  printf '#!/bin/sh\necho "ArtCraft Launcher packaging fixture"\n' >"$work/target/$target/release/artcraft-launcher"
  chmod +x "$work/target/$target/release/artcraft-launcher"
  # A set cross target skips the version smoke execution; no target binary is run.
  (
    unset EMULATOR
    if [[ "$floor" = unset ]]; then unset GLIBC_FLOOR; else export GLIBC_FLOOR="$floor"; fi
    export CROSS_ARCH="$arch" CROSS_TARGET="$target" CROSS_COMPILE=fixture-
    export CARGO_TARGET_DIR="$work/target" DIST="$work/dist"
    export ARTCRAFT_LAUNCHER_VERSION=0.0.0 ARTCRAFT_LAUNCHER_BUILD_DATE=2026-01-01
    bash "$ROOT/packaging/linux/package.sh" --skip-build --formats "deb rpm"
  )
  deb="$work/dist/artcraft-launcher-0.0.0-linux-$arch.deb"
  actual_arch="$(dpkg-deb -f "$deb" Architecture)"
  [[ "$actual_arch" = "$deb_arch" ]] || {
    echo "error: $arch GLIBC_FLOOR=${floor:-empty}: expected deb architecture $deb_arch, got $actual_arch" >&2
    return 1
  }
  deb_requires="$(dpkg-deb -f "$deb" Depends)"
  tr ',' '\n' <<<"$deb_requires" | sed 's/^ *//' | grep -Fxq "libc6 (>= $expected)" || {
    echo "error: $arch GLIBC_FLOOR=${floor:-empty}: expected libc6 >= $expected; deb Depends: $deb_requires" >&2
    return 1
  }
  rpm_requires="$(rpm -qp --requires "$work/dist/artcraft-launcher-0.0.0-linux-$arch.rpm")"
  grep -Fxq "glibc >= $expected" <<<"$rpm_requires" || {
    echo "error: $arch GLIBC_FLOOR=${floor:-empty}: expected glibc >= $expected; rpm Requires: $rpm_requires" >&2
    return 1
  }
  echo "ok: $arch GLIBC_FLOOR=${floor:-empty} -> deb/rpm >= $expected"
}

check_packages x86_64 x86_64-unknown-linux-gnu unset 2.35 amd64
check_packages aarch64 aarch64-unknown-linux-gnu unset 2.35 arm64
check_packages riscv64 riscv64gc-unknown-linux-gnu 2.39 2.39 riscv64
check_packages x86_64 x86_64-unknown-linux-gnu "" 2.35 amd64
check_packages x86_64 x86_64-unknown-linux-gnu 2.40 2.40 amd64

echo "All 5 glibc metadata cases passed (deb and rpm)."
