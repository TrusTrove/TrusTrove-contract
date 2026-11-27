//! Exercise the deployment CLI without invoking Stellar or touching network state.
#![cfg(unix)]

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

const CONFIG: &str =
    "DEPLOYER_ACCOUNT=testing\nUSDC_ISSUER=fixture-issuer\nXLM_ASSET=fixture-xlm\n";
const OUTPUTS: [&str; 2] = [".deployed-addresses", ".env.deployed"];

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".env"), CONFIG).unwrap();
    let stellar = dir.path().join("stellar");
    fs::write(&stellar, "#!/bin/sh\ntouch stellar-invoked\nexit 99\n").unwrap();
    fs::set_permissions(stellar, fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn dry_run(dir: &Path, executable: &str, script: &str, args: &[&str]) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts")
        .join(script);
    let original_path = env::var_os("PATH").unwrap_or_default();
    let path =
        env::join_paths(std::iter::once(dir.to_path_buf()).chain(env::split_paths(&original_path)))
            .unwrap();
    let mut command = Command::new(executable);
    if executable == "pwsh" {
        command.args(["-NoProfile", "-NonInteractive", "-File"]);
    }
    let output = command
        .arg(script)
        .args(args)
        .env("PATH", path)
        .env_remove("DEPLOYER_ADDRESS")
        .env_remove("AGENT_REGISTRY_CONTRACT")
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(!dir.join("stellar-invoked").exists());
    output
}

fn stdout(output: Output, success: bool) -> String {
    assert_eq!(
        output.status.success(),
        success,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn shell_prints_deployments_in_order_without_creating_files() {
    let dir = fixture();
    let plan = stdout(
        dry_run(dir.path(), "bash", "deploy.sh", &["--dry-run"]),
        true,
    );
    assert!(plan.contains("<address-for-testing>"));
    assert!(
        plan.find("Deploying registry_contract").unwrap()
            < plan.find("Deploying invoice_contract").unwrap()
    );
    for output in OUTPUTS {
        assert!(!dir.path().join(output).exists());
    }
}

#[test]
fn shell_rejects_invalid_contract_without_creating_files() {
    let dir = fixture();
    let plan = stdout(
        dry_run(
            dir.path(),
            "bash",
            "deploy.sh",
            &["--dry-run", "--only", "unknown"],
        ),
        false,
    );
    assert!(plan.contains("unknown contract key"));
    for output in OUTPUTS {
        assert!(!dir.path().join(output).exists());
    }
}

fn assert_fresh_preserves_files(executable: &str, script: &str, args: &[&str]) {
    let dir = fixture();
    for output in OUTPUTS {
        fs::write(dir.path().join(output), "registry=existing-address\n").unwrap();
    }
    for _ in 0..2 {
        let plan = stdout(dry_run(dir.path(), executable, script, args), true);
        assert!(plan.contains("<address-for-testing>"));
        assert!(!plan.contains("existing-address"));
        for output in OUTPUTS {
            assert_eq!(
                fs::read_to_string(dir.path().join(output)).unwrap(),
                "registry=existing-address\n"
            );
        }
    }
}

#[test]
fn shell_fresh_dry_run_can_repeat_without_rewriting_files() {
    assert_fresh_preserves_files("bash", "deploy.sh", &["--dry-run", "--fresh"]);
}

#[test]
fn powershell_fresh_dry_run_can_repeat_without_rewriting_files() {
    if Command::new("pwsh")
        .args(["-NoProfile", "-Command", "exit 0"])
        .status()
        .is_ok_and(|status| status.success())
    {
        assert_fresh_preserves_files("pwsh", "deploy.ps1", &["-DryRun", "-Fresh"]);
    } else {
        eprintln!("PowerShell unavailable; dry-run subprocess test skipped");
    }
}
