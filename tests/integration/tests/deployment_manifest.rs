use serde_json::Value;
use soroban_sdk::{Bytes, Env};
use std::{
    fmt::Write,
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

const WASM_DIR: &str = "target/wasm32v1-none/release";
const WASMS: [&str; 4] = ["registry", "invoice", "escrow", "pool"];
const CONTRACTS: [&str; 6] = [
    "registry",
    "invoice",
    "escrow_usdc",
    "escrow_xlm",
    "pool_usdc",
    "pool_xlm",
];
const IDS: [&str; 6] = [
    "registry-id",
    "invoice-id",
    "escrow-usdc-id",
    "escrow-xlm-id",
    "pool-usdc-id",
    "pool-xlm-id",
];

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(WASM_DIR)).unwrap();
    for name in WASMS {
        fs::write(dir.path().join(wasm_path(name)), name.as_bytes()).unwrap();
    }
    for args in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    ] {
        assert_success(
            Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap(),
        );
    }
    dir
}

fn wasm_path(name: &str) -> String {
    format!("{WASM_DIR}/trusttrove_{name}.wasm")
}

fn manifest(dir: &Path, action: &str, ids: &[&str]) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/deployment_manifest.py");
    Command::new("python3")
        .arg(script)
        .args([action, "deployments.json"])
        .args(ids)
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn writes_deployment_metadata_and_matches_soroban_hashes() {
    let dir = fixture();
    assert_success(manifest(dir.path(), "write", &IDS));
    let data: Value =
        serde_json::from_slice(&fs::read(dir.path().join("deployments.json")).unwrap()).unwrap();
    let commit = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(commit.status.success());
    assert_eq!(
        data["source_commit"],
        String::from_utf8(commit.stdout).unwrap().trim()
    );
    assert_eq!(
        data["network_passphrase"],
        "Test SDF Network ; September 2015"
    );
    for (key, id) in CONTRACTS.into_iter().zip(IDS) {
        assert_eq!(data[key], id);
    }
    let env = Env::default();
    assert_eq!(data["wasm_sha256"].as_object().unwrap().len(), WASMS.len());
    for name in WASMS {
        let hash = env
            .crypto()
            .sha256(&Bytes::from_slice(&env, name.as_bytes()));
        let mut expected = String::new();
        for byte in hash.to_array() {
            write!(expected, "{byte:02x}").unwrap();
        }
        assert_eq!(data["wasm_sha256"][wasm_path(name)], expected);
    }
    assert_success(manifest(dir.path(), "verify", &[]));
}

#[test]
fn rejects_missing_or_empty_contract_ids_without_overwriting_manifest() {
    let dir = fixture();
    let path = dir.path().join("deployments.json");
    fs::write(&path, b"existing manifest").unwrap();
    let mut empty_id = IDS;
    empty_id[0] = "";
    for ids in [&IDS[..5], &empty_id[..]] {
        let output = manifest(dir.path(), "write", ids);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("expected one nonempty address"));
        assert_eq!(fs::read(&path).unwrap(), b"existing manifest");
    }
}

#[test]
fn verifies_repeatedly_without_mutation_and_rejects_a_one_byte_change() {
    let dir = fixture();
    assert_success(manifest(dir.path(), "write", &IDS));
    let path = dir.path().join("deployments.json");
    let original = fs::read(&path).unwrap();
    for _ in 0..2 {
        assert_success(manifest(dir.path(), "verify", &[]));
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    fs::write(dir.path().join(wasm_path("pool")), b"poom").unwrap();
    let output = manifest(dir.path(), "verify", &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("WASM checksum verification failed"));
    assert_eq!(fs::read(&path).unwrap(), original);
}
