#!/usr/bin/env python3
"""Install and exercise immutable release candidates on disposable hosted runners.

No rebuilds, local-mirror features, patched signatures, mocked installers or security-policy
changes. All OS mutations are confined to a fresh VM; test homes and destinations are isolated.
Only stdlib Python plus the OS's real package utilities are needed.
"""

import argparse
import contextlib
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import tempfile
import time
import traceback
import urllib.request
import zipfile

from contract import BINARY_ARCHES, TARGETS, artifact_names, digest, operations, require


class Runner:
    def __init__(self, args):
        hosted = os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
        disposable_vm = (os.environ.get("RUNNER_ENVIRONMENT") == "self-hosted" and
                         os.environ.get("ARTCRAFT_QUALIFICATION_DISPOSABLE_VM") == "1")
        require(os.environ.get("GITHUB_ACTIONS") == "true" and (hosted or disposable_vm),
                "Refusing system installs outside a disposable Actions runner")
        self.args = args
        self.output = args.output.resolve()
        self.output.mkdir(parents=True, exist_ok=False)
        self.work = Path(tempfile.mkdtemp(prefix="launcher-e2e-work-", dir=os.environ["RUNNER_TEMP"]))
        self.env = dict(os.environ)
        # Windows Known Folders/registry belong to the disposable runner account. Redirection
        # alone is not sufficient there; never advertise this harness as workstation-safe.
        home = self.work / "home"
        for path in (home / ".Trash", home / ".config", home / ".cache", home / ".local/share"):
            path.mkdir(parents=True, exist_ok=True)
        self.env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
                        XDG_CACHE_HOME=str(home / ".cache"), XDG_DATA_HOME=str(home / ".local/share"),
                        APPIMAGE_EXTRACT_AND_RUN="1", RUST_BACKTRACE="1")
        # Prevent accidental fixture redirection even if a developer set the variable upstream.
        self.env.pop("ARTCRAFT_LAUNCHER_DOWNLOAD_BASE", None)
        self.commands = 0
        self.report = dict(schema=1, target=args.target, source_sha=os.environ["GITHUB_SHA"],
                           run_id=os.environ["GITHUB_RUN_ID"], version=args.version,
                           runner_arch=platform.machine(), success=False, artifacts={}, lifecycles={}, packages={})

    def command(self, argv, *, env=None, timeout=600, allowed=(0,), record_output=True):
        argv = [str(a) for a in argv]
        self.commands += 1
        label = f"{self.commands:03d}-{Path(argv[0]).name}"
        print(f"[{label}] {' '.join(argv)}", flush=True)
        # Avoid printing the environment: tokens must never enter artifacts.
        try:
            result = subprocess.run(argv, env=env or self.env, cwd=self.work,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            (self.output / f"{label}.log").write_bytes((exc.stdout or b"") if record_output else b"process inventory omitted\n")
            raise RuntimeError(f"{label} timed out after {timeout}s") from exc
        (self.output / f"{label}.log").write_bytes(result.stdout if record_output else b"process inventory omitted\n")
        detail = result.stdout[-4000:].decode(errors="replace") if record_output else "process inventory omitted"
        require(result.returncode in allowed, f"{label} exited {result.returncode}: {detail}")
        return result.stdout.decode(errors="replace")

    def executable(self, directory, name="artcraft-launcher.exe"):
        matches = [path for path in directory.rglob(name) if path.is_file()]
        require(len(matches) == 1, f"expected exactly one {name} under {directory}: {matches}")
        return matches[0]

    @contextlib.contextmanager
    def installed_launcher(self, artifact):
        folder = self.work / f"package-{self.commands}"
        folder.mkdir()
        if artifact.suffix == ".dmg":
            mount = folder / "mount"
            mount.mkdir()
            attached = False
            try:
                self.command(["hdiutil", "attach", "-readonly", "-nobrowse", "-noautoopen", "-mountpoint", mount, artifact])
                attached = True
                bundle = folder / "ArtCraft Launcher.app"
                self.command(["ditto", mount / bundle.name, bundle])
            finally:
                if attached:
                    self.command(["hdiutil", "detach", mount])
            self.command(["codesign", "--verify", "--deep", "--strict", bundle])
            self.command(["xcrun", "stapler", "validate", bundle])
            if self.args.gatekeeper == "enabled":
                self.command(["/usr/sbin/spctl", "--assess", "--type", "execute", bundle])
            try:
                yield bundle / "Contents/MacOS/ArtCraft Launcher"
            finally:
                shutil.rmtree(bundle)
                require(not bundle.exists(), "launcher bundle remains after removal")
        elif artifact.suffix == ".msi":
            exe = folder / "artcraft-launcher.exe"
            installed = False
            try:
                self.command(["msiexec.exe", "/i", artifact, "/qn", "/norestart",
                              f"INSTALLFOLDER={folder}", "INSTALLDESKTOPSHORTCUT=1",
                              "/L*v", self.output / "launcher-install.log"], allowed=(0, 3010))
                installed = True
                require(exe.is_file(), "MSI did not install launcher executable")
                self.check_windows_registration(folder, installed=True)
                yield exe
            finally:
                if installed:
                    self.command(["msiexec.exe", "/x", artifact, "/qn", "/norestart",
                                  "/L*v", self.output / "launcher-uninstall.log"], allowed=(0, 3010))
                    require(not exe.exists(), "MSI uninstall left launcher executable")
                    self.check_windows_registration(folder, installed=False)
        elif artifact.suffix == ".zip":
            with zipfile.ZipFile(artifact) as archive:
                for name in archive.namelist():
                    require((folder / name).resolve().is_relative_to(folder), "unsafe candidate ZIP path")
                archive.extractall(folder)
            exe = self.executable(folder)
            require((exe.parent / "portable.txt").is_file(), "portable marker missing")
            try:
                yield exe
            finally:
                shutil.rmtree(folder)
                require(not exe.exists(), "portable launcher remains after removal")
        elif artifact.suffix == ".deb":
            installed = False
            try:
                self.command(["sudo", "apt-get", "install", "-y", artifact])
                installed = True
                require(self.command(["dpkg-query", "-W", "-f=${Status}", "artcraft-launcher"]).strip() == "install ok installed",
                        "Debian package is not registered")
                yield Path("/usr/bin/artcraft-launcher")
            finally:
                if installed:
                    self.command(["sudo", "apt-get", "remove", "-y", "artcraft-launcher"])
                    require(not Path("/usr/bin/artcraft-launcher").exists(), "Debian uninstall left launcher executable")
        elif artifact.suffix == ".AppImage":
            exe = folder / artifact.name
            shutil.copy2(artifact, exe)
            exe.chmod(0o755)
            try:
                yield exe
            finally:
                exe.unlink()
                require(not exe.exists(), "AppImage launcher remains after removal")
        elif artifact.name.endswith(".tar.gz"):
            # tar validates the release's real Unix permissions/symlinks; the candidate is from
            # the same trusted build, and extraction is confined to a disposable runner folder.
            self.command(["tar", "-xzf", artifact, "-C", folder])
            exe = self.executable(folder, "artcraft-launcher")
            try:
                yield exe
            finally:
                shutil.rmtree(folder)
        else:
            raise RuntimeError(f"unsupported candidate format: {artifact}")

    def check_windows_registration(self, folder, *, installed):
        import winreg
        key_path = r"Software\Learning Machines LLC\ArtCraft Launcher"
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key_path) as key:
                location = winreg.QueryValueEx(key, "InstallDir")[0]
        except FileNotFoundError:
            location = None
        require((location is not None) == installed, "MSI registry ownership did not follow install/remove")
        if installed:
            require(Path(location).resolve() == folder.resolve(), "MSI ignored chosen install folder")
        for relative in ("Microsoft/Windows/Start Menu/Programs/ArtCraft Launcher.lnk",):
            shortcut = Path(os.environ["APPDATA"]) / relative
            require(shortcut.exists() == installed, f"shortcut state is wrong: {shortcut}")

    def operation(self, exe, root, kind, operation, *, elevated=False):
        result_file = root / "result.json"
        # Reports must be fresh, not success left over from a previous invocation.
        if result_file.exists():
            result_file.unlink()
        prefix = ["sudo", "-E"] if elevated else []
        try:
            self.command([*prefix, exe, "--qualify-install", root, kind, operation])
        finally:
            if elevated and root.exists():
                # Atomic native reports are mode 0600. Keep this test-owned directory readable
                # by the runner after root executes the real system-package operation. -P never
                # traverses symlinks into installed/system paths; those stay package-manager owned.
                self.command(["sudo", "chown", "-RP", "--", f"{os.getuid()}:{os.getgid()}", root])
        result = json.loads(result_file.read_text())
        require(result["success"] and result["operation"] == operation and result["launcher_version"] == self.args.version,
                f"wrong qualification result: {result}")
        require(result["build_sha"] == os.environ["GITHUB_SHA"], "installed launcher was built from another source revision")
        require(result["binary_arch"] == BINARY_ARCHES[self.args.target], "wrong architecture executed (possibly emulated)")
        return result

    @staticmethod
    def app_executable(installed):
        path = Path(installed["location"])
        if path.suffix == ".app":
            info = plistlib.loads((path / "Contents/Info.plist").read_bytes())
            path = path / "Contents/MacOS" / info["CFBundleExecutable"]
        return path

    def probe_app(self, result):
        path = self.app_executable(result["installed"])
        output = self.command([path, "--version"], timeout=60)
        # The executable, not just a manifest or registry row, must report the version.
        version = "0.13.1" if result["operation"] == "verify-new" else "0.12.1"
        require(version in output, f"installed app did not report {version}: {output}")

    def stop_app(self, installed):
        """Stop only processes whose executable/command is the exact test-owned installation.

        This is test teardown, not a claim that the launcher has a stop-app feature. Descendants
        of an AppImage runtime are included so its extracted process cannot outlive the test.
        """
        path = self.app_executable(installed)
        if platform.system() == "Windows":
            env = dict(self.env, QUALIFIED_APP_EXE=str(path))
            self.command(["powershell", "-NoProfile", "-NonInteractive", "-Command",
                          "$ErrorActionPreference = 'Stop'; Get-CimInstance Win32_Process | "
                          "Where-Object { $_.ExecutablePath -eq $env:QUALIFIED_APP_EXE } | "
                          "ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction Stop }"], env=env)
        else:
            # Inspect in memory only: unrelated process arguments can contain runner secrets.
            output = self.command(["ps", "-axo", "pid=,ppid=,args="], record_output=False)
            rows = []
            for line in output.splitlines():
                fields = line.strip().split(None, 2)
                if len(fields) == 3:
                    rows.append((int(fields[0]), int(fields[1]), fields[2]))
            owned = {pid for pid, _, command in rows if command == str(path) or command.startswith(str(path) + " ")}
            # Bound descendant traversal to the finite process snapshot.
            for _ in range(len(rows)):
                added = {pid for pid, parent, _ in rows if parent in owned} - owned
                if not added:
                    break
                owned.update(added)
            for pid in sorted(owned, reverse=True):
                try:
                    if installed["source"] == "Deb":
                        self.command(["sudo", "kill", "-TERM", str(pid)], allowed=(0, 1))
                    else:
                        os.kill(pid, 15)
                except ProcessLookupError:
                    pass
        deadline = time.monotonic() + 10
        while self.is_running(path) and time.monotonic() < deadline:
            time.sleep(0.2)
        require(not self.is_running(path), f"test-owned process did not stop: {path}")

    def is_running(self, path):
        if platform.system() == "Windows":
            env = dict(self.env, QUALIFIED_APP_EXE=str(path))
            output = self.command(["powershell", "-NoProfile", "-NonInteractive", "-Command",
                                   "Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq $env:QUALIFIED_APP_EXE } | "
                                   "ForEach-Object { $_.ProcessId }"], env=env)
            return bool(output.strip())
        rows = self.command(["ps", "-axo", "args="], record_output=False).splitlines()
        return any(row == str(path) or row.startswith(str(path) + " ") for row in rows)

    def self_update(self, candidate_exe, artifact, kind):
        """Execute the candidate's updater/helper against a genuine older installation.

        Candidate bytes enter the normal checksum-verified cache, since their release URL is
        deliberately not public yet. This qualifies replacement/restart, not update discovery.
        """
        root = self.work / "launcher-e2e-self-update"
        self.operation(candidate_exe, root, kind, "init")
        old_name = artifact.name.replace(f"-{self.args.version}-", "-0.9.0-", 1)
        sums = Path(__file__).with_name("fixtures").joinpath("launcher-0.9.0.sha256").read_text()
        pins = {line.split()[1].lstrip("*"): line.split()[0] for line in sums.splitlines() if line.strip()}
        require(old_name in pins, f"previous launcher fixture missing: {old_name}")
        old = self.work / old_name
        url = f"https://github.com/storytold/craft-launcher/releases/download/v0.9.0/{old_name}"
        request = urllib.request.Request(url, headers={"User-Agent": "ArtCraft-Launcher/Qualification"})
        with urllib.request.urlopen(request, timeout=60) as response, old.open("wb") as output:
            require(response.headers.get("Content-Length") is None or int(response.headers["Content-Length"]) < 1024 ** 3,
                    "oversized previous-version fixture")
            received = 0
            deadline = time.monotonic() + 300
            while chunk := response.read(1024 * 1024):
                received += len(chunk)
                require(received < 1024 ** 3 and time.monotonic() < deadline, "fixture download limit exceeded")
                output.write(chunk)
        require(digest(old) == pins[old_name], "previous launcher fixture checksum mismatch")
        with self.installed_launcher(old) as target_exe:
            require("0.9.0" in self.command([target_exe, "--version"], timeout=60), "not the pinned older launcher")
            before = digest(target_exe)
            # Windows portable data is next to the application and must survive replacement.
            personal = target_exe.parent / "ArtCraftLauncherData/qualification-personal.txt"
            if kind == "portable":
                personal.parent.mkdir(parents=True, exist_ok=True)
                personal.write_text("keep launcher preferences\n")
            target = target_exe.parents[2] if kind == "dmg" else target_exe
            try:
                env = dict(self.env, ARTCRAFT_LAUNCHER_PRETEND_VERSION="0.9.0")
                self.command([candidate_exe, "--qualify-self-update", root, kind, artifact, target], env=env)
                result = json.loads((root / "self-update.json").read_text())
                (self.output / "self-update-native.json").write_text(json.dumps(result, indent=2))
                require(result["scheduled"] and result["source_sha"] == os.environ["GITHUB_SHA"] and
                        result["candidate_sha256"] == digest(artifact), "wrong self-update scheduled")
                deadline = time.monotonic() + 90
                while time.monotonic() < deadline:
                    failure = Path(result["failure_log"])
                    require(not failure.exists(), f"self-update helper failed: {failure.read_text() if failure.exists() else ''}")
                    if target_exe.is_file() and digest(target_exe) != before and self.is_running(target_exe):
                        break
                    time.sleep(1)
                else:
                    raise RuntimeError("self-update did not replace the old launcher and restart it")
                require(self.args.version in self.command([target_exe, "--version"], timeout=60), "restart has wrong launcher version")
                require(Path(result["preferences"]).read_text() == "keep launcher preferences\n", "self-update lost launcher preferences")
                if kind == "portable":
                    require(personal.read_text() == "keep launcher preferences\n", "self-update lost portable preferences")
                # Invoke the installed replacement: source SHA/architecture must match the candidate.
                self.operation(target_exe, root, kind, "verify-absent")
                self.report["self_update"] = dict(from_version="0.9.0", to_version=self.args.version,
                                                candidate=artifact.name, sha256=digest(artifact),
                                                replaced=True, restarted=True, data_preserved=True)
            finally:
                self.stop_app(dict(location=str(target), source="Launcher"))

    def lifecycle(self, exe, kind):
        root = self.work / f"launcher-e2e-{kind}"
        completed = []
        self.report["lifecycles"][kind] = completed
        # apt invokes the real pkexec path as root on this disposable VM; no polkit policy edits.
        elevated = kind == "deb"
        launched = None
        cleanup_app = None
        try:
            for index, operation in enumerate(operations(kind)):
                result = self.operation(exe, root, kind, operation, elevated=elevated)
                cleanup_app = result["installed"]
                (self.output / f"{kind}-{index:02d}-{operation}.json").write_text(json.dumps(result, indent=2))
                if operation in ("verify-old", "verify-new"):
                    self.probe_app(result)
                if operation == "install-old" and kind == "portable":
                    # Personal files inside a portable installation are specifically preserved.
                    personal = Path(result["installed"]["location"]).parent / "personal-project.txt"
                    personal.write_text("keep my project\n")
                if operation == "launch":
                    launched = result["installed"]
                    require(self.is_running(self.app_executable(launched)), "no process from the exact installed app path")
                    if kind == "deb":
                        self.stop_app(launched)
                        launched = None
                if operation == "reject-running":
                    self.stop_app(launched)
                    launched = None
                if kind == "portable" and operation in ("verify-new", "remove"):
                    require(personal.read_text() == "keep my project\n", "portable update/removal lost a user file")
                completed.append(operation)
                if operation == "init" and not elevated:
                    self.gui(exe, root)
        finally:
            # Cleanup uses the launcher's own uninstall path, with the same ownership checks.
            # Never swallow cleanup failures or report a green run with a leftover install.
            if cleanup_app:
                self.stop_app(cleanup_app)
            if (root / "owner.txt").exists():
                self.operation(exe, root, kind, "remove", elevated=elevated)

    def gui(self, exe, root):
        report = root / "ui.json"
        for attempt in range(2):
            report.unlink(missing_ok=True)
            self.command([exe, "--qualify-ui", root], timeout=90)
            result = json.loads(report.read_text())
            require(result["ui_frames"] >= 3, "launcher never rendered its UI")
            (self.output / f"gui-{root.name}-{attempt}.json").write_text(json.dumps(result, indent=2))
        self.report.setdefault("ui", []).append(root.name)

    def run(self):
        target = self.args.target
        require(target.split("-")[0] == {"Darwin": "macos", "Windows": "windows", "Linux": "linux"}[platform.system()],
                "candidate target does not match runner OS")
        names = artifact_names(self.args.version, target)
        for name in names:
            artifact = self.args.artifacts.resolve() / name
            require(artifact.is_file(), f"missing candidate {name}")
            self.report["artifacts"][name] = digest(artifact)
        if platform.system() == "Darwin":
            # macOS reports disabled policy on stdout with exit 1. Preserve the actual output;
            # an exit code alone cannot distinguish disabled policy from an unknown failure.
            status = self.command(["/usr/sbin/spctl", "--status"], allowed=(0, 1)).strip()
            require(status == f"assessments {self.args.gatekeeper}", f"unexpected Gatekeeper status: {status}")
            self.report["gatekeeper"] = status
        for name in names:
            artifact = self.args.artifacts.resolve() / name
            with self.installed_launcher(artifact) as exe:
                require(self.args.version in self.command([exe, "--version"], timeout=60), "wrong launcher version")
                kind = {".dmg": "dmg", ".msi": "msi", ".zip": "portable", ".deb": "deb", ".AppImage": "appimage"}.get(artifact.suffix)
                # Every package must prove its executed binary's exact source/version/arch,
                # including tar.gz, which does not run an app lifecycle of its own.
                identity_root = self.work / f"launcher-e2e-package-{self.commands}"
                identity = self.operation(exe, identity_root, kind or "appimage", "init")
                (self.output / f"package-{name}.json").write_text(json.dumps(dict(
                    artifact=name, sha256=self.report["artifacts"][name], native=identity), indent=2))
                if kind:
                    self.lifecycle(exe, kind)
                if kind in ("dmg", "portable", "appimage"):
                    self.self_update(exe, artifact, kind)
                self.report["packages"][name] = "installed-and-executed"
            self.report["packages"][name] = "installed-executed-and-removed"
            require(digest(artifact) == self.report["artifacts"][name], "test mutated candidate artifact")
        require(set(self.report["lifecycles"]) == set(TARGETS[target][2]), "lifecycle coverage is incomplete")
        self.report["success"] = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--gatekeeper", choices=("enabled", "disabled"), default="enabled",
                        help="assert existing policy, never change it; disabled requires a separately provisioned disposable VM")
    runner = Runner(parser.parse_args())
    try:
        runner.run()
        # Keep isolated files on failure for diagnostics until the VM is discarded. Cleanup
        # failure also fails the report; only remove this harness's freshly allocated directory.
        if platform.system() == "Linux":
            runner.command(["sudo", "rm", "-rf", runner.work])
        else:
            shutil.rmtree(runner.work)
    except Exception:
        runner.report["success"] = False
        runner.report["error"] = traceback.format_exc()
        raise
    finally:
        (runner.output / f"qualification-{runner.args.target}.json").write_text(json.dumps(runner.report, indent=2) + "\n")


if __name__ == "__main__":
    main()
