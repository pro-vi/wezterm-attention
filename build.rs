// Records which commit the binary was built from, so an installed
// `attention` can say so through `--version`: without it, nothing shows
// whether the hooks on a machine run a binary built before a fix landed.
//
// Only `git` and the standard library are used, and a missing or failing
// `git` never fails the build: the version then says `unknown`.
//
// Plain comments, not `//!`, because the tests include this file to check
// the identity it computes.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The files and directories the binary is built from. `-dirty` means one of
/// them differs from the commit, and cargo re-runs this script when one of
/// them changes; the two lists must be the same, or the flag goes stale.
const BINARY_INPUTS: [&str; 5] = ["src", "protocol", "build.rs", "Cargo.toml", "Cargo.lock"];

struct BuildIdentity {
    build: String,
    watched: Vec<PathBuf>,
}

fn git(directory: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn build_identity(directory: &Path) -> BuildIdentity {
    let mut watched = BINARY_INPUTS
        .iter()
        .map(|input| directory.join(input))
        .collect::<Vec<_>>();
    let build = commit_identity(directory, &mut watched).unwrap_or_else(|| "unknown".to_owned());
    BuildIdentity { build, watched }
}

fn commit_identity(directory: &Path, watched: &mut Vec<PathBuf>) -> Option<String> {
    // Only this crate's own checkout names a commit. A copy vendored inside
    // another repository would otherwise report that repository's HEAD.
    let top = git(directory, &["rev-parse", "--show-toplevel"])?;
    if std::fs::canonicalize(top).ok()? != std::fs::canonicalize(directory).ok()? {
        return None;
    }
    // HEAD moves without any input changing, so watch what git writes when
    // it moves. `--git-path` resolves each file where this checkout keeps
    // it: a linked worktree keeps HEAD and its log apart from the shared
    // refs. A watched path that does not exist makes cargo re-run the script
    // on every build, so only existing ones are watched. `logs/HEAD` changes
    // on every commit, reset and checkout even when the branch's own ref is
    // packed and has no file.
    let mut names = vec![
        "HEAD".to_owned(),
        "logs/HEAD".to_owned(),
        "packed-refs".to_owned(),
    ];
    names.extend(git(directory, &["symbolic-ref", "-q", "HEAD"]));
    for name in names {
        if let Some(path) = git(directory, &["rev-parse", "--git-path", &name]) {
            let path = directory.join(path);
            if path.exists() {
                watched.push(path);
            }
        }
    }
    let commit = git(directory, &["rev-parse", "--short=12", "HEAD"])?;
    let mut status = vec!["status", "--porcelain", "--"];
    status.extend(BINARY_INPUTS);
    Some(match git(directory, &status) {
        Some(changes) if !changes.is_empty() => format!("{commit}-dirty"),
        _ => commit,
    })
}

#[allow(dead_code)]
fn main() {
    let directory = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    );
    let identity = build_identity(&directory);
    for path in &identity.watched {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-env=ATTENTION_BUILD_COMMIT={}", identity.build);
}
