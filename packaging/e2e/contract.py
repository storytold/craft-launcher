"""Shared, dependency-free qualification contract for runners and the release gate."""

import hashlib
from pathlib import Path

TARGETS = {
    "macos-arm64": ("macos-universal", ["dmg"], ["dmg"]),
    "macos-x64": ("macos-universal", ["dmg"], ["dmg"]),
    "windows-x64": ("windows-x64", ["msi", "portable.zip"], ["msi", "portable"]),
    "windows-x86": ("windows-x86", ["msi", "portable.zip"], ["msi", "portable"]),
    "windows-arm64": ("windows-arm64", ["msi", "portable.zip"], ["msi", "portable"]),
    "linux-x86_64": ("linux-x86_64", ["deb", "AppImage", "tar.gz"], ["deb", "appimage"]),
    "linux-aarch64": ("linux-aarch64", ["deb", "AppImage", "tar.gz"], ["deb", "appimage"]),
}

BINARY_ARCHES = {
    "macos-arm64": "aarch64", "macos-x64": "x86_64", "windows-x64": "x86_64", "windows-x86": "x86",
    "windows-arm64": "aarch64", "linux-x86_64": "x86_64", "linux-aarch64": "aarch64",
}

OPERATIONS = [
    "init", "verify-absent", "install-old", "verify-old", "cancel-new", "verify-old",
    "bad-checksum", "verify-old", "pause-new", "install-new", "verify-new",
    "reject-downgrade", "verify-new", "corrupt-cache", "install-new", "verify-new", "launch", "reject-running",
    "remove", "verify-absent", "remove", "install-new", "verify-new", "remove", "verify-absent",
]


def operations(kind):
    # The production launcher deliberately delegates running-process policy to dpkg for a
    # system Debian install. Portable/macOS/MSI installs have their own running-app guard.
    return [step for step in OPERATIONS if not (kind == "deb" and step == "reject-running")]


def artifact_names(version, target):
    token, formats, _ = TARGETS[target]
    stem = f"artcraft-launcher-{version}-{token}"
    return [f"{stem}-portable.zip" if fmt == "portable.zip" else f"{stem}.{fmt}" for fmt in formats]


def self_update_artifact(version, target):
    suffix = ".dmg" if target.startswith("macos-") else ("-portable.zip" if target.startswith("windows-") else ".AppImage")
    return next(name for name in artifact_names(version, target) if name.endswith(suffix))


def digest(path):
    result = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)
