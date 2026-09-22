//! Records which commit the binary was built from, so an installed
//! `attention` can say so through `--version`. A fix once landed while every
//! hook on the machine kept running a binary built minutes before it, and
//! nothing on either side could show that.
//!
//! Only `git` and the standard library are used, and a missing or failing
//! `git` never fails the build: the version then says `unknown`.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    let commit = git(&["rev-parse", "--short=12", "HEAD"]);
    let dirty =
        git(&["status", "--porcelain", "--untracked-files=no"]).map(|status| !status.is_empty());
    let build = match (commit, dirty) {
        (Some(commit), Some(true)) => format!("{commit}-dirty"),
        (Some(commit), _) => commit,
        (None, _) => "unknown".to_owned(),
    };
    println!("cargo:rustc-env=ATTENTION_BUILD_COMMIT={build}");
    // HEAD moves without any source file changing, so watch the ref itself;
    // the installer also touches this file so an install always re-reads it.
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        if let Some(head) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={git_dir}/{head}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
