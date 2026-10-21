import unittest
from generate_contract_api import parse_contract_signatures, replace_section, BEGIN, END

class ContractApiGeneratorTests(unittest.TestCase):
    def test_multiline_signature_and_nested_generic_types(self):
        source = """
#[contractimpl]
impl Example {
    pub fn nested(
        env: Env,
        records: Map<String, Vec<(Address, Option<BytesN<32>>)>>,
    ) -> Option<Vec<(Address, u128)>> {
        unimplemented!()
    }
}
"""
        self.assertEqual(
            parse_contract_signatures(source),
            ["nested(records: Map<String, Vec<(Address, Option<BytesN<32>>)>>) -> Option<Vec<(Address, u128)>>"],
        )

    def test_multiple_contractimpl_blocks_are_collected(self):
        source = """
#[contractimpl]
impl First { pub fn one(env: Env, value: u32) -> bool { value > 0 } }
#[contractimpl]
impl Second { pub fn two(env: Env) {} }
"""
        self.assertEqual(parse_contract_signatures(source), ["one(value: u32) -> bool", "two() -> ()"])

    def test_stale_generated_table_is_detected_by_comparison(self):
        readme = "Intro\n" + BEGIN + "\nold table\n" + END + "\nOutro\n"
        expected = "Intro\nnew table\nOutro\n"
        self.assertNotEqual(readme, replace_section(readme, "new table"))
        self.assertEqual(replace_section(readme, "new table"), expected)

if __name__ == "__main__":
    unittest.main()
