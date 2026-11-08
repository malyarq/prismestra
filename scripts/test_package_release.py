import importlib.util
import json
from pathlib import Path
import tempfile
import sys
import unittest
from unittest.mock import patch
import zipfile

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("package_release", Path(__file__).with_name("package_release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class PackageChecks(unittest.TestCase):
    def test_manifest_does_not_publish_local_paths_or_package_ids(self):
        package = dict(name="example", version="1.0", license="MIT", repository=None,
                       source=None, manifest_path="C:/Users/private/secret/Cargo.toml",
                       id="path+file:///C:/private/example", license_file=None)
        encoded = json.dumps(release.inventory({"packages": [package]}))
        self.assertNotIn("private", encoded)
        self.assertNotIn("manifest_path", encoded)

    def test_missing_license_is_an_error(self):
        with self.assertRaisesRegex(ValueError, "Missing license"):
            release.inventory({"packages": [dict(name="unknown", license=None)]})

    def test_unreviewed_or_apache_only_license_cannot_enter_gpl2_distribution(self):
        for license_name in ("Apache-2.0", "Unknown-1.0"):
            with self.assertRaisesRegex(ValueError, "Review license"):
                release.inventory({"packages": [dict(name="example", license=license_name)]})

    def test_dual_license_selection_preserves_additional_terms(self):
        entries = release.inventory({"packages": [
            dict(name="example", version="1", license="(MIT OR Apache-2.0) AND IJG")
        ]})
        self.assertEqual(entries[0]["distribution_license"], "MIT AND IJG")

    def test_dirty_or_changed_checkout_is_rejected(self):
        expected = "a" * 40
        for outputs in (["b" * 40], [expected, "?? private.json"]):
            with patch.object(release, "run", side_effect=outputs):
                with self.assertRaises(ValueError):
                    release.clean_commit(Path("."), expected)

    def test_zip_contains_only_the_selected_directory_and_cannot_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected = root / "Prismestra"
            selected.mkdir()
            (selected / "LICENSE").write_text("license", encoding="utf-8")
            (root / "private.json").write_text("secret", encoding="utf-8")
            archive = root / "release.zip"
            release.write_zip(selected, archive)
            with zipfile.ZipFile(archive) as output:
                self.assertEqual(output.namelist(), ["Prismestra/LICENSE"])
                self.assertEqual(output.read("Prismestra/LICENSE"), b"license")
            with self.assertRaises(FileExistsError):
                release.write_zip(selected, archive)


if __name__ == "__main__":
    unittest.main()
