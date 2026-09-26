//! A scratch directory that the wezterm executable resolver will trust.
//!
//! The resolver runs a `wezterm` found outside PATH only when the directories
//! above it pass its ownership rule (`is_trusted_fallback` in src/wezterm.rs).
//! A test that puts a fake `wezterm` under the checkout therefore passes or
//! fails by where the checkout sits: under `/tmp`, which is world-writable,
//! every such candidate is rightly refused. So the directory is chosen, not
//! assumed: the first base where a probe resolves is used, and a host with no
//! such base fails loudly rather than reporting a refusal that proves nothing.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use uuid::Uuid;
use wezterm_attention::wezterm::resolve_wezterm_executable;

pub struct TrustedScratch(pub PathBuf);

impl TrustedScratch {
    pub fn new() -> Self {
        let mut bases = vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"),
            std::env::temp_dir(),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            bases.push(PathBuf::from(home));
        }
        for base in &bases {
            let Ok(base) = fs::canonicalize(base) else {
                continue;
            };
            let scratch = Self(base.join(format!("wa-exec-{}", Uuid::new_v4().simple())));
            if fs::create_dir(&scratch.0).is_err()
                || fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o700)).is_err()
            {
                continue;
            }
            let probe = scratch.executable("wezterm", "exit 0");
            let trusted =
                resolve_wezterm_executable(None, None, None, std::slice::from_ref(&probe)).is_ok();
            fs::remove_file(&probe).expect("remove trust probe");
            if trusted {
                return scratch;
            }
        }
        panic!(
            "none of {bases:?} is trusted for a fallback executable: each has an ancestor \
             owned by another account or writable by group or other"
        );
    }

    /// Write an executable shell script called `name` holding `body`.
    pub fn executable(&self, name: &str, body: &str) -> PathBuf {
        write_script(&self.0.join(name), body)
    }
}

impl Drop for TrustedScratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn write_script(path: &Path, body: &str) -> PathBuf {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make script executable");
    path.to_path_buf()
}
