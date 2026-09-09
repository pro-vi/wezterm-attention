use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;
use wezterm_attention::identity::{length_prefixed_digest, pane_address};
use wezterm_attention::protocol::{
    EMBEDDED_MANIFEST, eligible_subagent_presence, manifest, parse_manifest, parse_record_value,
    parse_wire_value,
};
use wezterm_attention::query::{read_bindings, read_bindings_with_ports};
use wezterm_attention::records::{
    RecordIdentity, atomic_replace, launch_path, pane_path, state_root, with_lock,
};
use wezterm_attention::wezterm::{
    Clock, PaneLister, PaneRow, RuntimePorts, SystemTtyWriter, TtyWriter, parse_pane_rows,
    publication_bytes, resolve_wezterm_executable, tty_path_from_fd,
};

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let path = PathBuf::from("/tmp").join(format!("wa-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&path).expect("create scratch directory");
        Self { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct FixedClock(&'static str);

impl Clock for FixedClock {
    fn monotonic_ns20(&self) -> wezterm_attention::protocol::Result<String> {
        Ok(self.0.to_owned())
    }

    fn unix_ns20(&self) -> wezterm_attention::protocol::Result<String> {
        Ok("00000000001000000000".to_owned())
    }
}

struct BlockingClock<'a> {
    entered: &'a Barrier,
    release: &'a Barrier,
}

impl Clock for BlockingClock<'_> {
    fn monotonic_ns20(&self) -> wezterm_attention::protocol::Result<String> {
        self.entered.wait();
        self.release.wait();
        Ok("00000000000000000150".to_owned())
    }

    fn unix_ns20(&self) -> wezterm_attention::protocol::Result<String> {
        Ok("00000000001000000000".to_owned())
    }
}

#[derive(Default)]
struct FakePanes(Vec<PaneRow>);

impl PaneLister for FakePanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        Ok(self.0.clone())
    }
}

struct CountingPanes {
    calls: AtomicUsize,
}

impl PaneLister for CountingPanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![PaneRow {
            pane_id: "42".to_owned(),
            tty_name: Some("/dev/ttys999".to_owned()),
        }])
    }
}

struct FakeTty {
    path: String,
    fingerprint: String,
    writes: Mutex<Vec<(String, Vec<u8>, String)>>,
}

impl FakeTty {
    fn new() -> Self {
        Self {
            path: "/dev/ttys999".to_owned(),
            fingerprint: "f".repeat(64),
            writes: Mutex::new(Vec::new()),
        }
    }
}

impl TtyWriter for FakeTty {
    fn current_path(&self) -> wezterm_attention::protocol::Result<String> {
        Ok(self.path.clone())
    }

    fn fingerprint(&self, _path: &str) -> wezterm_attention::protocol::Result<String> {
        Ok(self.fingerprint.clone())
    }

    fn write(
        &self,
        path: &str,
        data: &[u8],
        expected_fingerprint: &str,
    ) -> wezterm_attention::protocol::Result<()> {
        self.writes.lock().expect("writes lock").push((
            path.to_owned(),
            data.to_vec(),
            expected_fingerprint.to_owned(),
        ));
        Ok(())
    }
}

struct FailingWriteTty(FakeTty);

impl TtyWriter for FailingWriteTty {
    fn current_path(&self) -> wezterm_attention::protocol::Result<String> {
        self.0.current_path()
    }

    fn fingerprint(&self, path: &str) -> wezterm_attention::protocol::Result<String> {
        self.0.fingerprint(path)
    }

    fn write(
        &self,
        _path: &str,
        _data: &[u8],
        _expected_fingerprint: &str,
    ) -> wezterm_attention::protocol::Result<()> {
        Err(wezterm_attention::protocol::AttentionError::new(
            "unsafe_tty",
            "test publication failure",
        ))
    }
}

struct RebirthTty {
    socket_path: PathBuf,
    replacement: Mutex<Option<UnixListener>>,
}

impl TtyWriter for RebirthTty {
    fn current_path(&self) -> wezterm_attention::protocol::Result<String> {
        Ok("/dev/ttys999".to_owned())
    }

    fn fingerprint(&self, _path: &str) -> wezterm_attention::protocol::Result<String> {
        fs::remove_file(&self.socket_path).expect("remove old socket name");
        let listener = UnixListener::bind(&self.socket_path).expect("bind replacement socket");
        *self.replacement.lock().expect("replacement lock") = Some(listener);
        Ok("e".repeat(64))
    }

    fn write(
        &self,
        _path: &str,
        _data: &[u8],
        _expected_fingerprint: &str,
    ) -> wezterm_attention::protocol::Result<()> {
        panic!("incarnation change must be rejected before tty publication")
    }
}

fn setup() -> (Scratch, UnixListener, BTreeMap<String, String>) {
    let scratch = Scratch::new();
    let socket_path = scratch.path.join("mux.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind disposable mux socket");
    let state = scratch.path.join("state");
    let environment = BTreeMap::from([
        (
            "HOME".to_owned(),
            scratch.path.to_string_lossy().into_owned(),
        ),
        (
            "WEZTERM_ATTENTION_DIR".to_owned(),
            state.to_string_lossy().into_owned(),
        ),
        (
            "WEZTERM_UNIX_SOCKET".to_owned(),
            socket_path.to_string_lossy().into_owned(),
        ),
        ("WEZTERM_PANE".to_owned(), "42".to_owned()),
        (
            "WEZTERM_ATTENTION_LAUNCH_ID".to_owned(),
            "00000000-0000-4000-8000-000000000101".to_owned(),
        ),
    ]);
    (scratch, listener, environment)
}

fn ports<'a>(
    clock: &'a dyn Clock,
    tty: &'a dyn TtyWriter,
    panes: &'a dyn PaneLister,
) -> RuntimePorts<'a> {
    RuntimePorts { clock, tty, panes }
}

fn fill_tty_output_queue(fd: libc::c_int) -> usize {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert_ne!(flags, -1);
    assert_ne!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        -1
    );
    let block = [b'x'; 1024];
    let mut filled = 0_usize;
    loop {
        let written = unsafe { libc::write(fd, block.as_ptr().cast(), block.len()) };
        if written > 0 {
            filled += written as usize;
            assert!(filled <= 1024 * 1024, "pty output queue did not fill");
            continue;
        }
        if written == 0 {
            return filled;
        }
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock
        );
        return filled;
    }
}

#[test]
fn embedded_manifest_equals_protocol_file_byte_for_byte() {
    let disk = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("protocol/v2.json"))
        .expect("read protocol manifest");
    assert_eq!(EMBEDDED_MANIFEST.as_bytes(), disk.as_bytes());
}

#[test]
fn durable_record_writes_use_posix_fsync_instead_of_apple_fullfsync() {
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/records.rs"))
        .expect("read record implementation");
    assert!(source.contains("libc::fsync"));
    assert!(!source.contains(".sync_all()"));
}

#[test]
fn manifest_rejects_unknown_field_type_at_load() {
    let mut value: Value = serde_json::from_str(EMBEDDED_MANIFEST).expect("manifest JSON");
    value["records"]["claim"]["types"]["kind"] = json!("unknown_type");
    let error = parse_manifest(&serde_json::to_string(&value).expect("manifest serialization"))
        .expect_err("unknown field type must fail at manifest load");
    assert_eq!(error.diagnostic.code, "integration_version_mismatch");
}

fn assign_path(value: &mut Value, dotted: &str, replacement: Value) {
    let mut current = value;
    let parts: Vec<_> = dotted.split('.').collect();
    for part in &parts[..parts.len() - 1] {
        if !current.get(*part).is_some_and(Value::is_object) {
            current[*part] = json!({});
        }
        current = &mut current[*part];
    }
    current[parts[parts.len() - 1]] = replacement;
}

fn remove_path(value: &mut Value, dotted: &str) {
    let mut current = value;
    let parts: Vec<_> = dotted.split('.').collect();
    for part in &parts[..parts.len() - 1] {
        let Some(next) = current.get_mut(*part) else {
            return;
        };
        current = next;
    }
    if let Some(object) = current.as_object_mut() {
        object.remove(parts[parts.len() - 1]);
    }
}

fn case_value(case: &Value, fixture: &Value) -> Value {
    let mut value = if let Some(raw) = case.get("raw").and_then(Value::as_str) {
        serde_json::from_str(raw).expect("raw fixture JSON")
    } else if let Some(value) = case.get("value") {
        value.clone()
    } else if case["parser"] == "wire" {
        fixture["wire_sample"].clone()
    } else {
        fixture["record_samples"][case["sample"].as_str().expect("sample name")].clone()
    };
    if let Some(patches) = case.get("patch").and_then(Value::as_object) {
        for (path, replacement) in patches {
            assign_path(&mut value, path, replacement.clone());
        }
    }
    if let Some(patches) = case.get("repeat_patch").and_then(Value::as_object) {
        for (path, repeated) in patches {
            let prefix = repeated.get("prefix").and_then(Value::as_str).unwrap_or("");
            let text = repeated["text"].as_str().expect("repeat text");
            let count = repeated["count"].as_u64().expect("repeat count") as usize;
            assign_path(
                &mut value,
                path,
                json!(format!("{prefix}{}", text.repeat(count))),
            );
        }
    }
    if let Some(paths) = case.get("remove").and_then(Value::as_array) {
        for path in paths {
            remove_path(&mut value, path.as_str().expect("remove path"));
        }
    }
    value
}

#[test]
fn shared_protocol_rows_match_the_independent_checker_verdicts() {
    let fixture: Value = serde_json::from_str(include_str!("../fixtures/v2/protocol-cases.json"))
        .expect("protocol fixture JSON");
    let protocol = manifest().expect("embedded manifest");
    for case in fixture["parse_cases"].as_array().expect("parse cases") {
        let value = case_value(case, &fixture);
        let actual = if matches!(case["parser"].as_str(), Some("wire" | "wire_json")) {
            parse_wire_value(&value, protocol)
        } else {
            parse_record_value(&value, protocol)
        };
        let expected = case["expected"].as_str().expect("expected verdict");
        assert_eq!(actual.as_str(), expected, "case {}", case["id"]);
    }
    for case in fixture["eligibility_cases"]
        .as_array()
        .expect("eligibility cases")
    {
        let mut presence = fixture["record_samples"]["subagent_presence"].clone();
        for field in ["written_at_unix_ns", "observed_mono_ns", "status"] {
            if let Some(value) = case.get(field) {
                presence[field] = value.clone();
            }
        }
        let clear = match case.get("clear_mono_ns") {
            Some(Value::Bool(false)) => None,
            Some(value) => value.as_str(),
            None => fixture["record_samples"]["subagent_clear"]["observed_mono_ns"].as_str(),
        };
        let floor = match case.get("floor_mono_ns") {
            Some(Value::Bool(false)) => None,
            Some(value) => value.as_str(),
            None => fixture["record_samples"]["subagent_retention_floor"]["floor_mono_ns"].as_str(),
        };
        let (eligible, diagnostic) = eligible_subagent_presence(
            &presence,
            clear,
            floor,
            case.get("now_unix_ns").and_then(Value::as_str),
            protocol,
        );
        assert_eq!(
            eligible,
            case["expected"].as_bool().expect("eligibility expected"),
            "case {}",
            case["id"]
        );
        assert_eq!(
            diagnostic,
            case.get("diagnostic").and_then(Value::as_str),
            "case {} diagnostic",
            case["id"]
        );
    }
    let scratch = Scratch::new();
    for entry in fixture["state_case"]["files"]
        .as_array()
        .expect("state files")
    {
        let relative = entry["path"].as_str().expect("state path");
        let sample = entry["sample"].as_str().expect("state sample");
        let value = &fixture["record_samples"][sample];
        let identity = RecordIdentity::from_state_path(
            &scratch.path,
            &scratch.path.join(relative),
            value["kind"].as_str().expect("record kind"),
        )
        .expect("fixture path shape");
        identity
            .validate(value)
            .expect("fixture identity matches path");
    }
    let status = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v2/check.py"))
        .status()
        .expect("run independent checker");
    assert!(status.success());
}

#[test]
fn identity_digest_recipes_match_python_known_answers() {
    let realm = wezterm_attention::protocol::sha256_hex(b"/tmp/attention-v2/socket");
    assert_eq!(
        realm,
        "d1fcc834e451eb07ca08dbc16dce35932ba7a6e2477dd9f4bed2f797a4beea5e"
    );
    assert_eq!(
        length_prefixed_digest([realm.as_str(), "16777234", "1234567", "1788401900000000000"]),
        "5fc8f0a15e059c122d08065433bf566c5d15ab9c9b11c3781b6395d359c24fa5"
    );
    assert_eq!(
        length_prefixed_digest(["16777234", "1234567", "34816"]),
        "25280626fae1c4f4948c0257f770a10ddfcef8571204ace40886b70090cfe1b5"
    );
    assert_eq!(
        wezterm_attention::lifecycle::binding_id(
            "claude",
            "session-42",
            "00000000-0000-4000-8000-000000000001"
        ),
        "db88df84885f12868b6bb1ee44b9886b085fd5d526a8ad7e4e99ba58903cf4c9"
    );
}

#[test]
fn pane_enumeration_accepts_numeric_ids_and_rejects_malformed_rows() {
    let rows = parse_pane_rows(br#"[{"pane_id":42,"tty_name":"/dev/ttys042"}]"#)
        .expect("numeric pane id is valid");
    assert_eq!(rows[0].pane_id, "42");
    let error = parse_pane_rows(br#"[{"pane_id":42,"tty_name":7}]"#)
        .expect_err("malformed tty_name must make the probe unavailable");
    assert_eq!(error.diagnostic.code, "record_invalid");
}

#[test]
fn pane_enumeration_keeps_rows_without_a_tty_for_per_row_skipping() {
    let rows = parse_pane_rows(
        br#"[{"pane_id":42,"tty_name":"/dev/ttys042"},{"pane_id":43,"tty_name":null}]"#,
    )
    .expect("a documented null tty must not reject the realm");
    assert_eq!(rows.len(), 2);
}

#[test]
fn realm_publish_skips_only_the_row_without_a_tty() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let no_panes = FakePanes::default();
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &no_panes))
        .expect("claim succeeds");
    let panes = FakePanes(vec![
        PaneRow {
            pane_id: "42".to_owned(),
            tty_name: Some(tty.path.clone()),
        },
        PaneRow {
            pane_id: "43".to_owned(),
            tty_name: None,
        },
    ]);
    let report = wezterm_attention::publish_realm(
        &environment["WEZTERM_UNIX_SOCKET"],
        &environment,
        &ports(&clock, &tty, &panes),
    )
    .expect("realm publish returns a report");
    assert_eq!(report.attempted, 2);
    assert_eq!(report.published, 1);
    assert_eq!(report.v2_published, 1);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.diagnostics[0].code, "unsafe_tty");
}

#[test]
fn executable_resolution_uses_the_explicit_fallback_when_path_is_empty() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("fallback-{}", Uuid::new_v4().simple()));
    fs::create_dir_all(&directory).expect("create trusted fallback directory");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .expect("secure fallback directory");
    let executable = directory.join("wezterm");
    fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("write fake executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("make fake executable executable");
    let resolved = resolve_wezterm_executable(None, None, std::slice::from_ref(&executable))
        .expect("fallback resolves");
    assert_eq!(resolved, executable);
    fs::remove_dir_all(directory).expect("remove fallback directory");
}

#[test]
fn executable_resolution_rejects_a_fallback_below_a_group_writable_directory() {
    let scratch = Scratch::new();
    let executable = scratch.path.join("wezterm");
    fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("write fake executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("make fake executable executable");
    let error = resolve_wezterm_executable(None, None, &[executable])
        .expect_err("a fallback below /tmp must not be trusted");
    assert_eq!(error.diagnostic.code, "realm_unavailable");
}

#[test]
fn claim_is_private_durable_and_published() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let panes = FakePanes::default();
    let result = wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &panes))
        .expect("claim succeeds");
    let (address, _) = pane_address(&environment).expect("pane address");
    let claim =
        pane_path(&state_root(&environment).expect("state root"), &address).join("claim.json");
    let claim_record: Value =
        serde_json::from_slice(&fs::read(&claim).expect("claim record")).expect("claim JSON");
    assert_eq!(result.disposition, "applied");
    assert_eq!(result.publication, "published");
    assert_eq!(result.launch_id, environment["WEZTERM_ATTENTION_LAUNCH_ID"]);
    assert_eq!(
        claim_record["address"],
        serde_json::to_value(&address).expect("address JSON")
    );
    assert_eq!(
        claim_record["launch_id"],
        environment["WEZTERM_ATTENTION_LAUNCH_ID"]
    );
    let root = state_root(&environment).expect("state root");
    let realm: Value = serde_json::from_slice(
        &fs::read(
            root.join("v2/realms")
                .join(&address.realm_id)
                .join("realm.json"),
        )
        .expect("realm record"),
    )
    .expect("realm JSON");
    let incarnation: Value = serde_json::from_slice(
        &fs::read(
            root.join("v2/realms")
                .join(&address.realm_id)
                .join("incarnations")
                .join(&address.incarnation_id)
                .join("incarnation.json"),
        )
        .expect("incarnation record"),
    )
    .expect("incarnation JSON");
    assert_eq!(realm["realm_id"], address.realm_id);
    assert_eq!(incarnation["incarnation_id"], address.incarnation_id);
    assert_eq!(
        fs::metadata(&claim)
            .expect("claim metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(claim.parent().expect("pane directory").join("reviews"))
            .expect("reviews metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let writes = tty.writes.lock().expect("writes lock");
    assert_eq!(writes.len(), 1);
    assert!(
        writes[0]
            .1
            .windows(b"WEZTERM_PANE".len())
            .any(|window| window == b"WEZTERM_PANE")
    );
    assert!(
        writes[0]
            .1
            .windows(b"WEZTERM_ATTENTION".len())
            .any(|window| window == b"WEZTERM_ATTENTION")
    );
}

#[test]
fn claim_mints_a_launch_id_when_the_shell_has_none() {
    let (_scratch, _listener, mut environment) = setup();
    environment.remove("WEZTERM_ATTENTION_LAUNCH_ID");
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let panes = FakePanes::default();
    let result = wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &panes))
        .expect("claim succeeds");
    Uuid::parse_str(&result.launch_id).expect("Rust minted a UUID");
    assert_eq!(result.publication, "published");
}

#[test]
fn committed_claim_reports_pending_when_tty_publication_fails() {
    let (_scratch, _listener, mut environment) = setup();
    environment.remove("WEZTERM_ATTENTION_LAUNCH_ID");
    let clock = FixedClock("00000000000000000100");
    let tty = FailingWriteTty(FakeTty::new());
    let panes = FakePanes::default();
    let result = wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &panes))
        .expect("durable claim is still successful");
    assert_eq!(result.publication, "pending");
    assert_eq!(
        result
            .publication_diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("unsafe_tty")
    );
    let (address, _) = pane_address(&environment).expect("pane address");
    let claim_path =
        pane_path(&state_root(&environment).expect("state root"), &address).join("claim.json");
    let claim: Value = serde_json::from_slice(&fs::read(claim_path).expect("claim persisted"))
        .expect("claim JSON");
    assert_eq!(claim["launch_id"], result.launch_id);
}

#[test]
fn duplicate_claim_republishes_without_rewrite() {
    let (_scratch, _listener, environment) = setup();
    let tty = FakeTty::new();
    let panes = FakePanes::default();
    let first_clock = FixedClock("00000000000000000100");
    wezterm_attention::claim_launch(&environment, &ports(&first_clock, &tty, &panes))
        .expect("first claim");
    let (address, _) = pane_address(&environment).expect("pane address");
    let claim =
        pane_path(&state_root(&environment).expect("state root"), &address).join("claim.json");
    let before = fs::read(&claim).expect("claim bytes");
    let modified = fs::metadata(&claim)
        .expect("claim metadata")
        .modified()
        .expect("modified time");
    let second_clock = FixedClock("00000000000000000200");
    let result = wezterm_attention::claim_launch(&environment, &ports(&second_clock, &tty, &panes))
        .expect("duplicate claim");
    assert_eq!(result.disposition, "confirmed");
    assert_eq!(fs::read(&claim).expect("claim bytes"), before);
    assert_eq!(
        fs::metadata(&claim)
            .expect("claim metadata")
            .modified()
            .expect("modified time"),
        modified
    );
    assert_eq!(tty.writes.lock().expect("writes lock").len(), 2);
}

#[test]
fn delayed_older_claim_loses() {
    let (_scratch, _listener, mut older_environment) = setup();
    let mut newer_environment = older_environment.clone();
    older_environment.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".to_owned(),
        "00000000-0000-4000-8000-000000000102".to_owned(),
    );
    newer_environment.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".to_owned(),
        "00000000-0000-4000-8000-000000000103".to_owned(),
    );
    let tty = FakeTty::new();
    let panes = FakePanes::default();
    let entered = Barrier::new(2);
    let release = Barrier::new(2);
    let older_clock = BlockingClock {
        entered: &entered,
        release: &release,
    };
    let result = thread::scope(|scope| {
        let older = scope.spawn(|| {
            wezterm_attention::claim_launch(&older_environment, &ports(&older_clock, &tty, &panes))
                .expect("older claim")
        });
        entered.wait();
        let newer_clock = FixedClock("00000000000000000200");
        wezterm_attention::claim_launch(&newer_environment, &ports(&newer_clock, &tty, &panes))
            .expect("newer claim");
        release.wait();
        older.join().expect("older claimant")
    });
    assert_eq!(result.disposition, "ignored");
    assert_eq!(
        result.launch_id,
        newer_environment["WEZTERM_ATTENTION_LAUNCH_ID"]
    );
    let (address, _) = pane_address(&newer_environment).expect("pane address");
    let claim_path = pane_path(
        &state_root(&newer_environment).expect("state root"),
        &address,
    )
    .join("claim.json");
    let claim: Value =
        serde_json::from_slice(&fs::read(claim_path).expect("claim")).expect("claim JSON");
    assert_eq!(
        claim["launch_id"],
        newer_environment["WEZTERM_ATTENTION_LAUNCH_ID"]
    );
}

#[test]
fn equal_pane_numbers_in_two_sockets_have_distinct_addresses() {
    let (first_scratch, _first_listener, first_environment) = setup();
    let second_socket = first_scratch.path.join("second.sock");
    let _second_listener = UnixListener::bind(&second_socket).expect("bind second socket");
    let mut second_environment = first_environment.clone();
    second_environment.insert(
        "WEZTERM_UNIX_SOCKET".to_owned(),
        second_socket.to_string_lossy().into_owned(),
    );
    let (first, _) = pane_address(&first_environment).expect("first address");
    let (second, _) = pane_address(&second_environment).expect("second address");
    assert_eq!(first.pane_id, second.pane_id);
    assert_ne!(first.realm_id, second.realm_id);
    assert_ne!(first.incarnation_id, second.incarnation_id);
}

#[test]
fn socket_rebirth_before_commit_is_rejected_without_a_claim_write() {
    let (scratch, _listener, environment) = setup();
    let (old_address, _) = pane_address(&environment).expect("old address");
    let tty = RebirthTty {
        socket_path: scratch.path.join("mux.sock"),
        replacement: Mutex::new(None),
    };
    let clock = FixedClock("00000000000000000100");
    let panes = FakePanes::default();
    let error = wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &panes))
        .expect_err("socket rebirth must reject claim");
    assert_eq!(error.diagnostic.code, "incarnation_changed");
    let old_claim =
        pane_path(&state_root(&environment).expect("state root"), &old_address).join("claim.json");
    assert!(!old_claim.exists());
}

#[test]
fn lock_contention_is_bounded_and_diagnosed() {
    let scratch = Scratch::new();
    let path = Arc::new(scratch.path.join("contended.lock"));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let thread_path = Arc::clone(&path);
    let thread_entered = Arc::clone(&entered);
    let thread_release = Arc::clone(&release);
    let holder = thread::spawn(move || {
        with_lock(&thread_path, Duration::from_secs(1), || {
            thread_entered.wait();
            thread_release.wait();
            Ok(())
        })
        .expect("holder lock");
    });
    entered.wait();
    let error = with_lock(&path, Duration::from_millis(25), || Ok(()))
        .expect_err("second lock must time out");
    assert_eq!(error.diagnostic.code, "probe_unavailable");
    release.wait();
    holder.join().expect("holder thread");
}

#[test]
fn opened_tty_descriptor_is_revalidated() {
    let mut first_master = 0;
    let mut first_slave = 0;
    let mut second_master = 0;
    let mut second_slave = 0;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut first_master,
                &mut first_slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut second_master,
                &mut second_slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let first_path = tty_path_from_fd(first_slave).expect("first tty path");
    let expected =
        wezterm_attention::identity::tty_fingerprint(&first_path).expect("first tty fingerprint");
    let second_file = unsafe { fs::File::from_raw_fd(second_slave) };
    let error = SystemTtyWriter::validate_opened(&second_file, &expected)
        .expect_err("opened second tty must not match first");
    assert_eq!(error.diagnostic.code, "unsafe_tty");
    unsafe {
        libc::close(first_master);
        libc::close(first_slave);
        libc::close(second_master);
    }
}

#[test]
fn real_disposable_pty_receives_osc_output() {
    let mut master = 0;
    let mut slave = 0;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let path = tty_path_from_fd(slave).expect("tty path");
    let writer = SystemTtyWriter;
    let fingerprint = writer.fingerprint(&path).expect("tty fingerprint");
    let address = wezterm_attention::identity::PaneAddress {
        realm_id: "a".repeat(64),
        incarnation_id: "b".repeat(64),
        pane_id: "42".to_owned(),
    };
    let bytes = publication_bytes(&address, Some("00000000-0000-4000-8000-000000000001"))
        .expect("publication bytes");
    writer
        .write(&path, &bytes, &fingerprint)
        .expect("write publication");
    let mut master_file = unsafe { fs::File::from_raw_fd(master) };
    let mut received = vec![0; bytes.len()];
    master_file
        .read_exact(&mut received)
        .expect("read pty output");
    assert_eq!(received, bytes);
    unsafe { libc::close(slave) };
}

#[test]
fn tty_input_guard_records_zero_stdin_bytes_during_a_real_claim() {
    let (scratch, _listener, mut environment) = setup();
    environment.remove("WEZTERM_ATTENTION_LAUNCH_ID");
    let mut master = 0;
    let mut slave = 0;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let guard_result = scratch.path.join("tty-guard.json");
    let guard_path = std::env::var_os("WEZTERM_ATTENTION_TTY_INPUT_GUARD")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/tty_input_guard.py")
        });
    let guard_stdin = unsafe { fs::File::from_raw_fd(libc::dup(slave)) };
    let mut guard = Command::new("python3")
        .arg(guard_path)
        .arg(&guard_result)
        .env_clear()
        .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin")
        .stdin(Stdio::from(guard_stdin))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start tty input guard");
    thread::sleep(Duration::from_millis(200));
    let claim_stdin = unsafe { fs::File::from_raw_fd(libc::dup(slave)) };
    let claim = Command::new(env!("CARGO_BIN_EXE_attention"))
        .args(["hooks", "claim"])
        .env_clear()
        .envs(&environment)
        .stdin(Stdio::from(claim_stdin))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run real claim");
    assert!(
        claim.status.success(),
        "{}",
        String::from_utf8_lossy(&claim.stderr)
    );
    let selected_launch = String::from_utf8(claim.stdout)
        .expect("claim stdout")
        .trim()
        .to_owned();
    Uuid::parse_str(&selected_launch).expect("claim stdout is a UUID");
    let (address, _) = pane_address(&environment).expect("pane address");
    let claim_record: Value = serde_json::from_slice(
        &fs::read(
            pane_path(&state_root(&environment).expect("state root"), &address).join("claim.json"),
        )
        .expect("claim record"),
    )
    .expect("claim JSON");
    assert_eq!(claim_record["launch_id"], selected_launch);
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while !guard_result.exists() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    let result: Value =
        serde_json::from_slice(&fs::read(&guard_result).expect("tty input guard result"))
            .expect("tty input guard JSON");
    assert_eq!(result["bytes"], 0);
    let _ = guard.kill();
    let _ = guard.wait();
    unsafe {
        libc::close(master);
        libc::close(slave);
    }
}

#[test]
fn full_tty_output_queue_returns_a_pending_claim_within_the_deadline() {
    let (_scratch, _listener, environment) = setup();
    let mut master = 0;
    let mut slave = 0;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let filled = fill_tty_output_queue(slave);
    assert!(filled > 0);
    let tty_path = tty_path_from_fd(slave).expect("full tty path");
    let clock = FixedClock("00000000000000000100");
    let writer = SystemTtyWriter;
    let panes = FakePanes::default();
    let started = Instant::now();
    let result = wezterm_attention::claim_launch_at_tty(
        &environment,
        &ports(&clock, &writer, &panes),
        &tty_path,
    )
    .expect("durable claim remains successful when tty publication times out");
    let elapsed = started.elapsed();
    unsafe {
        libc::close(master);
        libc::close(slave);
    }
    assert!(
        elapsed < Duration::from_secs(1),
        "claim exceeded its tty publication deadline: {elapsed:?}"
    );
    assert_eq!(result.publication, "pending");
    assert_eq!(
        result
            .publication_diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("unsafe_tty")
    );
}

#[test]
fn full_tty_output_queue_does_not_block_later_realm_panes() {
    let (_scratch, _listener, environment) = setup();
    let mut first_master = 0;
    let mut first_slave = 0;
    let mut second_master = 0;
    let mut second_slave = 0;
    for (master, slave) in [
        (&mut first_master, &mut first_slave),
        (&mut second_master, &mut second_slave),
    ] {
        assert_eq!(
            unsafe {
                libc::openpty(
                    master,
                    slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
    }
    assert!(fill_tty_output_queue(first_slave) > 0);
    let first_path = tty_path_from_fd(first_slave).expect("first tty path");
    let second_path = tty_path_from_fd(second_slave).expect("second tty path");
    let panes = FakePanes(vec![
        PaneRow {
            pane_id: "42".to_owned(),
            tty_name: Some(first_path),
        },
        PaneRow {
            pane_id: "43".to_owned(),
            tty_name: Some(second_path),
        },
    ]);
    let clock = FixedClock("00000000000000000100");
    let writer = SystemTtyWriter;
    let started = Instant::now();
    let report = wezterm_attention::publish_realm(
        &environment["WEZTERM_UNIX_SOCKET"],
        &environment,
        &ports(&clock, &writer, &panes),
    )
    .expect("realm publication report");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(report.attempted, 2);
    assert_eq!(report.published, 1);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.diagnostics[0].code, "unsafe_tty");
    let mut second_output = unsafe { fs::File::from_raw_fd(second_master) };
    let mut bytes = [0_u8; 4096];
    let count = second_output
        .read(&mut bytes)
        .expect("read second tty output");
    assert!(
        bytes[..count]
            .windows(b"SetUserVar=WEZTERM_PANE".len())
            .any(|window| window == b"SetUserVar=WEZTERM_PANE")
    );
    unsafe {
        libc::close(first_master);
        libc::close(first_slave);
        libc::close(second_slave);
    }
}

#[test]
fn claimed_pane_has_no_bindings_yet() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let panes = FakePanes::default();
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &panes))
        .expect("claim succeeds");
    let (rows, diagnostics) =
        read_bindings(&state_root(&environment).expect("state root")).expect("bindings query");
    assert!(rows.is_empty());
    assert!(diagnostics.is_empty());
}

fn binding_record(
    address: &wezterm_attention::identity::PaneAddress,
    launch_id: &str,
    binding_id: &str,
) -> Value {
    json!({
        "kind": "binding",
        "schema": 2,
        "address": address,
        "launch_id": launch_id,
        "binding_id": binding_id,
        "event_id": "00000000-0000-4000-8000-000000000301",
        "provider": "claude",
        "provider_session_id": "session-42",
        "start_source": "startup",
        "observed_mono_ns": "00000000000000000300",
        "written_at_unix_ns": "00000000001000000000",
        "writer_version": "2.0.0"
    })
}

#[test]
fn bindings_query_returns_all_identity_axes_and_rejects_path_mismatch() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let panes = FakePanes::default();
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &panes))
        .expect("claim succeeds");
    let root = state_root(&environment).expect("state root");
    let (address, _) = pane_address(&environment).expect("pane address");
    let launch_id = &environment["WEZTERM_ATTENTION_LAUNCH_ID"];
    let binding_id = "c".repeat(64);
    let launch = launch_path(&root, &address, launch_id);
    let binding_dir = launch.join("bindings").join(&binding_id);
    atomic_replace(
        &binding_dir.join("binding.json"),
        &binding_record(&address, launch_id, &binding_id),
    )
    .expect("write binding");
    atomic_replace(
        &launch.join("current-binding.json"),
        &json!({
            "kind": "current_binding",
            "schema": 2,
            "address": address,
            "launch_id": launch_id,
            "binding_id": binding_id,
        }),
    )
    .expect("write pointer");
    let wrong_id = "d".repeat(64);
    atomic_replace(
        &launch.join("bindings").join(&wrong_id).join("binding.json"),
        &binding_record(&address, launch_id, &binding_id),
    )
    .expect("write path-mismatched binding");

    let (rows, diagnostics) = read_bindings(&root).expect("bindings query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].address, address);
    assert_eq!(rows[0].launch_id, *launch_id);
    assert_eq!(rows[0].binding_id, binding_id);
    assert_eq!(rows[0].provider_session_id, "session-42");
    assert!(rows[0].current);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "record_invalid")
    );
}

#[test]
fn bindings_query_rejects_foreign_end_pointer_and_claim_records() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let panes = FakePanes::default();
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &panes))
        .expect("claim succeeds");
    let root = state_root(&environment).expect("state root");
    let (address, _) = pane_address(&environment).expect("pane address");
    let launch_id = &environment["WEZTERM_ATTENTION_LAUNCH_ID"];
    let binding_id = "c".repeat(64);
    let launch = launch_path(&root, &address, launch_id);
    let binding_dir = launch.join("bindings").join(&binding_id);
    atomic_replace(
        &binding_dir.join("binding.json"),
        &binding_record(&address, launch_id, &binding_id),
    )
    .expect("write binding");
    atomic_replace(
        &launch.join("current-binding.json"),
        &json!({
            "kind":"current_binding","schema":2,"address":address,
            "launch_id":"00000000-0000-4000-8000-000000000999","binding_id":binding_id
        }),
    )
    .expect("write foreign pointer");
    atomic_replace(
        &binding_dir.join("end.json"),
        &json!({
            "kind":"binding_end","schema":2,"address":address,
            "launch_id":"00000000-0000-4000-8000-000000000999",
            "binding_id":"f".repeat(64),"reason":"session_end",
            "event_id":"00000000-0000-4000-8000-000000000998",
            "observed_mono_ns":"00000000000000000999",
            "written_at_unix_ns":"00000000001000000000"
        }),
    )
    .expect("write foreign end");
    let claim_path = pane_path(&root, &address).join("claim.json");
    let mut claim: Value =
        serde_json::from_slice(&fs::read(&claim_path).expect("claim")).expect("claim JSON");
    claim["address"]["pane_id"] = json!("99");
    atomic_replace(&claim_path, &claim).expect("write foreign claim");

    let (rows, diagnostics) = read_bindings(&root).expect("bindings query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].binding_phase, "active");
    assert!(!rows[0].current);
    assert_eq!(rows[0].binding_health, "invalid");
    assert!(
        diagnostics
            .iter()
            .filter(|item| item.code == "record_invalid")
            .count()
            >= 3
    );
}

#[test]
fn bindings_query_probes_presence_once_per_pane() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let empty_panes = FakePanes::default();
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &empty_panes))
        .expect("claim succeeds");
    let root = state_root(&environment).expect("state root");
    let (address, _) = pane_address(&environment).expect("pane address");
    let launch_id = &environment["WEZTERM_ATTENTION_LAUNCH_ID"];
    let launch = launch_path(&root, &address, launch_id);
    for binding_id in ["c".repeat(64), "d".repeat(64)] {
        atomic_replace(
            &launch
                .join("bindings")
                .join(&binding_id)
                .join("binding.json"),
            &binding_record(&address, launch_id, &binding_id),
        )
        .expect("write binding");
    }
    let panes = CountingPanes {
        calls: AtomicUsize::new(0),
    };
    let (rows, _) = read_bindings_with_ports(&root, Some(&panes), None).expect("bindings query");
    assert_eq!(rows.len(), 2);
    assert_eq!(panes.calls.load(Ordering::SeqCst), 1);
    assert!(rows.iter().all(|row| row.pane_presence == "present"));
}

#[test]
fn missing_incarnation_manifest_fails_presence_closed() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let empty_panes = FakePanes::default();
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &empty_panes))
        .expect("claim succeeds");
    let root = state_root(&environment).expect("state root");
    let (address, _) = pane_address(&environment).expect("pane address");
    let launch_id = &environment["WEZTERM_ATTENTION_LAUNCH_ID"];
    let binding_id = "c".repeat(64);
    let launch = launch_path(&root, &address, launch_id);
    atomic_replace(
        &launch
            .join("bindings")
            .join(&binding_id)
            .join("binding.json"),
        &binding_record(&address, launch_id, &binding_id),
    )
    .expect("write binding");
    atomic_replace(
        &launch.join("current-binding.json"),
        &json!({"kind":"current_binding","schema":2,"address":address,"launch_id":launch_id,"binding_id":binding_id}),
    )
    .expect("write pointer");
    fs::remove_file(
        root.join("v2/realms")
            .join(&address.realm_id)
            .join("incarnations")
            .join(&address.incarnation_id)
            .join("incarnation.json"),
    )
    .expect("remove incarnation manifest");
    let panes = CountingPanes {
        calls: AtomicUsize::new(0),
    };
    let (rows, _) = read_bindings_with_ports(&root, Some(&panes), None).expect("bindings query");
    assert_eq!(rows[0].pane_presence, "unavailable");
    assert_eq!(rows[0].reader_confidence, "unconfirmed");
    assert_eq!(panes.calls.load(Ordering::SeqCst), 0);
}
