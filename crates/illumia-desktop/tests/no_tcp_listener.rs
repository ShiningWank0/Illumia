//! all-in-one モードが TCP を一切 bind しないことの検証 (docs/01_architecture.md の必須要件)。
//!
//! 「localhost で listen して認証で弾く」方式は禁止されている。ここでは
//! ローカル backend を構築・実行し、プロセスに listen 中の TCP ソケットが
//! 増えていないことを OS 越しに確認する。
//!
//! 型レベルでも `LocalBackend` は HTTP クライアント / listener を持たないが、
//! 依存の追加で意図せず listener が生えることを防ぐため実測でも押さえる。

use illumia_desktop::backend::{Backend, LocalBackend};

/// 自プロセスが listen 中の TCP ソケット数を数える。
///
/// macOS / Linux では `lsof` が使える。使えない環境では計測を諦めて None を返す
/// (CI の Linux ランナーでは利用可能)。
fn listening_tcp_sockets() -> Option<usize> {
    let output = std::process::Command::new("lsof")
        .args([
            "-a",
            "-p",
            &std::process::id().to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
            "-P",
            "-n",
        ])
        .output()
        .ok()?;
    if !output.status.success() && output.stdout.is_empty() {
        // 該当ソケットが 0 件のとき lsof は非ゼロ終了しうる。
        return Some(0);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().filter(|line| line.contains("LISTEN")).count())
}

#[test]
fn all_in_one_backend_never_binds_a_tcp_listener() {
    let before = listening_tcp_sockets();

    let dir = std::env::temp_dir().join(format!(
        "illumia-desktop-no-tcp-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let backend = LocalBackend::open(&dir).expect("ローカル backend を開ける");

    // 実際に DB を触る操作を通しても listener が生えないことを確認する。
    assert!(!backend.uses_network(), "all-in-one は network を使わない");
    let buckets = backend
        .buckets(illumia_core::timeline::Granularity::Day)
        .expect("空 DB でもバケット取得は成功する");
    assert!(buckets.is_empty(), "新規 DB にバケットは無い");

    let after = listening_tcp_sockets();

    match (before, after) {
        (Some(before), Some(after)) => {
            assert_eq!(
                after, before,
                "all-in-one 実行中に listen 中の TCP ソケットが増えた \
                 (docs/01: TCP を一切 bind しないこと)"
            );
        }
        _ => {
            // lsof が無い環境では実測できない。型レベルの保証だけ残す。
            eprintln!("lsof が使えないため TCP listener の実測を省略した");
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}
