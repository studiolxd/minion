//! Runs `minion corpus` against the checked-in corpus, if there is one.
//!
//! Both halves of that are typically missing on a fresh checkout: the
//! recognition model is ~670 MB and not in git (see `CLAUDE.md`), and the
//! corpus itself is nobody's voice but the machine it was recorded on (see
//! `minion/corpus/README.md` — only `corpus.toml` and `baseline.toml` are
//! committed). Skipped rather than failed in either case, so `cargo test`
//! stays green without either.
//!
//! Minion is a binary crate with no library target, so this drives the
//! built binary as a subprocess rather than calling into `corpus::run`
//! directly — the same shape as any other external caller of `minion
//! corpus`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the model would be, mirroring `models::directory` and
/// `models::present` (both private to the binary and so not reachable from
/// here).
fn model_present() -> bool {
    let dir = std::env::var("MINION_MODEL").ok().map(PathBuf::from).or_else(|| {
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join("Library/Application Support/Minion/model"))
    });
    dir.is_some_and(|dir| dir.join("vocab.txt").exists() && dir.join("encoder-model.onnx").exists())
}

#[test]
fn corpus_meets_its_baseline() {
    let corpus_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    if !corpus_dir.join("corpus.toml").exists() {
        eprintln!(
            "skipping: no {} — see minion/corpus/README.md",
            corpus_dir.join("corpus.toml").display()
        );
        return;
    }
    if !model_present() {
        eprintln!("skipping: no recognition model found — see CLAUDE.md for how to fetch one");
        return;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_minion"))
        .arg("corpus")
        .arg(&corpus_dir)
        .output()
        .expect("running `minion corpus`");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    print!("{stdout}");
    assert!(
        output.status.success(),
        "`minion corpus {}` reported a regression against baseline.toml:\n{stdout}{stderr}",
        corpus_dir.display(),
    );
}
