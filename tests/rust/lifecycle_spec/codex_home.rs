//! The Codex home `inspect` names for an available Codex host: the home its
//! start-up environment names, only when the arg0 helper lock the host holds
//! open lies in that same directory and the host is still the one recorded.

use super::host_facet::{SESSION, facet, facts_with, launch, shell_session};
use super::self_claim::records;
use super::*;
use std::path::Path;
use wezterm_attention::wezterm::Arg0Lock;

/// `relative` under the pane's scratch directory.
fn scratch(setup: &Setup, relative: &str) -> PathBuf {
    PathBuf::from(&setup.env["HOME"]).join(relative)
}

/// A directory under the pane's scratch, created, by its real path.
fn dir(setup: &Setup, relative: &str) -> PathBuf {
    let path = scratch(setup, relative);
    fs::create_dir_all(&path).expect("create directory");
    path.canonicalize().expect("real path")
}

/// The path of `relative` under the pane's scratch, as a variable holds it.
fn at(setup: &Setup, relative: &str) -> Vec<u8> {
    scratch(setup, relative)
        .into_os_string()
        .into_encoded_bytes()
}

/// The arg0 helper lock Codex creates in `home`, as a scan of the host names
/// it: by its real path, with its device and inode.
fn lock_in(home: &Path) -> Arg0Lock {
    let path = home.join("tmp/arg0/codex-arg0Ab12Cd/.lock");
    fs::create_dir_all(path.parent().expect("lock directory")).expect("lock directory");
    fs::write(&path, b"").expect("lock file");
    let metadata = fs::metadata(&path).expect("lock metadata");
    Arg0Lock::of(path, &metadata)
}

/// What the host's start-up environment and its open files give.
fn host_reads(setup: &Setup, codex_home: Option<&[u8]>, home: Option<&[u8]>, locks: Arg0Locks) {
    setup.processes.start_homes.lock().expect("lock").insert(
        AGENT_PID,
        StartHomeVariables {
            codex_home: codex_home.map(<[u8]>::to_vec),
            home: home.map(<[u8]>::to_vec),
        },
    );
    setup
        .processes
        .arg0_locks
        .lock()
        .expect("lock")
        .insert(AGENT_PID, locks);
}

/// The user database's home for the host's user, `relative` under the
/// pane's scratch.
fn passwd_home(setup: &Setup, relative: &str) {
    setup
        .processes
        .passwd_homes
        .lock()
        .expect("lock")
        .insert(AGENT_PID, at(setup, relative));
}

/// The host record `inspect` gives for `provider`'s binding, which must be
/// available.
fn record(setup: &Setup, provider: &str) -> Value {
    let host = facet(setup, provider);
    assert_eq!(host["availability"], "available", "{host}");
    host["record"].clone()
}

/// The home `inspect` names for the Codex binding, or its refusal; exactly
/// one of the two is given.
fn answer(setup: &Setup) -> std::result::Result<String, String> {
    let record = record(setup, "codex");
    match (record.get("config_dir"), record.get("config_dir_refusal")) {
        (Some(home), None) => Ok(home.as_str().expect("text").to_owned()),
        (None, Some(reason)) => Err(reason.as_str().expect("text").to_owned()),
        other => panic!("exactly one of config_dir and config_dir_refusal: {other:?}"),
    }
}

fn named(home: &Path) -> std::result::Result<String, String> {
    Ok(home.to_str().expect("UTF-8").to_owned())
}

fn refused(reason: &str) -> std::result::Result<String, String> {
    Err(reason.to_owned())
}

#[test]
fn a_default_home_is_named_when_the_host_s_lock_lies_in_it() {
    let setup = shell_session("codex");
    let home = dir(&setup, "a/.codex");
    host_reads(
        &setup,
        None,
        Some(&at(&setup, "a")),
        Arg0Locks::Complete(vec![lock_in(&home)]),
    );
    assert_eq!(answer(&setup), named(&home));
    // The binding keeps what its start hook saw: no CODEX_HOME.
    let binding = facts_with(
        &setup,
        &binding_id("codex", SESSION, &launch(&setup)),
        &setup.tty,
    )
    .binding;
    let binding = serde_json::to_value(binding).expect("binding JSON");
    assert!(binding.get("config_dir").is_none(), "{binding}");
}

#[test]
fn an_explicit_codex_home_wins_over_home() {
    let setup = shell_session("codex");
    dir(&setup, "a/.codex");
    let explicit = dir(&setup, "b/.codex");
    host_reads(
        &setup,
        Some(&at(&setup, "b/.codex")),
        Some(&at(&setup, "a")),
        Arg0Locks::Complete(vec![lock_in(&explicit)]),
    );
    assert_eq!(answer(&setup), named(&explicit));
}

#[test]
fn an_empty_codex_home_and_one_that_is_not_utf8_count_as_unset() {
    for codex_home in [&b""[..], b"/b\xff/.codex"] {
        let setup = shell_session("codex");
        let home = dir(&setup, "a/.codex");
        host_reads(
            &setup,
            Some(codex_home),
            Some(&at(&setup, "a")),
            Arg0Locks::Complete(vec![lock_in(&home)]),
        );
        assert_eq!(answer(&setup), named(&home), "{codex_home:?}");
    }
}

#[test]
fn a_symlinked_home_is_named_by_its_real_path() {
    let setup = shell_session("codex");
    let real = dir(&setup, "b/.codex");
    std::os::unix::fs::symlink(&real, scratch(&setup, "link")).expect("symlink");
    host_reads(
        &setup,
        Some(&at(&setup, "link")),
        None,
        Arg0Locks::Complete(vec![lock_in(&real)]),
    );
    assert_eq!(answer(&setup), named(&real));

    let setup = shell_session("codex");
    let real = dir(&setup, "a/.codex");
    std::os::unix::fs::symlink(real.parent().expect("a"), scratch(&setup, "hlink"))
        .expect("symlink");
    host_reads(
        &setup,
        None,
        Some(&at(&setup, "hlink")),
        Arg0Locks::Complete(vec![lock_in(&real)]),
    );
    assert_eq!(answer(&setup), named(&real));
}

/// As of Codex 0.162.0 and 0.162.1, a `.env` in the default home that sets
/// `HOME`, with `CODEX_HOME` unset, moves Codex to `.codex` under that home
/// after it starts; its lock then lies there.
#[test]
fn a_home_the_host_moved_to_after_start_is_refused() {
    let setup = shell_session("codex");
    dir(&setup, "a/.codex");
    let moved = dir(&setup, "c/.codex");
    host_reads(
        &setup,
        None,
        Some(&at(&setup, "a")),
        Arg0Locks::Complete(vec![lock_in(&moved)]),
    );
    assert_eq!(answer(&setup), refused("home_changed_after_start"));
}

#[test]
fn relative_values_are_refused_and_never_replaced_by_the_passwd_home() {
    for (codex_home, home, reason) in [
        (Some("rel/.codex"), None, "codex_home_relative"),
        (Some("~/codex"), None, "codex_home_relative"),
        (None, Some("relhome"), "home_relative"),
        (None, Some("~/h"), "home_relative"),
    ] {
        let setup = shell_session("codex");
        let passwd = dir(&setup, "p/.codex");
        passwd_home(&setup, "p");
        host_reads(
            &setup,
            codex_home.map(str::as_bytes),
            home.map(str::as_bytes),
            Arg0Locks::Complete(vec![lock_in(&passwd)]),
        );
        assert_eq!(answer(&setup), refused(reason), "{codex_home:?} {home:?}");
    }
}

#[test]
fn a_home_that_is_not_utf8_is_refused() {
    let setup = shell_session("codex");
    host_reads(
        &setup,
        None,
        Some(b"/h\xff"),
        Arg0Locks::Complete(Vec::new()),
    );
    assert_eq!(answer(&setup), refused("home_not_utf8"));
}

/// An empty or unset `HOME` takes `.codex` under the user database's home, as
/// dirs-sys 0.5.0 does for Codex 0.162.0 and 0.162.1, read through the fake
/// here so no test reaches the real user's.
#[test]
fn an_empty_or_unset_home_takes_the_passwd_home() {
    for home in [None, Some(&b""[..])] {
        let setup = shell_session("codex");
        let passwd = dir(&setup, "p/.codex");
        passwd_home(&setup, "p");
        host_reads(
            &setup,
            None,
            home,
            Arg0Locks::Complete(vec![lock_in(&passwd)]),
        );
        assert_eq!(answer(&setup), named(&passwd), "{home:?}");
    }
    let setup = shell_session("codex");
    host_reads(&setup, None, None, Arg0Locks::Complete(Vec::new()));
    assert_eq!(answer(&setup), refused("home_unresolvable"));
}

#[test]
fn a_home_that_names_no_directory_is_refused() {
    let setup = shell_session("codex");
    fs::write(scratch(&setup, "file"), b"").expect("file");
    dir(&setup, "no-codex-directory");
    for (codex_home, home, reason) in [
        (Some("missing/.codex"), None, "codex_home_unresolvable"),
        (Some("file"), None, "codex_home_unresolvable"),
        (None, Some("no-codex-directory"), "home_unresolvable"),
    ] {
        host_reads(
            &setup,
            codex_home.map(|value| at(&setup, value)).as_deref(),
            home.map(|value| at(&setup, value)).as_deref(),
            Arg0Locks::Complete(Vec::new()),
        );
        assert_eq!(answer(&setup), refused(reason), "{codex_home:?} {home:?}");
    }
}

#[test]
fn an_unreadable_environment_is_refused() {
    let setup = shell_session("codex");
    let home = dir(&setup, "a/.codex");
    setup
        .processes
        .arg0_locks
        .lock()
        .expect("lock")
        .insert(AGENT_PID, Arg0Locks::Complete(vec![lock_in(&home)]));
    assert_eq!(answer(&setup), refused("environment_unreadable"));
}

#[test]
fn a_home_needs_one_whole_scan_that_finds_its_lock() {
    let setup = shell_session("codex");
    let home = dir(&setup, "a/.codex");
    let other = dir(&setup, "c/.codex");
    for (locks, reason) in [
        (Arg0Locks::Complete(Vec::new()), "home_unobserved"),
        (
            Arg0Locks::Complete(vec![lock_in(&home), lock_in(&other)]),
            "home_ambiguous",
        ),
        (Arg0Locks::Unavailable, "home_observation_unavailable"),
    ] {
        host_reads(&setup, None, Some(&at(&setup, "a")), locks);
        assert_eq!(answer(&setup), refused(reason));
    }
}

#[test]
fn a_lock_no_longer_at_its_path_is_refused() {
    let setup = shell_session("codex");
    let home = dir(&setup, "a/.codex");
    let mut lock = lock_in(&home);
    // The file at the lock's path is not the one the host holds.
    lock.inode += 1;
    host_reads(
        &setup,
        None,
        Some(&at(&setup, "a")),
        Arg0Locks::Complete(vec![lock]),
    );
    assert_eq!(answer(&setup), refused("home_changed_after_start"));

    let setup = shell_session("codex");
    let home = dir(&setup, "a/.codex");
    let lock = lock_in(&home);
    fs::remove_file(&lock.path).expect("remove the lock file");
    host_reads(
        &setup,
        None,
        Some(&at(&setup, "a")),
        Arg0Locks::Complete(vec![lock]),
    );
    assert_eq!(answer(&setup), refused("home_changed_after_start"));
}

/// A home moved while it is read is still the same directory, but no longer
/// at the path that would name it.
#[test]
fn a_home_moved_while_it_is_read_is_refused() {
    let setup = shell_session("codex");
    let home = dir(&setup, "a/.codex");
    let lock = lock_in(&home);
    let from = home.parent().expect("a").to_path_buf();
    let to = from.with_file_name("moved");
    // As the kernel names the held lock once its directory has moved.
    let moved = Arg0Lock {
        path: to
            .join(".codex")
            .join(lock.path.strip_prefix(&home).expect("lock in home")),
        ..lock
    };
    host_reads(
        &setup,
        None,
        Some(&at(&setup, "a")),
        Arg0Locks::Complete(vec![moved]),
    );
    *setup.processes.on_scan.lock().expect("scan hook lock") = Some(Box::new(move || {
        fs::rename(&from, &to).expect("move the home");
    }));
    assert_eq!(answer(&setup), refused("home_changed_after_start"));
}

/// The host is read again after its home: one that another process replaced
/// meanwhile names no home.
#[test]
fn a_host_that_changed_during_the_read_names_no_home() {
    let setup = shell_session("codex");
    let home = dir(&setup, "a/.codex");
    host_reads(
        &setup,
        None,
        Some(&at(&setup, "a")),
        Arg0Locks::Complete(vec![lock_in(&home)]),
    );
    let scans = setup.processes.scans.clone();
    let mut replaced = setup.processes.facts(AGENT_PID);
    replaced.start.microseconds += 1;
    *setup.processes.on_read.lock().expect("hook lock") = Some(std::sync::Arc::new(move |pid| {
        (pid == AGENT_PID && scans.load(std::sync::atomic::Ordering::SeqCst) > 0)
            .then(|| ProcessRead::Found(replaced.clone()))
    }));
    assert_eq!(answer(&setup), refused("host_changed_during_read"));
}

#[test]
fn other_providers_name_no_home() {
    for provider in ["claude", "pi"] {
        let setup = shell_session(provider);
        let home = dir(&setup, "a/.codex");
        host_reads(
            &setup,
            None,
            Some(&at(&setup, "a")),
            Arg0Locks::Complete(vec![lock_in(&home)]),
        );
        let record = record(&setup, provider);
        assert!(
            record.get("config_dir").is_none() && record.get("config_dir_refusal").is_none(),
            "{provider}: {record}"
        );
    }
}

/// Naming the home writes nothing: the records the hooks left are byte for
/// byte the same after `inspect`.
#[test]
fn naming_the_home_writes_no_record() {
    let setup = shell_session("codex");
    let home = dir(&setup, "a/.codex");
    host_reads(
        &setup,
        None,
        Some(&at(&setup, "a")),
        Arg0Locks::Complete(vec![lock_in(&home)]),
    );
    let before = records(&setup);
    assert_eq!(answer(&setup), named(&home));
    assert_eq!(records(&setup), before);
}
