"""Exercise the shipped launcher with Windows PowerShell 5 and a harmless argv recorder."""

import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest


PORTABLE = Path(__file__).with_name("portable")
POWERSHELL = shutil.which("powershell.exe")
CSC = Path(os.environ.get("WINDIR", "C:/Windows")) / "Microsoft.NET/Framework64/v4.0.30319/csc.exe"


@unittest.skipUnless(os.name == "nt" and POWERSHELL and CSC.is_file(), "Windows PowerShell 5 and .NET compiler required")
class PortableLauncherChecks(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix="pl-")
        cls.base = Path(cls.temporary.name)
        source = cls.base / "Recorder.cs"
        source.write_text('''using System;
using System.IO;
using System.Text;
using System.Threading;
using System.Security.AccessControl;
using System.Security.Principal;
class Recorder {
    static int Main(string[] args) {
        if (args.Length > 1 && args[0] == "--test-deny-write") {
            var acl = Directory.GetAccessControl(args[1]);
            Console.Write(acl.GetSecurityDescriptorSddlForm(AccessControlSections.Access));
            acl.AddAccessRule(new FileSystemAccessRule(WindowsIdentity.GetCurrent().User,
                FileSystemRights.WriteData, AccessControlType.Deny));
            Directory.SetAccessControl(args[1], acl);
            return 0;
        }
        if (args.Length > 2 && args[0] == "--test-restore-acl") {
            var acl = Directory.GetAccessControl(args[1]);
            acl.SetSecurityDescriptorSddlForm(args[2], AccessControlSections.Access);
            Directory.SetAccessControl(args[1], acl);
            return 0;
        }
        string[] lines = new string[args.Length + 1];
        lines[0] = Convert.ToBase64String(Encoding.UTF8.GetBytes(Environment.CurrentDirectory));
        for (int i = 0; i < args.Length; ++i)
            lines[i + 1] = Convert.ToBase64String(Encoding.UTF8.GetBytes(args[i]));
        File.WriteAllLines(Environment.GetEnvironmentVariable("PRISMESTRA_TEST_RECORD"), lines);
        File.WriteAllText(Environment.GetEnvironmentVariable("PRISMESTRA_TEST_RECORD") + ".pid", System.Diagnostics.Process.GetCurrentProcess().Id.ToString());
        var timer = System.Diagnostics.Stopwatch.StartNew();
        int delay = Int32.Parse(Environment.GetEnvironmentVariable("PRISMESTRA_TEST_SLEEP") ?? "0");
        while (timer.ElapsedMilliseconds < delay && !File.Exists(Environment.GetEnvironmentVariable("PRISMESTRA_TEST_RECORD") + ".stop"))
            Thread.Sleep(20);
        File.WriteAllText(Environment.GetEnvironmentVariable("PRISMESTRA_TEST_RECORD") + ".exited", "done");
        return Int32.Parse(Environment.GetEnvironmentVariable("PRISMESTRA_TEST_EXIT") ?? "0");
    }
}''', encoding="utf-8")
        cls.fake = cls.base / "Recorder.exe"
        subprocess.run([str(CSC), "/nologo", "/target:exe", f"/out:{cls.fake}", str(source)], check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    def setUp(self):
        self.package = self.base / ("Ю [1] & O'Brien ! % ^ (x) " + str(len(list(self.base.iterdir()))))
        self.package.mkdir()
        for source in PORTABLE.iterdir():
            shutil.copy2(source, self.package / source.name)
        shutil.copy2(self.fake, self.package / "Prismestra.exe")
        self.record = self.base / (self._testMethodName + ".args")
        self.env = dict(os.environ, PRISMESTRA_TEST_RECORD=str(self.record))

    def run_launcher(self, *args, wrapper=None):
        if wrapper:
            command = f'"{os.environ["COMSPEC"]}" /d /s /c ""{self.package / wrapper}" {" ".join(args)}"'
        else:
            command = [POWERSHELL, "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(self.package / "Launch Prismestra.ps1"), *args]
        return subprocess.run(command, cwd=self.base, env=self.env, input="\n", capture_output=True, encoding="utf-8", errors="replace", timeout=30)

    def saved_profile(self, bindings=True):
        (self.package / "data/state").mkdir(parents=True)
        (self.package / "data/profile.json").write_text("{}", encoding="utf-8")
        if bindings:
            (self.package / "data/state/lighting-local.json").write_text("{}", encoding="utf-8")

    def recorded(self):
        return [base64.b64decode(line).decode("utf-8") for line in self.record.read_text().splitlines()]

    def expected(self, *extra):
        return [str(self.package), "--state-dir", str(self.package / "data/state"), *extra]

    def assert_failure_without_process(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(message, result.stdout + result.stderr)
        self.assertFalse(self.record.exists())

    def test_preview_from_other_cwd_and_special_paths(self):
        result = self.run_launcher()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.recorded(), self.expected())
        self.assertFalse(list((self.package / "data/state").glob(".launcher-write-*")))

    def test_preview_uses_profile_without_hardware(self):
        self.saved_profile()
        result = self.run_launcher("-ClassicEditor")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.recorded(), self.expected("--project", str(self.package / "data/profile.json"), "--classic-editor"))

    def test_connected_arguments(self):
        self.saved_profile()
        result = self.run_launcher("-Connected")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.recorded(), self.expected("--project", str(self.package / "data/profile.json"), "--start-connected"))

    def test_check_does_not_create_state_or_launch(self):
        before = set(self.package.rglob("*"))
        result = self.run_launcher("-Check")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        plan = json.loads(result.stdout)
        self.assertTrue(plan["ok"])
        self.assertEqual(plan["arguments"], self.expected()[1:])
        self.assertEqual(before, set(self.package.rglob("*")))
        self.assertFalse(self.record.exists())

    def test_missing_executable(self):
        (self.package / "Prismestra.exe").unlink()
        result = self.run_launcher("-Check")
        self.assert_failure_without_process(result, "Missing Prismestra.exe")
        self.assertFalse(json.loads(result.stdout)["ok"])

    def test_connected_requires_profile_and_bindings(self):
        self.assert_failure_without_process(self.run_launcher("-Connected"), "Missing saved profile")
        self.saved_profile(bindings=False)
        self.assert_failure_without_process(self.run_launcher("-Connected"), "Missing saved local connections")

    def test_state_path_file_cannot_activate(self):
        self.saved_profile()
        shutil.rmtree(self.package / "data/state")
        (self.package / "data/state").write_text("blocked", encoding="utf-8")
        self.assert_failure_without_process(self.run_launcher(), "State path is a file")

    def test_child_failure_is_propagated(self):
        self.env["PRISMESTRA_TEST_EXIT"] = "23"
        result = self.run_launcher()
        self.assertEqual(result.returncode, 23, result.stdout + result.stderr)

    def test_unwritable_state_fails_before_connected_process(self):
        self.saved_profile()
        state = str(self.package / "data/state")
        deny = subprocess.run([str(self.fake), "--test-deny-write", state], capture_output=True, text=True, check=True)
        try:
            probe = Path(state) / "acl-enforcement-probe"
            try:
                probe.write_bytes(b"test")
            except PermissionError:
                pass
            else:
                probe.unlink()
                self.skipTest("Sandbox does not enforce this current-user WriteData deny; real unwritable-directory acceptance remains unverified here")
            self.assert_failure_without_process(self.run_launcher("-Connected"), "Cannot write portable state")
        finally:
            subprocess.run([str(self.fake), "--test-restore-acl", state, deny.stdout], capture_output=True, check=True)

    def test_launcher_returns_while_application_continues(self):
        self.env["PRISMESTRA_TEST_SLEEP"] = "10000"
        # Redirect child inherited handles too: capture_output would wait for their EOF.
        command = [POWERSHELL, "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(self.package / "Launch Prismestra.ps1")]
        started = time.monotonic()
        try:
            result = subprocess.run(command, cwd=self.base, env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=6)
            self.assertEqual(result.returncode, 0)
            self.assertLess(time.monotonic() - started, 6)
            self.assertEqual(self.recorded(), self.expected())
        finally:
            # Cooperatively stop only this test's fake; taskkill may be sandbox-restricted.
            self.record.with_suffix(self.record.suffix + ".stop").touch()
            exited = self.record.with_suffix(self.record.suffix + ".exited")
            deadline = time.monotonic() + 5
            while not exited.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(exited.exists(), "Fake application did not stop")
            time.sleep(0.1)

    def test_cmd_wrappers_use_package_paths_and_explicit_connected(self):
        self.saved_profile()
        for wrapper, extra in (("Open Prismestra.cmd", []), ("Start saved scene.cmd", ["--start-connected"])):
            result = self.run_launcher(wrapper=wrapper)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            # cmd's %~dp0 can restore filesystem casing (TEMP may have a different case).
            actual = self.recorded()
            expected = self.expected("--project", str(self.package / "data/profile.json"), *extra)
            self.assertEqual([os.path.normcase(arg) if ":\\" in arg else arg for arg in actual],
                             [os.path.normcase(arg) if ":\\" in arg else arg for arg in expected])
        result = self.run_launcher("-Check", wrapper="Open Prismestra.cmd")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(json.loads(result.stdout)["ok"])


if __name__ == "__main__":
    unittest.main()
