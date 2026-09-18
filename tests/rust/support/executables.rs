//! Where the suite finds the external programs it drives.
//!
//! Several tests run a real `node`, a real `wezterm` or a real `python3`, and
//! they clear the child environment first because production PATH resolution is
//! part of what is under test. Those programs used to be named by absolute
//! path -- `/opt/homebrew/bin/node` -- which is this machine's address for the
//! program rather than a way to find it, so the suite failed on any host that
//! installs them elsewhere. That matters more than usual here: the repository
//! has no CI, so a gate that only runs in one home directory is a gate nobody
//! else can run at all.

use std::path::{Path, PathBuf};

/// Resolve one external program to an absolute path.
///
/// `ATTENTION_TEST_<PROGRAM>` wins when it is set, so a host can point at a
/// specific build. Otherwise the PATH that started `cargo test` is searched --
/// the same PATH the developer or a CI runner already arranged. A program that
/// cannot be found fails loudly, because a silently skipped dependency is how a
/// suite comes to prove less than it claims.
pub fn resolve(program: &str) -> PathBuf {
    let override_name = format!("ATTENTION_TEST_{}", program.to_uppercase());
    if let Some(value) = std::env::var_os(&override_name) {
        let path = PathBuf::from(value);
        assert!(
            path.is_absolute() && path.is_file(),
            "{override_name} must name an existing absolute path, got {path:?}"
        );
        return path;
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path_var)
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable_file(candidate))
        .unwrap_or_else(|| {
            panic!(
                "{program} is required by this test and was not found on PATH; \
                 install it or set {override_name} to its absolute path"
            )
        })
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// A PATH for a child whose own scripts must resolve the same installation.
///
/// The resolved program's directory is appended rather than prepended, so a
/// caller that puts a synthetic executable first keeps that precedence.
pub fn child_path(first: &[&Path], programs: &[&Path]) -> String {
    let mut entries: Vec<PathBuf> = first.iter().map(|path| path.to_path_buf()).collect();
    for program in programs {
        if let Some(directory) = program.parent()
            && !entries.iter().any(|existing| existing == directory)
        {
            entries.push(directory.to_path_buf());
        }
    }
    for fallback in ["/usr/bin", "/bin"] {
        entries.push(PathBuf::from(fallback));
    }
    std::env::join_paths(entries)
        .expect("child PATH")
        .to_string_lossy()
        .into_owned()
}
