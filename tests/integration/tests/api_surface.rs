//! Public Soroban API surface snapshot (issue #1016).
//!
//! `#[contractimpl]` exports every `pub fn` of its impl block as a contract
//! entry point, but CI previously had no review signal when a helper became
//! public or a required entry point disappeared. This test derives the
//! deterministic exported-method list for the four core contracts directly
//! from their sources and compares it with the checked-in manifest at
//! `docs/api-surface.txt`.
//!
//! A change is only accepted when the manifest is updated deliberately — and
//! the manifest carries a `review_note:` line that must cite the issue/PR
//! justifying the change. The manifest parser also requires each contract
//! block to be sorted and duplicate-free so unrelated edits cannot reorder or
//! shadow an entry point.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// The four contracts whose exported surface is pinned. Names match the
/// `contract <name>` blocks in the manifest and `env!("CARGO_PKG_NAME")`-style
/// crate directory names.
const CONTRACTS: [(&str, &str); 4] = [
    ("escrow", "contracts/escrow/src/lib.rs"),
    ("invoice", "contracts/invoice/src/lib.rs"),
    ("pool", "contracts/pool/src/lib.rs"),
    ("registry", "contracts/registry/src/lib.rs"),
];

const MANIFEST_REL: &str = "docs/api-surface.txt";

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/tests/integration`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root should resolve")
}

/// Returns the sorted, deduplicated names of every `pub fn` declared inside a
/// `#[contractimpl]` block.
fn exported_methods(source: &str) -> Vec<String> {
    const MARKER: &str = "#[contractimpl]";
    let mut methods = Vec::new();
    let mut search_from = 0;

    while let Some(relative) = find_in_code(source, MARKER, search_from) {
        let after_marker = relative + MARKER.len();
        let Some(open) = find_in_code(source, "{", after_marker) else {
            panic!("`{MARKER}` is not followed by an impl block");
        };
        let close = matching_brace(source, open)
            .unwrap_or_else(|| panic!("unbalanced braces after `{MARKER}`"));
        let body = &source[open + 1..close];

        // Scan for `pub fn` ignoring comments and string literals, so a doc
        // example or an explanatory comment cannot register a method.
        let mut cursor = 0;
        while let Some(position) = find_in_code(body, "pub fn ", cursor) {
            let name_start = position + "pub fn ".len();
            let name: String = body[name_start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            assert!(
                !name.is_empty(),
                "could not parse the method name after `pub fn` in `{}`",
                body[position..].lines().next().unwrap_or("").trim()
            );
            methods.push(name);
            cursor = name_start;
        }

        search_from = close + 1;
    }

    methods.sort();
    methods.dedup();
    methods
}

/// Finds `needle` in `source` at or after `from`, ignoring occurrences inside
/// line comments, block comments, and string literals. Marker text frequently
/// appears in documentation (for example the pool crate's `#[contractimpl]`
/// comment), so a plain `str::find` would treat prose as code.
fn find_in_code(source: &str, needle: &str, from: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let needle = needle.as_bytes();
    let mut i = from;
    let (mut line_comment, mut block_comment, mut string, mut escaped) =
        (false, false, false, false);

    while i < bytes.len() {
        let byte = bytes[i];
        let next = bytes.get(i + 1).copied();

        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                string = false;
            }
            i += 1;
            continue;
        }

        match byte {
            b'/' if next == Some(b'/') => {
                line_comment = true;
                i += 2;
            }
            b'/' if next == Some(b'*') => {
                block_comment = true;
                i += 2;
            }
            b'"' => {
                string = true;
                i += 1;
            }
            _ if bytes[i..].starts_with(needle) => return Some(i),
            _ => i += 1,
        }
    }

    None
}

/// Finds the closing brace matching the `{` at `open`, ignoring braces inside
/// line comments, block comments, and string literals.
fn matching_brace(source: &str, open: usize) -> Option<usize> {
    assert_eq!(source.as_bytes()[open], b'{');
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    let (mut line_comment, mut block_comment, mut string, mut escaped) =
        (false, false, false, false);

    while i < bytes.len() {
        let byte = bytes[i];
        let next = bytes.get(i + 1).copied();

        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                string = false;
            }
            i += 1;
            continue;
        }

        match byte {
            b'/' if next == Some(b'/') => {
                line_comment = true;
                i += 2;
            }
            b'/' if next == Some(b'*') => {
                block_comment = true;
                i += 2;
            }
            b'"' => {
                string = true;
                i += 1;
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }

    None
}

struct Manifest {
    review_note: String,
    contracts: BTreeMap<String, Vec<String>>,
}

fn parse_manifest(source: &str) -> Manifest {
    let mut review_note = None;
    let mut contracts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<(String, Vec<String>)> = None;

    for (index, raw) in source.lines().enumerate() {
        let line = raw.trim();
        let line_number = index + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(note) = line.strip_prefix("review_note:") {
            assert!(
                review_note.is_none(),
                "{MANIFEST_REL}:{line_number}: duplicate `review_note:`"
            );
            review_note = Some(note.trim().to_string());
            continue;
        }

        if let Some(name) = line.strip_prefix("contract ") {
            let name = name.trim();
            assert!(
                current.is_none(),
                "{MANIFEST_REL}:{line_number}: `contract {name}` starts before the previous block's `end`"
            );
            assert!(
                CONTRACTS.iter().any(|(known, _)| *known == name),
                "{MANIFEST_REL}:{line_number}: unknown contract `{name}`"
            );
            assert!(
                !contracts.contains_key(name),
                "{MANIFEST_REL}:{line_number}: duplicate contract block `{name}`"
            );
            current = Some((name.to_string(), Vec::new()));
            continue;
        }

        if line == "end" {
            let (name, methods) = current.take().unwrap_or_else(|| {
                panic!("{MANIFEST_REL}:{line_number}: `end` without an open contract block")
            });
            let mut canonical = methods.clone();
            canonical.sort();
            canonical.dedup();
            assert_eq!(
                methods, canonical,
                "{MANIFEST_REL}:{line_number}: methods for `{name}` must be unique and sorted"
            );
            contracts.insert(name, methods);
            continue;
        }

        let Some((name, methods)) = current.as_mut() else {
            panic!("{MANIFEST_REL}:{line_number}: entry outside a `contract` block: `{line}`");
        };
        assert!(
            line.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "{MANIFEST_REL}:{line_number}: invalid method name `{line}` in `{name}`"
        );
        methods.push(line.to_string());
    }

    assert!(
        current.is_none(),
        "{MANIFEST_REL}: unterminated contract block for `{}`",
        current.unwrap().0
    );

    let review_note = review_note.expect("manifest must contain a `review_note:` line");
    let expected: BTreeSet<&str> = CONTRACTS.iter().map(|(name, _)| *name).collect();
    let actual: BTreeSet<&str> = contracts.keys().map(String::as_str).collect();
    assert_eq!(
        actual, expected,
        "manifest must define exactly the pinned contracts"
    );

    Manifest {
        review_note,
        contracts,
    }
}

fn load_manifest() -> Manifest {
    let path = repo_root().join(MANIFEST_REL);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    parse_manifest(&source)
}

/// Derives the exported surface for every pinned contract.
fn actual_surface() -> BTreeMap<String, Vec<String>> {
    let root = repo_root();
    let mut surface = BTreeMap::new();
    for (name, relative) in CONTRACTS {
        let path = root.join(relative);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let methods = exported_methods(&source);
        assert!(
            !methods.is_empty(),
            "no `#[contractimpl]` entry points found in {}",
            path.display()
        );
        surface.insert(name.to_string(), methods);
    }
    surface
}

#[test]
fn public_api_surface_matches_manifest() {
    let manifest = load_manifest();
    let actual = actual_surface();

    for (name, expected) in &manifest.contracts {
        let Some(observed) = actual.get(name) else {
            panic!("no generated surface for `{name}`");
        };
        if observed == expected {
            continue;
        }

        let expected_set: BTreeSet<&String> = expected.iter().collect();
        let observed_set: BTreeSet<&String> = observed.iter().collect();
        let added: Vec<&&String> = observed_set.difference(&expected_set).collect();
        let removed: Vec<&&String> = expected_set.difference(&observed_set).collect();

        panic!(
            "public Soroban API surface for `{name}` changed:\n\
             \n  added entry points:   {added:?}\
             \n  removed entry points: {removed:?}\n\
             \nIf this change is intentional, update `{MANIFEST_REL}` and replace \
             its `review_note:` with the issue/PR that justifies it. \
             Otherwise revert the accidental `pub fn`."
        );
    }
}

#[test]
fn parser_ignores_entry_points_hidden_in_comments_and_strings() {
    let source = r#"
// #[contractimpl]
// impl Fake {
//     pub fn commented_out(env: Env) {}
// }
#[contractimpl]
impl RealContract {
    /// `pub fn` in a doc comment is documentation, not an entry point.
    pub fn real_entry(env: Env) {}

    fn helper(env: Env) {
        let _ = "pub fn in a string literal";
        let _ = "a } brace in a string";
    }
}
"#;

    assert_eq!(exported_methods(source), vec!["real_entry".to_string()]);
}

#[test]
fn manifest_review_note_cites_a_change_reference() {
    let manifest = load_manifest();
    let note = manifest.review_note.trim();
    assert!(
        !note.is_empty(),
        "`{MANIFEST_REL}` must carry a non-empty `review_note:`"
    );
    let cites_reference = note.match_indices('#').any(|(hash, _)| {
        note[hash + 1..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
    });
    assert!(
        cites_reference,
        "`{MANIFEST_REL}` review_note must cite an issue/PR number (e.g. `add foo (#1234)`), got: {note}"
    );
}
