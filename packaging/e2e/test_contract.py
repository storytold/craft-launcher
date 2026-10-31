"""Fast PR tests of fail-closed evidence validation, not substitutes for OS integration runs."""

import copy
import json
import os
import sys
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from contract import BINARY_ARCHES, TARGETS, artifact_names, digest, operations, self_update_artifact
from runner import Runner
from verify_release import verify


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.artifacts = self.root / "artifacts"
        self.reports = self.root / "reports"
        self.artifacts.mkdir()
        self.reports.mkdir()
        self.version, self.sha, self.run_id = "1.2.3", "a" * 40, "123"
        self.data = {}
        for target in TARGETS:
            names = artifact_names(self.version, target)
            for name in names:
                (self.artifacts / name).write_bytes(name.encode())
            self.data[target] = dict(schema=1, success=True, target=target, source_sha=self.sha,
                                     run_id=self.run_id, version=self.version,
                                     artifacts={name: digest(self.artifacts / name) for name in names},
                                     packages={name: "installed-executed-and-removed" for name in names},
                                     lifecycles={kind: operations(kind) for kind in TARGETS[target][2]},
                                     ui=[f"launcher-e2e-{kind}" for kind in TARGETS[target][2] if kind != "deb"],
                                     gatekeeper="assessments enabled")
            update_name = self_update_artifact(self.version, target)
            self.data[target]["self_update"] = dict(from_version="0.9.0", to_version=self.version,
                                                    candidate=update_name, sha256=digest(self.artifacts / update_name),
                                                    replaced=True, restarted=True, data_preserved=True)

    def write(self):
        for target, data in self.data.items():
            folder = self.reports / target
            folder.mkdir(exist_ok=True)
            (folder / f"qualification-{target}.json").write_text(json.dumps(data))
            # Synthetic evidence is ONLY for testing this validator; never installer proof.
            for name in artifact_names(self.version, target):
                identity = dict(artifact=name, sha256=digest(self.artifacts / name), native=dict(
                    schema=1, success=True, operation="init", launcher_version=self.version,
                    build_sha=self.sha, binary_arch=BINARY_ARCHES[target]))
                (folder / f"package-{name}.json").write_text(json.dumps(identity))
            for kind in TARGETS[target][2]:
                for index, operation in enumerate(operations(kind)):
                    native = dict(schema=1, success=True, operation=operation, launcher_version=self.version,
                                  build_sha=self.sha, binary_arch=BINARY_ARCHES[target])
                    (folder / f"{kind}-{index:02d}-{operation}.json").write_text(json.dumps(native))
                if kind != "deb":
                    for attempt in range(2):
                        (folder / f"gui-launcher-e2e-{kind}-{attempt}.json").write_text(json.dumps(dict(schema=1, ui_frames=3)))
            (folder / "self-update-native.json").write_text(json.dumps(dict(schema=1, scheduled=True, source_sha=self.sha,
                from_version="0.9.0", to_version=self.version, candidate_sha256=digest(self.artifacts / self_update_artifact(self.version, target)))))

    def check(self):
        self.write()
        return verify(self.artifacts, self.reports, self.version, self.sha, self.run_id)

    def test_complete_same_run_exact_bytes_pass(self):
        self.assertEqual(set(self.check()["qualified"]), set(TARGETS))

    def test_changed_artifact_fails_even_with_green_platform_reports(self):
        (self.artifacts / artifact_names(self.version, "windows-x64")[0]).write_bytes(b"replacement")
        with self.assertRaisesRegex(RuntimeError, "changed"):
            self.check()

    def test_missing_platform_fails(self):
        del self.data["macos-x64"]
        with self.assertRaisesRegex(RuntimeError, "missing"):
            self.check()

    def test_green_summary_cannot_hide_missing_or_stale_process_evidence(self):
        self.write()
        native_path = self.reports / "windows-x86/msi-00-init.json"
        native = json.loads(native_path.read_text())
        native["binary_arch"] = "x86_64"
        native_path.write_text(json.dumps(native))
        with self.assertRaisesRegex(RuntimeError, "invalid native result"):
            verify(self.artifacts, self.reports, self.version, self.sha, self.run_id)
        native_path.unlink()
        with self.assertRaisesRegex(RuntimeError, "missing native result"):
            verify(self.artifacts, self.reports, self.version, self.sha, self.run_id)

    def test_green_summary_requires_both_gui_starts_and_native_update(self):
        for relative, error in (("macos-arm64/gui-launcher-e2e-dmg-1.json", "missing GUI result"),
                                ("windows-arm64/self-update-native.json", "missing native self-update")):
            self.write()
            (self.reports / relative).unlink()
            with self.assertRaisesRegex(RuntimeError, error):
                verify(self.artifacts, self.reports, self.version, self.sha, self.run_id)

    def test_every_package_requires_matching_executed_source_architecture_and_artifact(self):
        target = "linux-aarch64"
        name = next(name for name in artifact_names(self.version, target) if name.endswith(".tar.gz"))
        path = self.reports / target / f"package-{name}.json"
        for field, value in (("build_sha", "stale-commit"), ("binary_arch", "x86_64"), ("launcher_version", "1.2.30")):
            self.write()
            identity = json.loads(path.read_text())
            identity["native"][field] = value
            path.write_text(json.dumps(identity))
            with self.assertRaisesRegex(RuntimeError, "invalid native result"):
                verify(self.artifacts, self.reports, self.version, self.sha, self.run_id)
        self.write()
        identity = json.loads(path.read_text())
        identity["sha256"] = "00" * 32
        path.write_text(json.dumps(identity))
        with self.assertRaisesRegex(RuntimeError, "another artifact"):
            verify(self.artifacts, self.reports, self.version, self.sha, self.run_id)
        path.unlink()
        with self.assertRaisesRegex(RuntimeError, "missing package identity"):
            verify(self.artifacts, self.reports, self.version, self.sha, self.run_id)

    def test_stale_source_run_or_version_fails(self):
        for field in ("source_sha", "run_id", "version"):
            with self.subTest(field=field):
                previous = self.data["linux-x86_64"][field]
                self.data["linux-x86_64"][field] = "other"
                with self.assertRaisesRegex(RuntimeError, "another commit"):
                    self.check()
                self.data["linux-x86_64"][field] = previous

    def test_skipped_cleanup_or_recovery_fails(self):
        original = copy.deepcopy(self.data)
        for operation in ("cancel-new", "bad-checksum", "pause-new", "remove", "reject-downgrade", "install-new"):
            with self.subTest(operation=operation):
                self.data = copy.deepcopy(original)
                self.data["windows-arm64"]["lifecycles"]["msi"].remove(operation)
                with self.assertRaisesRegex(RuntimeError, "skipped"):
                    self.check()

    def test_failed_report_missing_ui_or_failed_package_removal_fails(self):
        original = copy.deepcopy(self.data)
        for change in (dict(success=False), dict(ui=[]), dict(packages={}), dict(self_update={}), dict(gatekeeper="assessments disabled")):
            self.data = copy.deepcopy(original)
            self.data["macos-arm64"].update(change)
            with self.assertRaises(RuntimeError):
                self.check()

    def test_no_workstation_installs(self):
        with patch.dict(os.environ, {}, clear=True):
            with self.assertRaisesRegex(RuntimeError, "disposable"):
                Runner(None)

    def test_tarball_executable_lookup_ignores_same_named_documentation_directory(self):
        # The real Linux package contains bin/artcraft-launcher AND share/doc/artcraft-launcher.
        binary = self.root / "bin/artcraft-launcher"
        binary.parent.mkdir()
        binary.write_bytes(b"executable fixture")
        (self.root / "share/doc/artcraft-launcher").mkdir(parents=True)
        runner = Runner.__new__(Runner)
        self.assertEqual(runner.executable(self.root, "artcraft-launcher"), binary)
        duplicate = self.root / "other/artcraft-launcher"
        duplicate.parent.mkdir()
        duplicate.write_bytes(b"ambiguous second executable")
        with self.assertRaisesRegex(RuntimeError, "exactly one"):
            runner.executable(self.root, "artcraft-launcher")

    def test_nonzero_status_preserves_real_subprocess_output(self):
        # Exercise the runner's real wrapper, not a mocked successful command. spctl --status
        # can return exit 1 + stdout "assessments disabled" on an actual disabled-policy VM.
        runner = Runner.__new__(Runner)
        runner.output, runner.work, runner.env, runner.commands = self.root, self.root, dict(os.environ), 0
        argv = [sys.executable, "-c", "import sys; print('assessments disabled'); sys.exit(1)"]
        self.assertEqual(runner.command(argv, allowed=(0, 1)).strip(), "assessments disabled")
        with self.assertRaisesRegex(RuntimeError, "exited 1"):
            runner.command(argv)
        with self.assertRaisesRegex(RuntimeError, "exited 2"):
            runner.command([sys.executable, "-c", "raise SystemExit(2)"], allowed=(0, 1))

    def test_process_inventory_is_not_saved_or_exposed_on_failure(self):
        runner = Runner.__new__(Runner)
        runner.output, runner.work, runner.env, runner.commands = self.root, self.root, dict(os.environ), 0
        argv = [sys.executable, "-c", "print('sample-private-process-argument')"]
        self.assertIn("sample-private", runner.command(argv, record_output=False))
        with self.assertRaisesRegex(RuntimeError, "process inventory omitted") as failure:
            runner.command([sys.executable, "-c", "print('sample-private-process-argument'); raise SystemExit(2)"], record_output=False)
        self.assertNotIn("sample-private", str(failure.exception))
        self.assertTrue(all("sample-private" not in log.read_text() for log in self.root.glob("*.log")))


if __name__ == "__main__":
    unittest.main()
