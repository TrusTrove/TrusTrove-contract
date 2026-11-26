"""Regression tests for the read-only repository malware guard."""

import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path


SPEC = importlib.util.spec_from_file_location("scan_repo", Path(__file__).with_name("scan_repo.py"))
assert SPEC and SPEC.loader
scanner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scanner)


class MalwareGuardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)

    def add(self, name: str, content: bytes):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        subprocess.run(["git", "-C", str(self.root), "-c", "core.autocrlf=false", "add", "-f", name], check=True)

    def test_clean_rust_file(self):
        self.add("contracts/pool/src/lib.rs", b"pub fn balance() -> u32 { 1 }\n")
        self.assertEqual(scanner.scan(self.root), [])

    def test_tracked_vscode_task_is_rejected_even_when_ignored(self):
        self.add(".gitignore", b".vscode/\n")
        task = b'{"runOptions":{"runOn":"folder' + b'Open"},"command":"node font.lkf"}'
        self.add(".vscode/tasks.json", task)
        findings = scanner.scan(self.root)
        self.assertTrue(any("VS Code workspace" in hit for hit in findings))
        self.assertTrue(any("folder-open" in hit for hit in findings))

    def test_disguised_font_and_hidden_loader(self):
        self.add("public/fonts/fa-solid.woff2", b" " * 300 + b"require('child_process')")
        self.add("postcss.config.js", b"module.exports={};" + b"\t" * 315 + b"eval('x')\n")
        findings = scanner.scan(self.root)
        self.assertTrue(any("invalid file signature" in hit for hit in findings))
        self.assertTrue(any("hidden after 100+" in hit for hit in findings))

    def test_build_hook_and_loader_tooling(self):
        self.add("package.json", b'{"scripts":{"build":"node api.' + b'js && cargo build"}}')
        self.add(".gitignore", b"temp_auto_" + b"push.bat\n")
        findings = scanner.scan(self.root)
        self.assertTrue(any("node api.js build hook" in hit for hit in findings))
        self.assertTrue(any("published PolinRider tooling" in hit for hit in findings))

    def test_never_executes_candidate_code(self):
        marker = self.root / "executed.txt"
        self.add("payload.py", f"open({str(marker)!r}, 'w').write('ran')\n".encode())
        self.assertEqual(scanner.scan(self.root), [])
        self.assertFalse(marker.exists())


if __name__ == "__main__":
    unittest.main()
