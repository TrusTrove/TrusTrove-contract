import unittest

from check_production_panic_calls import scan_source


class ProductionPanicCheckerTests(unittest.TestCase):
    def test_unwrap_is_rejected(self):
        found = scan_source("fn f() { value.unwrap(); }", "contracts/demo/src/lib.rs")
        self.assertEqual([(item.kind, item.line) for item in found], [("unwrap", 1)])

    def test_expect_is_rejected(self):
        found = scan_source('fn f() { value.expect("must exist"); }', "contracts/demo/src/lib.rs")
        self.assertEqual([item.kind for item in found], ["expect"])

    def test_bare_panic_macro_is_rejected(self):
        found = scan_source('fn f() { panic!("bad"); }', "contracts/demo/src/lib.rs")
        self.assertEqual([item.kind for item in found], ["panic"])

    def test_comments_and_string_literals_are_ignored(self):
        source = r'''
// value.unwrap(); panic!("comment");
/* nested comment: /* value.expect("x") */ still a comment */
const MESSAGE: &str = "value.unwrap(); panic!(\\"not code\\")";
fn f() { let _ = value.unwrap_or_default(); }
'''
        self.assertEqual(scan_source(source, "contracts/demo/src/lib.rs"), [])

    def test_typed_panic_and_fallbacks_are_allowed(self):
        source = """
fn f(env: &Env) {
    panic_with_error!(env, Error::Bad);
    let _ = value.unwrap_or(0);
    let _ = value.unwrap_or_else(make_default);
}
"""
        self.assertEqual(scan_source(source, "contracts/demo/src/lib.rs"), [])

    def test_inline_cfg_test_item_is_ignored(self):
        source = """
#[cfg(test)]
mod tests {
    fn fixture() { value.unwrap(); panic!("test"); }
}
fn production() { value.expect("required"); }
"""
        found = scan_source(source, "contracts/demo/src/lib.rs")
        self.assertEqual([(item.kind, item.line) for item in found], [("expect", 6)])

    def test_multiline_calls_are_detected(self):
        source = """
fn f() {
    value
        .expect(
            "required"
        );
    another.unwrap
        (
        );
}
"""
        found = scan_source(source, "contracts/demo/src/lib.rs")
        self.assertEqual([item.kind for item in found], ["expect", "unwrap"])


if __name__ == "__main__":
    unittest.main()
