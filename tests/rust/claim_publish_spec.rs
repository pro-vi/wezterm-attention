#[path = "support/executables.rs"]
mod executables;
#[path = "support/trusted_scratch.rs"]
mod trusted_scratch;

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
use trusted_scratch::TrustedScratch;
use uuid::Uuid;
use wezterm_attention::identity::{length_prefixed_digest, pane_address};
use wezterm_attention::protocol::{
    EMBEDDED_MANIFEST, eligible_subagent_presence, manifest, parse_manifest, parse_record_value,
    parse_wire_value,
};
use wezterm_attention::query::{read_bindings, read_bindings_with_ports};
use wezterm_attention::records::{
    RecordIdentity, RecordRead, atomic_replace, launch_path, pane_path, read_record_typed,
    state_root, with_lock,
};
use wezterm_attention::wezterm::{
    Clock, PaneLister, PaneRow, Presence, ProcessListing, ProcessProbe, RuntimePorts,
    SystemProcessInspector, SystemProcessProbe, SystemTtyWriter, TtyWriter, parse_pane_rows,
    publication_bytes, resolve_wezterm_executable, tty_path_from_fd,
};

struct Scratch {
    path: PathBuf,
}

#[test]
fn c8_socket_rebirth_inside_realm_publication_is_rejected() {
    let (scratch, _listener, environment) = setup();
    let tty = RebirthTty {
        socket_path: scratch.path.join("mux.sock"),
        replacement: Mutex::new(None),
    };
    let clock = FixedClock("00000000000000000100");
    let panes = FakePanes(vec![PaneRow {
        pane_id: "42".into(),
        tty_name: Some("/dev/ttys999".into()),
    }]);
    let report = wezterm_attention::publish_realm(
        &environment["WEZTERM_UNIX_SOCKET"],
        &environment,
        &ports(&clock, &tty, &panes),
    )
    .unwrap();
    assert_eq!(report.published, 0);
    assert_eq!(report.diagnostics[0].code, "incarnation_changed");
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

/// The mux's view of the pane `setup` claims from: pane 42 on `FakeTty`'s tty.
fn this_pane() -> FakePanes {
    FakePanes(vec![PaneRow {
        pane_id: "42".into(),
        tty_name: Some(FakeTty::new().path),
    }])
}

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
    RuntimePorts {
        clock,
        tty,
        panes,
        processes: &SystemProcessInspector,
    }
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
fn an_unreadable_record_is_diagnosed_as_a_failed_probe_not_as_invalid_bytes() {
    // `record_invalid` is a claim about bytes that were read and did not parse.
    // A record the process cannot open has had no bytes read at all, so telling a
    // consumer it is invalid points the repair at the wrong thing: the file is
    // fine and the permissions are not. `RecordRead` already keeps the two apart;
    // the diagnostic travelling with it has to agree.
    let scratch = Scratch::new();
    let path = scratch.path.join("claim.json");
    fs::write(&path, b"{}").expect("write record");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("make unreadable");
    let read = read_record_typed(&path, Some("claim"), &RecordIdentity::unscoped());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore permissions");
    let RecordRead::Unavailable(error) = read else {
        panic!("an unopenable record must read as unavailable");
    };
    assert_eq!(error.diagnostic.code, "probe_unavailable");
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
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
    let scratch = TrustedScratch::new();
    let executable = scratch.executable("wezterm", "exit 0");
    let resolved = resolve_wezterm_executable(None, None, None, std::slice::from_ref(&executable))
        .expect("fallback resolves");
    assert_eq!(resolved, executable);
}

#[test]
fn executable_resolution_rejects_a_fallback_below_a_group_writable_directory() {
    let scratch = TrustedScratch::new();
    let shared = scratch.0.join("shared");
    fs::create_dir(&shared).expect("create shared directory");
    let executable = trusted_scratch::write_script(&shared.join("wezterm"), "exit 0");
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o700)).expect("private directory");
    assert!(
        resolve_wezterm_executable(None, None, None, std::slice::from_ref(&executable)).is_ok(),
        "the same file in a private directory resolves, so the refusal below is the mode's"
    );
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o770)).expect("group-writable");
    let error = resolve_wezterm_executable(None, None, None, &[executable])
        .expect_err("a fallback below a group-writable directory must not be trusted");
    assert_eq!(error.diagnostic.code, "realm_unavailable");
}

#[test]
fn the_cli_beside_a_running_mux_server_is_used_and_the_server_itself_never_is() {
    let scratch = TrustedScratch::new();
    let server = scratch.executable("wezterm-mux-server", "exit 0");
    let cli = scratch.executable("wezterm", "exit 0");
    let empty_path = Scratch::new();
    let path = empty_path.path.as_os_str();

    let beside_executable =
        resolve_wezterm_executable(Some(path), None, Some(server.as_os_str()), &[])
            .expect("the CLI beside WEZTERM_EXECUTABLE resolves");
    assert_eq!(beside_executable, cli);
    let in_executable_dir =
        resolve_wezterm_executable(Some(path), Some(scratch.0.as_os_str()), None, &[])
            .expect("the CLI in WEZTERM_EXECUTABLE_DIR resolves");
    assert_eq!(in_executable_dir, cli);

    fs::remove_file(&cli).expect("remove the CLI");
    let error = resolve_wezterm_executable(
        Some(path),
        Some(scratch.0.as_os_str()),
        Some(server.as_os_str()),
        std::slice::from_ref(&server),
    )
    .expect_err("a mux server is never run in place of the CLI");
    assert_eq!(error.diagnostic.code, "realm_unavailable");
}

#[test]
fn executable_resolution_skips_a_relative_path_entry() {
    let scratch = TrustedScratch::new();
    let cli = scratch.executable("wezterm", "exit 0");
    let here = fs::canonicalize(std::env::current_dir().expect("current directory"))
        .expect("canonical current directory");
    let mut relative = PathBuf::new();
    for _ in here.components().skip(1) {
        relative.push("..");
    }
    relative.push(scratch.0.strip_prefix("/").expect("absolute scratch"));
    assert!(
        relative.is_relative() && relative.join("wezterm").exists(),
        "the relative entry names the directory that holds a CLI"
    );
    let error = resolve_wezterm_executable(Some(relative.as_os_str()), None, None, &[])
        .expect_err("a relative PATH entry is skipped");
    assert_eq!(error.diagnostic.code, "realm_unavailable");
    let absolute = resolve_wezterm_executable(Some(scratch.0.as_os_str()), None, None, &[])
        .expect("the same directory as an absolute entry resolves");
    assert_eq!(absolute, cli);
}

#[test]
fn claim_is_private_durable_and_published() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let result = wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
    let result = wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
    let result = wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
    let first_clock = FixedClock("00000000000000000100");
    wezterm_attention::claim_launch(&environment, &ports(&first_clock, &tty, &this_pane()))
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
    let result =
        wezterm_attention::claim_launch(&environment, &ports(&second_clock, &tty, &this_pane()))
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
    let entered = Barrier::new(2);
    let release = Barrier::new(2);
    let older_clock = BlockingClock {
        entered: &entered,
        release: &release,
    };
    let result = thread::scope(|scope| {
        let older = scope.spawn(|| {
            wezterm_attention::claim_launch(
                &older_environment,
                &ports(&older_clock, &tty, &this_pane()),
            )
            .expect("older claim")
        });
        entered.wait();
        let newer_clock = FixedClock("00000000000000000200");
        wezterm_attention::claim_launch(
            &newer_environment,
            &ports(&newer_clock, &tty, &this_pane()),
        )
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
    let error = wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/python/tty_input_guard.py")
        });
    let guard_stdin = unsafe { fs::File::from_raw_fd(libc::dup(slave)) };
    let python = executables::resolve("python3");
    let mut guard = Command::new(&python)
        .arg(guard_path)
        .arg(&guard_result)
        .env_clear()
        .env("PATH", executables::child_path(&[], &[&python]))
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
    // The guard's `write_text` creates the file before it holds any bytes, so
    // waiting for the path to exist hands back an empty file whenever the machine
    // is loaded enough to interleave there. Wait for content that parses, which is
    // what the assertion below actually needs.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let result: Value = loop {
        if let Ok(bytes) = fs::read(&guard_result)
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        {
            break value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tty input guard wrote no parseable result before the deadline"
        );
        thread::sleep(Duration::from_millis(50));
    };
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
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
        "schema": 3,
        "address": address,
        "launch_id": launch_id,
        "binding_id": binding_id,
        "event_id": "00000000-0000-4000-8000-000000000301",
        "provider": "claude",
        "provider_session_id": "session-42",
        "start_source": "startup",
        "observed_mono_ns": "00000000000000000300",
        "written_at_unix_ns": "00000000001000000000",
        "writer_version": "1.0.0"
    })
}

#[test]
fn bindings_query_returns_all_identity_axes_and_rejects_path_mismatch() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
            "schema": 3,
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
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
            "kind":"current_binding","schema":3,"address":address,
            "launch_id":"00000000-0000-4000-8000-000000000999","binding_id":binding_id
        }),
    )
    .expect("write foreign pointer");
    atomic_replace(
        &binding_dir.join("end.json"),
        &json!({
            "kind":"binding_end","schema":3,"address":address,
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
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
        &json!({"kind":"current_binding","schema":3,"address":address,"launch_id":launch_id,"binding_id":binding_id}),
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

/// A child that lives until killed, with exactly `environment`.
///
/// It is this crate's own binary waiting on stdin for a hook payload. A
/// system shell would not do: macOS withholds the environment of its own
/// platform binaries from every reader, `ps` included.
struct Waiting(std::process::Child);

impl Waiting {
    fn spawn(provider: &str, environment: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_attention"));
        command
            .args(["hooks", "event", provider, "Stop"])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (name, value) in environment {
            command.env(name, value);
        }
        Self(command.spawn().expect("spawn waiting child"))
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Ask until `expected` comes back or two seconds pass, since a child just
/// spawned may not have reached its own program yet.
fn settled_presence(socket: &str, pane: &str, expected: Presence) -> Presence {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let presence = SystemProcessProbe.presence(socket, pane);
        if presence == expected || Instant::now() >= deadline {
            return presence;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Not seen by a listing that was read. On macOS that is `Unseen`, because
/// some of this user's system processes always hide their environment;
/// where every process can be read it is `Absent`.
fn assert_not_seen(presence: Presence) {
    assert!(
        matches!(presence, Presence::Absent | Presence::Unseen),
        "{presence:?}"
    );
}

#[test]
fn the_process_probe_finds_a_pane_in_a_live_process_environment() {
    let socket = format!("/tmp/wa-probe-{}.sock", Uuid::new_v4().simple());
    assert_not_seen(SystemProcessProbe.presence(&socket, "4242"));
    let waiting = Waiting::spawn(
        "claude",
        &[("WEZTERM_UNIX_SOCKET", &socket), ("WEZTERM_PANE", "4242")],
    );
    assert_eq!(
        settled_presence(&socket, "4242", Presence::Present),
        Presence::Present
    );
    assert_not_seen(SystemProcessProbe.presence(&socket, "424"));
    drop(waiting);
    assert_not_seen(SystemProcessProbe.presence(&socket, "4242"));
}

#[test]
fn pane_variables_in_a_process_arguments_are_not_its_environment() {
    let socket = format!("/tmp/wa-probe-{}.sock", Uuid::new_v4().simple());
    let text = format!("WEZTERM_PANE=4343 WEZTERM_UNIX_SOCKET={socket}");
    let _waiting = Waiting::spawn(
        &text,
        &[("WEZTERM_UNIX_SOCKET", &socket), ("WEZTERM_PANE", "4444")],
    );
    assert_eq!(
        settled_presence(&socket, "4444", Presence::Present),
        Presence::Present,
        "the listing read this process"
    );
    assert_not_seen(SystemProcessProbe.presence(&socket, "4343"));
}

/// A process carries its socket as WezTerm was configured to spell it, which
/// may pass through a symlinked directory; a realm record carries it resolved.
/// Both name the same socket, including once the socket file itself is gone.
#[test]
fn the_process_probe_matches_a_socket_spelled_through_a_symlinked_directory() {
    let base = PathBuf::from("/tmp").join(format!("wa-probe-{}", Uuid::new_v4().simple()));
    fs::create_dir_all(base.join("real")).expect("socket directory");
    std::os::unix::fs::symlink(base.join("real"), base.join("link")).expect("link");
    let spelled = base.join("link/mux.sock");
    let spelled = spelled.to_str().expect("UTF-8 path");
    let resolved = fs::canonicalize(base.join("real"))
        .expect("resolve")
        .join("mux.sock");
    let _waiting = Waiting::spawn(
        "claude",
        &[("WEZTERM_UNIX_SOCKET", spelled), ("WEZTERM_PANE", "4646")],
    );
    assert_eq!(
        settled_presence(spelled, "4646", Presence::Present),
        Presence::Present
    );
    assert_eq!(
        SystemProcessProbe.presence(resolved.to_str().expect("UTF-8 path"), "4646"),
        Presence::Present
    );
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn the_process_probe_is_available_exactly_when_its_listing_is() {
    let probe = SystemProcessProbe;
    assert!(matches!(probe.pane_processes(), ProcessListing::Listed(_)));
    assert!(probe.available());
}

struct UnavailablePanes;

impl PaneLister for UnavailablePanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        Err(wezterm_attention::protocol::AttentionError::new(
            "realm_unavailable",
            "test listing failure",
        ))
    }
}

fn claim_file(environment: &BTreeMap<String, String>) -> PathBuf {
    let (address, _) = pane_address(environment).expect("pane address");
    pane_path(&state_root(environment).expect("state root"), &address).join("claim.json")
}

#[test]
fn a_claim_from_a_terminal_other_than_the_pane_s_own_is_refused() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let elsewhere = FakePanes(vec![PaneRow {
        pane_id: "42".into(),
        tty_name: Some("/dev/ttys001".into()),
    }]);
    let error = wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &elsewhere))
        .expect_err("an inherited pane id must not claim from another terminal");
    assert_eq!(error.diagnostic.code, "unsafe_tty");
    let unlisted = FakePanes(vec![PaneRow {
        pane_id: "7".into(),
        tty_name: Some(tty.path.clone()),
    }]);
    let error = wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &unlisted))
        .expect_err("a pane its mux does not list must not be claimed");
    assert_eq!(error.diagnostic.code, "unsafe_tty");
    assert!(!claim_file(&environment).exists());
    assert!(tty.writes.lock().expect("writes lock").is_empty());
}

#[test]
fn a_claim_takes_one_pane_listing() {
    let (_scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    let panes = CountingPanes {
        calls: AtomicUsize::new(0),
    };
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &panes))
        .expect("the pane's own terminal claims");
    assert_eq!(panes.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn without_a_listing_a_claim_inside_tmux_or_screen_is_refused_and_others_proceed() {
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    for (name, value, refused) in [
        ("TMUX", "/tmp/tmux-501/default,1,0", true),
        ("STY", "1234.pts-0.host", true),
        ("TMUX", "", false),
        ("UNRELATED", "value", false),
    ] {
        let (_scratch, _listener, mut environment) = setup();
        environment.insert(name.into(), value.into());
        let result =
            wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &UnavailablePanes));
        if refused {
            assert_eq!(
                result.expect_err("refused").diagnostic.code,
                "unsafe_tty",
                "{name}={value}"
            );
            assert!(!claim_file(&environment).exists());
        } else {
            result.expect("proceeds as before when nothing says the pane was inherited");
            assert!(claim_file(&environment).exists());
        }
    }
}

#[test]
fn a_complete_realm_wide_answer_exits_zero_beside_its_diagnostics() {
    let (scratch, _listener, environment) = setup();
    let clock = FixedClock("00000000000000000100");
    let tty = FakeTty::new();
    wezterm_attention::claim_launch(&environment, &ports(&clock, &tty, &this_pane()))
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
    // A binding filed under another id is refused with a diagnostic.
    atomic_replace(
        &launch
            .join("bindings")
            .join("d".repeat(64))
            .join("binding.json"),
        &binding_record(&address, launch_id, &binding_id),
    )
    .expect("write path-mismatched binding");
    let bin = scratch.path.join("bin");
    fs::create_dir(&bin).expect("create bin");
    trusted_scratch::write_script(
        &bin.join("wezterm"),
        "printf '%s' '[{\"pane_id\":42,\"tty_name\":\"/dev/ttys999\"}]'",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .args(["bindings", "--all"])
        .env_clear()
        .env("HOME", &environment["HOME"])
        .env(
            "WEZTERM_ATTENTION_DIR",
            &environment["WEZTERM_ATTENTION_DIR"],
        )
        .env("PATH", &bin)
        .output()
        .expect("run bindings");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("bindings JSON");
    assert_eq!(envelope["complete"], true, "{envelope}");
    assert_eq!(envelope["status"], "findings");
    assert_eq!(envelope["result"]["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(output.status.code(), Some(0), "{envelope}");
}

/// macOS only: after the queue is filled, freeing part of it lets a pty
/// there take part of a publication. A Linux pty frees room in whole buffer
/// blocks, so the same steps take all of it and no cut can be staged; the
/// cut itself is covered on every platform by the writer's unit tests.
#[cfg(target_os = "macos")]
#[test]
fn a_publication_cut_short_by_a_stalled_tty_is_closed_once_the_tty_drains() {
    let (_scratch, _listener, environment) = setup();
    let (address, _) = pane_address(&environment).expect("pane address");
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
    let tty_path = tty_path_from_fd(slave).expect("tty path");
    let fingerprint =
        wezterm_attention::identity::tty_fingerprint(&tty_path).expect("tty fingerprint");
    let data = publication_bytes(&address, Some("00000000-0000-4000-8000-000000000101"))
        .expect("publication");
    // Free less room than the publication needs, so it starts and stalls.
    let mut room = vec![0_u8; data.len() / 2];
    let freed = unsafe { libc::read(master, room.as_mut_ptr().cast(), room.len()) };
    assert!(freed > 0);
    let drained = thread::spawn(move || {
        // Stay stalled past the write deadline, then drain everything.
        thread::sleep(Duration::from_millis(400));
        let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
        unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        let mut received = Vec::new();
        let until = Instant::now() + Duration::from_millis(600);
        let mut block = [0_u8; 4096];
        while Instant::now() < until {
            let count = unsafe { libc::read(master, block.as_mut_ptr().cast(), block.len()) };
            if count > 0 {
                received.extend_from_slice(&block[..count as usize]);
            } else {
                thread::sleep(Duration::from_millis(5));
            }
        }
        received
    });
    let result = SystemTtyWriter.write(&tty_path, &data, &fingerprint);
    let received = drained.join().expect("drain");
    unsafe {
        libc::close(master);
        libc::close(slave);
    }
    assert!(
        result.is_err(),
        "the publication did not fit, so it is incomplete"
    );
    let published = received
        .iter()
        .position(|byte| *byte == 0x1b)
        .map(|start| &received[start..])
        .expect("the publication started");
    assert!(
        published.len() < data.len() && data.starts_with(&published[..published.len() - 2]),
        "filled {filled}, received {} publication bytes",
        published.len()
    );
    assert!(
        published.ends_with(b"!\x07"),
        "the cut sequence must end unparseable: {:?}",
        String::from_utf8_lossy(&published[published.len().saturating_sub(8)..])
    );
}

#[test]
fn a_claim_names_its_owner_with_all_owner_fields_or_none() {
    let fixture: Value = serde_json::from_str(include_str!("../fixtures/v2/protocol-cases.json"))
        .expect("protocol fixture JSON");
    let protocol = manifest().expect("embedded manifest");
    let shell = fixture["record_samples"]["claim"].clone();
    let mut self_owned = shell.clone();
    for (field, value) in [
        ("owner_pid", "4242"),
        ("owner_started_sec", "1700000000"),
        ("owner_started_usec", "123456"),
        (
            "owner_boot_session_id",
            "0f9a7c3e-51b2-4d6e-8a1b-2c3d4e5f6a7b",
        ),
    ] {
        self_owned[field] = json!(value);
    }
    assert_eq!(parse_record_value(&shell, protocol).as_str(), "valid");
    assert_eq!(parse_record_value(&self_owned, protocol).as_str(), "valid");
    for field in wezterm_attention::protocol::CLAIM_OWNER_FIELDS {
        let mut partial = self_owned.clone();
        partial.as_object_mut().expect("claim object").remove(field);
        assert_eq!(
            parse_record_value(&partial, protocol).as_str(),
            "record_invalid",
            "a claim missing only {field} is neither kind"
        );
    }
    for (field, value) in [
        ("owner_pid", "0"),
        ("owner_pid", "2147483648"),
        ("owner_started_sec", "18446744073709551616"),
        ("owner_started_usec", "1000000"),
    ] {
        let mut wrong = self_owned.clone();
        wrong[field] = json!(value);
        assert_eq!(
            parse_record_value(&wrong, protocol).as_str(),
            "record_invalid",
            "{field} {value}"
        );
    }

    // On disk a partial claim reads as invalid, never as a shell claim.
    let scratch = Scratch::new();
    let address: wezterm_attention::identity::PaneAddress =
        serde_json::from_value(shell["address"].clone()).expect("address");
    let path = pane_path(&scratch.path, &address).join("claim.json");
    let mut partial = self_owned.clone();
    partial
        .as_object_mut()
        .expect("claim object")
        .remove("owner_boot_session_id");
    fs::create_dir_all(path.parent().expect("pane directory")).expect("pane directory");
    fs::write(&path, serde_json::to_vec(&partial).expect("claim bytes")).expect("write claim");
    assert!(matches!(
        read_record_typed(&path, Some("claim"), &RecordIdentity::pane(&address)),
        RecordRead::Invalid(_)
    ));
}
