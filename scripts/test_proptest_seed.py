import json
import tempfile
import unittest
from pathlib import Path

from proptest_seed import (
    MAX_SEED,
    build_report,
    extract_failures,
    new_seed,
    parse_seed,
    write_report,
)

SAMPLE = """running 1 test
---- tests::prop_sum stdout ----
proptest: Saving this and future failures in proptest-regressions/test.txt
minimal failing input: a = 0, b = 18446744073709551615
test result: FAILED
"""


class ProptestSeedTests(unittest.TestCase):
    def test_extracts_minimal_failing_case(self):
        self.assertEqual(
            extract_failures(SAMPLE),
            [{"test": "tests::prop_sum",
              "minimal_failing_input": "a = 0, b = 18446744073709551615"}],
        )

    def test_passing_output_has_no_failures(self):
        self.assertEqual(extract_failures("test result: ok. 3 passed"), [])

    def test_invalid_seed_rejected(self):
        with self.assertRaises(ValueError):
            parse_seed("abc")
        with self.assertRaises(ValueError):
            parse_seed("-1")
        with self.assertRaises(ValueError):
            parse_seed(str(MAX_SEED))

    def test_seed_boundaries_accepted_and_generated_in_range(self):
        self.assertEqual(parse_seed("0"), 0)
        self.assertEqual(parse_seed(str(MAX_SEED - 1)), MAX_SEED - 1)
        self.assertTrue(0 <= new_seed() < MAX_SEED)

    def test_report_roundtrip_is_idempotent(self):
        report = build_report(42, 101, SAMPLE)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "nested" / "report.json"
            write_report(report, path)
            write_report(report, path)
            self.assertEqual(json.loads(path.read_text()), report)
        self.assertIn("--seed 42", report["replay"])


if __name__ == "__main__":
    unittest.main()
