//! Checks that the `shuttle` feature of this crate turns on every wrapper in the workspace.
//!
//! A wrapper is a workspace crate with a `shuttle` or `deterministic` feature. When a new wrapper
//! is added, this test fails until it is added to the `shuttle` feature in this crate's Cargo.toml.

use std::process::Command;

#[test]
fn shuttle_feature_lists_every_wrapper() {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("failed to run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata printed invalid JSON");
    let packages = metadata["packages"].as_array().expect("cargo metadata has no packages");

    let enabler = packages
        .iter()
        .find(|p| p["name"] == env!("CARGO_PKG_NAME"))
        .expect("this crate is not in the workspace");
    let enabled: Vec<&str> = enabler["features"]["shuttle"]
        .as_array()
        .expect("this crate has no `shuttle` feature")
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();

    let mut missing = Vec::new();
    for package in packages {
        let name = package["name"].as_str().unwrap();
        if name == env!("CARGO_PKG_NAME") {
            continue;
        }
        // Crates with `publish = false` are tests and examples, not wrappers that users depend on.
        if package["publish"]
            .as_array()
            .is_some_and(|registries| registries.is_empty())
        {
            continue;
        }
        for feature in ["shuttle", "deterministic"] {
            if package["features"].get(feature).is_some() {
                let entry = format!("{name}/{feature}");
                if !enabled.contains(&entry.as_str()) {
                    missing.push(entry);
                }
            }
        }
    }

    assert!(
        missing.is_empty(),
        "the `shuttle` feature in wrappers/shuttle_enabler/Cargo.toml must turn on every wrapper. \
         Add a dependency and a feature entry for each of: {missing:?}"
    );
}
