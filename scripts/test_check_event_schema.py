import unittest
from check_event_schema import parse_events, diff

SRC = """
use soroban_sdk::{Address, Env, Symbol};
// comment with .publish(ignored)
pub fn paused(env: &Env, admin: &Address) {
    env.events()
        .publish((Symbol::new(env, "paused"), admin.clone()), ());
}
pub fn batch(env: &Env, a: u32, b: u32) {
    env.events().publish((Symbol::new(env, "batch"),), (a, b));
}
pub fn helper(env: &Env) -> u32 { 1 }
"""


class EventSchemaTests(unittest.TestCase):
    def test_parses_emitters_and_ignores_non_emitters(self):
        ev = parse_events(SRC)
        self.assertEqual(sorted(ev), ["batch", "paused"])
        self.assertEqual(ev["paused"]["params"], ["admin:&Address"])
        self.assertEqual(ev["paused"]["publish"], ['(Symbol::new(env,"paused"),admin.clone()),()'])
        self.assertEqual(ev["batch"]["params"], ["a:u32", "b:u32"])

    def test_formatting_changes_are_not_schema_changes(self):
        reformatted = SRC.replace("env.events()\n        .publish", "env.events().publish")
        self.assertEqual(parse_events(SRC), parse_events(reformatted))

    def test_diff_flags_topic_data_change_addition_and_removal(self):
        old = {"c": parse_events(SRC)}
        changed = SRC.replace("(a, b)", "(b, a)").replace("pub fn helper", "pub fn extra(env: &Env) { env.events().publish((1,), ()); }\npub fn helper")
        problems = diff(old, {"c": parse_events(changed)})
        self.assertEqual(len(problems), 2)
        self.assertTrue(any("c::batch: schema changed" in p for p in problems))
        self.assertTrue(any("c::extra: new event" in p for p in problems))
        removed = diff(old, {"c": {}})
        self.assertEqual(len(removed), 2)
        self.assertEqual(diff(old, old), [])


if __name__ == "__main__":
    unittest.main()
