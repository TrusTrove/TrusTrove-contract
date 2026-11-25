    #[test]
fn test_deployment_manifest_scripts_execution() {
    let manifest_name = "test-deployment-manifest.json";

    // Run generate-manifest.sh
    let gen_status = Command::new("bash")
        .arg("scripts/generate-manifest.sh")
        .arg(manifest_name)
        .status();

    if let Ok(status) = gen_status {
        assert!(status.success(), "generate-manifest.sh should succeed");
    } else {
        // If bash or scripts are missing in restricted test environments, skip gracefully
        return;
    }

    assert!(Path::new(manifest_name).exists(), "manifest file should be generated");

    // Run verify-manifest.sh
    let ver_status = Command::new("bash")
        .arg("scripts/verify-manifest.sh")
        .arg(manifest_name)
        .status();
        
    assert!(ver_status.unwrap().success(), "verify-manifest.sh should succeed on valid manifest");

    // Test invalid-input rejection: tamper with a hash in the manifest
    let content = fs::read_to_string(manifest_name).unwrap();
    let tampered = content.replace('a', 'b');
    fs::write(manifest_name, tampered).unwrap();

    let ver_fail_status = Command::new("bash")
        .arg("scripts/verify-manifest.sh")
        .arg(manifest_name)
        .status();
        
    assert!(!ver_fail_status.unwrap().success(), "verify-manifest.sh should reject tampered manifest");

    let _ = fs::remove_file(manifest_name);
}

#[cfg(test)]
mod tests {
    use super::*;
use std::process::Command;
use std::fs;
use std::path::Path;

#[test]
fn test_deployment_manifest_scripts_execution() {
    let manifest_name = "test-deployment-manifest.json";
    
    let gen_status = Command::new("bash")
        .arg("scripts/generate-manifest.sh")
        .arg(manifest_name)
        .status();
        
    if let Ok(status) = gen_status {
        assert!(status.success(), "generate-manifest.sh should succeed");
    } else {
        return;
    }

    assert!(Path::new(manifest_name).exists(), "manifest file should be generated");

    let ver_status = Command::new("bash")
        .arg("scripts/verify-manifest.sh")
        .arg(manifest_name)
        .status();
        
    assert!(ver_status.unwrap().success(), "verify-manifest.sh should succeed on valid manifest");

    let content = fs::read_to_string(manifest_name).unwrap();
    let tampered = content.replace('a', 'b');
    fs::write(manifest_name, tampered).unwrap();

    let ver_fail_status = Command::new("bash")
        .arg("scripts/verify-manifest.sh")
        .arg(manifest_name)
        .status();
        
    assert!(!ver_fail_status.unwrap().success(), "verify-manifest.sh should reject tampered manifest");

    let _ = fs::remove_file(manifest_name);
    }
}
