use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(root: &Path, state: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_openlocus"))
        .current_dir(root)
        .arg("--root")
        .arg(root)
        .arg("--state-root")
        .arg(state)
        .args(arguments)
        .output()
        .unwrap()
}

fn json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn product_flow_builds_queries_updates_and_validates() {
    let source = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("lib.rs"),
        "pub fn original_marker() {}\n",
    )
    .unwrap();

    let built = json(run(source.path(), state.path(), &["index", "build"]));
    assert_eq!(built["files_indexed"], 1);

    let current = json(run(
        source.path(),
        state.path(),
        &["query", "original_marker"],
    ));
    assert_eq!(current["status"], "complete");
    assert_eq!(current["evidence"].as_array().unwrap().len(), 1);
    assert_eq!(
        current["evidence"][0]["channels"],
        serde_json::json!(["literal", "bm25"])
    );
    assert!(!source.path().join(".openlocus").exists());

    let citation = serde_json::json!([{
        "path": current["evidence"][0]["path"],
        "start_line": current["evidence"][0]["start_line"],
        "end_line": current["evidence"][0]["end_line"],
        "content_sha": current["evidence"][0]["content_sha"],
    }]);
    fs::write(
        source.path().join("citations.json"),
        serde_json::to_vec(&citation).unwrap(),
    )
    .unwrap();
    let validated = json(run(
        source.path(),
        state.path(),
        &["citations", "validate", "citations.json"],
    ));
    assert_eq!(validated[0]["valid"], true);

    fs::write(source.path().join("lib.rs"), "pub fn changed_marker() {}\n").unwrap();
    let stale = json(run(
        source.path(),
        state.path(),
        &["query", "original_marker"],
    ));
    assert_eq!(stale["status"], "partial");
    assert!(stale["evidence"].as_array().unwrap().is_empty());

    json(run(
        source.path(),
        state.path(),
        &["index", "update", "lib.rs"],
    ));
    let updated = json(run(
        source.path(),
        state.path(),
        &["query", "changed_marker"],
    ));
    assert_eq!(updated["status"], "complete");
    assert_eq!(updated["evidence"].as_array().unwrap().len(), 1);

    fs::write(source.path().join(".env.local"), "SECRET=value\n").unwrap();
    assert!(
        !run(source.path(), state.path(), &["read", ".env.local"])
            .status
            .success()
    );
}
