use std::path::Path;
use std::process::{Command, Output};

fn run(project_root: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cyclaw"));
    command.args(arguments).arg("--path").arg(project_root);
    let output = command.output().expect("应能启动 cyclaw CLI");
    assert!(
        output.status.success(),
        "CLI 执行失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn patch_id(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("事实草稿: "))
        .expect("输出应包含事实草稿 ID")
        .to_string()
}

#[test]
fn previews_applies_and_reverts_fact_through_real_cli() {
    let temp = tempfile::tempdir().expect("应能创建临时项目");
    let preview = run(
        temp.path(),
        &[
            "fact",
            "create",
            "CLI 修改必须经过 Fact Patch",
            "--fact-type",
            "constraint",
            "--evidence",
            "src/main.rs",
        ],
    );
    let id = patch_id(&preview);
    assert!(!temp.path().join(".cyclaw/memory/facts.jsonl").exists());

    run(temp.path(), &["fact", "apply", &id]);
    let facts = std::fs::read_to_string(temp.path().join(".cyclaw/memory/facts.jsonl"))
        .expect("应用后应生成 Fact Ledger");
    let fact = serde_json::from_str::<serde_json::Value>(facts.lines().next().unwrap())
        .expect("事实应为合法 JSON");
    assert_eq!(fact["status"], "active");
    assert_eq!(fact["evidence_details"][0]["path"], "src/main.rs");
    let fact_id = fact["id"].as_str().unwrap();

    let listed = run(
        temp.path(),
        &["fact", "list", "--status", "applied", "--limit", "1"],
    );
    assert!(String::from_utf8_lossy(&listed.stdout).contains(&id));
    run(temp.path(), &["fact", "verify", fact_id]);
    let verifications = run(
        temp.path(),
        &["fact", "verifications", "--fact-id", fact_id],
    );
    assert!(String::from_utf8_lossy(&verifications.stdout).contains(fact_id));

    run(temp.path(), &["fact", "revert", &id]);
    let facts = std::fs::read_to_string(temp.path().join(".cyclaw/memory/facts.jsonl"))
        .expect("撤销后账本仍应可读取");
    assert!(facts.trim().is_empty());
}
