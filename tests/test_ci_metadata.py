"""Publishing boundary tests; run with python -m unittest discover -s tests -p test_ci_*.py."""

import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("image_metadata", ROOT / ".github/scripts/image_metadata.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ImageMetadataTests(unittest.TestCase):
    def result(self, ref, event="push"):
        return MODULE.metadata(ref, "a" * 40, event, "AmirHmZz/speculum", (ROOT / "Dockerfile").read_text())

    def test_main_has_no_stable_aliases(self):
        result = self.result("refs/heads/main")
        self.assertEqual(result["publish"], "true")
        self.assertEqual(result["tags"].splitlines(), [
            "ghcr.io/amirhmzz/speculum:main",
            "ghcr.io/amirhmzz/speculum:sha-" + "a" * 40,
        ])

    def test_release_aliases(self):
        result = self.result("refs/tags/v0.2.1")
        self.assertEqual(result["version"], "0.2.1")
        self.assertEqual(result["tags"].splitlines(), [
            "ghcr.io/amirhmzz/speculum:0.2.1",
            "ghcr.io/amirhmzz/speculum:0.2",
            "ghcr.io/amirhmzz/speculum:latest",
            "ghcr.io/amirhmzz/speculum:sha-" + "a" * 40,
        ])

    def test_non_publishing_events(self):
        for ref, event in [
            ("refs/pull/7/merge", "pull_request"),
            ("refs/heads/main", "pull_request"),
            ("refs/tags/v0.2.1", "workflow_dispatch"),
            ("refs/heads/feature", "push"),
        ]:
            with self.subTest(ref=ref, event=event):
                result = self.result(ref, event)
                self.assertEqual(result["publish"], "false")
                self.assertEqual(result["tags"], "")

    def test_invalid_release_tags_fail(self):
        for tag in ["v1.0", "v1.2.3-rc.1", "v1.2.3+build", "v01.2.3", "v1.02.3", "v1.2.03", "v1.2.3/extra"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                self.result("refs/tags/" + tag)

    def test_toolchains_come_from_dockerfile(self):
        result = MODULE.metadata("refs/heads/main", "b" * 40, "push", "OWNER/Repo",
                                 "FROM oven/bun:1.4.2-alpine AS dashboard\nFROM rust:1.98-bookworm AS rust\n")
        self.assertEqual(result["rust"], "1.98")
        self.assertEqual(result["bun"], "1.4.2")
        self.assertEqual(result["image"], "ghcr.io/owner/repo")


if __name__ == "__main__":
    unittest.main()
