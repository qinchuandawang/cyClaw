use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};
use std::thread;

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

#[test]
fn creates_fact_with_structured_symbol_evidence() {
    let temp = tempfile::tempdir().expect("应能创建临时项目");
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/main.rs"), "fn governed() {}\n").unwrap();

    run(
        temp.path(),
        &[
            "fact",
            "create",
            "结构化证据应记录符号哈希",
            "--fact-type",
            "constraint",
            "--evidence",
            "src/main.rs",
            "--evidence-symbol",
            "governed",
            "--evidence-hash-scope",
            "symbol",
            "--evidence-type",
            "source",
            "--apply",
        ],
    );

    let facts = std::fs::read_to_string(temp.path().join(".cyclaw/memory/facts.jsonl"))
        .expect("应用后应生成 Fact Ledger");
    let fact = serde_json::from_str::<serde_json::Value>(facts.lines().next().unwrap())
        .expect("事实应为合法 JSON");
    let evidence = &fact["evidence_details"][0];
    assert_eq!(evidence["symbol"], "governed");
    assert_eq!(evidence["hash_scope"], "symbol");
    assert_eq!(evidence["evidence_type"], "source");
    assert!(
        evidence["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
}

#[test]
fn only_one_cli_process_can_apply_the_same_fact_patch() {
    let temp = tempfile::tempdir().expect("应能创建临时项目");
    let preview = run(
        temp.path(),
        &[
            "fact",
            "create",
            "多进程只能应用一次同一 Fact Patch",
            "--fact-type",
            "constraint",
        ],
    );
    let id = patch_id(&preview);
    let root = Arc::new(temp.path().to_path_buf());
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let root = Arc::clone(&root);
            let barrier = Arc::clone(&barrier);
            let id = id.clone();
            thread::spawn(move || {
                barrier.wait();
                Command::new(env!("CARGO_BIN_EXE_cyclaw"))
                    .args(["fact", "apply", &id, "--path"])
                    .arg(root.as_path())
                    .output()
                    .expect("应能启动竞争 CLI 进程")
            })
        })
        .collect::<Vec<_>>();
    let outputs = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        outputs
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1
    );
    let facts = std::fs::read_to_string(temp.path().join(".cyclaw/memory/facts.jsonl"))
        .expect("成功进程应写入 Fact Ledger");
    assert_eq!(facts.lines().count(), 1);
    let patch = std::fs::read_to_string(
        temp.path()
            .join(".cyclaw/memory/fact-patches")
            .join(format!("{}.json", id)),
    )
    .expect("Fact Patch 应继续存在");
    let patch: serde_json::Value = serde_json::from_str(&patch).unwrap();
    assert_eq!(patch["status"], "applied");
    let transaction_dir = temp.path().join(".cyclaw/memory/fact-transactions");
    assert_eq!(
        std::fs::read_dir(transaction_dir)
            .unwrap()
            .filter_map(Result::ok)
            .count(),
        0
    );
}
