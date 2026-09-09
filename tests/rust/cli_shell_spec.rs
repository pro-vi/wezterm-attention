use std::fs;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = PathBuf::from("/tmp").join(format!("wa-shell-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&path).expect("create scratch");
        Self(path)
    }

    fn writer(&self, body: &str) -> PathBuf {
        let bin = self.0.join("bin");
        fs::create_dir_all(&bin).expect("create bin");
        let writer = bin.join("attention");
        fs::write(&writer, format!("#!/bin/sh\n{body}")).expect("write fake writer");
        fs::set_permissions(&writer, fs::Permissions::from_mode(0o755)).expect("chmod writer");
        writer
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn repo_file(path: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(path)
        .to_string_lossy()
        .into_owned()
}

fn count_named_files(root: &Path, name: &str) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .map(|path| {
            if path.is_dir() {
                count_named_files(&path, name)
            } else {
                usize::from(path.file_name().and_then(|item| item.to_str()) == Some(name))
            }
        })
        .sum()
}

#[test]
fn zsh_explicit_claim_uses_selected_ids_and_clears_inherited_id_on_failure() {
    let scratch = Scratch::new();
    let log = scratch.0.join("calls");
    scratch.writer(&format!(
        "if [ -f '{log}' ]; then selected=00000000-0000-4000-8000-000000000202; else selected=00000000-0000-4000-8000-000000000201; fi\n\
         printf '%s|%s|%s\\n' \"${{WEZTERM_ATTENTION_LAUNCH_ID:-}}\" \"$selected\" \"$*\" >> '{log}'\n\
         printf '%s\\n' \"$selected\"\n",
        log = log.display()
    ));
    let script = format!(
        "export WEZTERM_ATTENTION_ROOT='{}'; source '{}'; wezterm_attention_claim; first=$WEZTERM_ATTENTION_LAUNCH_ID; wezterm_attention_claim; printf '%s|%s\\n' \"$first\" \"$WEZTERM_ATTENTION_LAUNCH_ID\"",
        scratch.0.display(),
        repo_file("shell/wezterm-attention.zsh")
    );
    let output = Command::new("zsh")
        .args(["-f", "-c", &script])
        .output()
        .expect("run zsh");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let selected = String::from_utf8(output.stdout).expect("stdout");
    assert!(
        selected
            .contains("00000000-0000-4000-8000-000000000201|00000000-0000-4000-8000-000000000202")
    );
    let calls = fs::read_to_string(&log).expect("calls");
    assert_eq!(calls.lines().count(), 2);
    assert!(calls.lines().all(|line| line.starts_with('|')));

    scratch.writer("exit 3\n");
    let failure = format!(
        "export WEZTERM_ATTENTION_ROOT='{}'; export WEZTERM_ATTENTION_LAUNCH_ID=00000000-0000-4000-8000-000000000999; source '{}'; wezterm_attention_claim || :; [[ -z ${{WEZTERM_ATTENTION_LAUNCH_ID:-}} ]]",
        scratch.0.display(),
        repo_file("shell/wezterm-attention.zsh")
    );
    assert!(
        Command::new("zsh")
            .args(["-f", "-c", &failure])
            .status()
            .expect("run failure zsh")
            .success()
    );
}

#[test]
fn bash_automatic_claim_preserves_debug_trap_and_keeps_pending_publication_id() {
    let scratch = Scratch::new();
    let log = scratch.0.join("calls");
    scratch.writer(&format!(
        "if [ -f '{log}' ]; then selected=00000000-0000-4000-8000-000000000202; else selected=00000000-0000-4000-8000-000000000201; fi\n\
         printf '%s|%s|%s\\n' \"${{WEZTERM_ATTENTION_LAUNCH_ID:-}}\" \"$selected\" \"$*\" >> '{log}'\n\
         printf '%s\\n' \"$selected\"\n\
         printf '%s\\n' 'attention: publication pending: unsafe_tty: test failure' >&2\n",
        log = log.display()
    ));
    let claude = scratch.0.join("claude");
    fs::write(&claude, "#!/bin/sh\nexit 0\n").expect("write claude");
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod claude");
    let script = format!(
        "export PATH='{}':\"$PATH\"; export WEZTERM_ATTENTION_ROOT='{}'; trap \"printf '<OLD:quoted phrase>\\n' >/dev/null\" DEBUG; source '{}'; eval \"$_WEZTERM_ATTENTION_PROMPT_INSTALL\"; M=stub claude; MODEL=\"one two\" claude; printf 'SELECTED=%s\\n' \"$WEZTERM_ATTENTION_LAUNCH_ID\"",
        scratch.0.display(),
        scratch.0.display(),
        repo_file("shell/wezterm-attention.bash")
    );
    let output = Command::new("bash")
        .args(["--noprofile", "--norc", "-c", &script])
        .output()
        .expect("run bash");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("SELECTED=00000000-0000-4000-8000-000000000202")
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("publication pending"));
    let calls = fs::read_to_string(&log).expect("calls");
    assert_eq!(calls.lines().count(), 2);
    assert!(calls.lines().all(|line| line.starts_with('|')));
}

#[test]
fn rust_cli_help_errors_and_empty_hook_input_keep_the_documented_shape() {
    let binary = env!("CARGO_BIN_EXE_attention");
    let home = Command::new(binary).output().expect("run help");
    assert!(home.status.success());
    assert!(String::from_utf8_lossy(&home.stdout).contains("Example:"));
    let obsolete = Command::new(binary)
        .arg("claim")
        .output()
        .expect("run obsolete command");
    assert_eq!(obsolete.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&obsolete.stderr).contains("attention hooks claim"));

    let operational = Command::new(binary)
        .args(["mark", "notify", "--json"])
        .env_clear()
        .output()
        .expect("run JSON error");
    assert_eq!(operational.status.code(), Some(3));
    let envelope: Value = serde_json::from_slice(&operational.stdout).expect("JSON envelope");
    assert_eq!(envelope["status"], "unavailable");

    let hook_help = Command::new(binary)
        .args(["hooks", "event", "--help"])
        .output()
        .expect("run hook help");
    let help = String::from_utf8_lossy(&hook_help.stdout);
    assert!(help.contains("SubagentStop"));
    assert!(!help.contains("SubagentStart"));

    let started = Instant::now();
    let empty = Command::new(binary)
        .args(["hooks", "event", "claude", "Stop", "--strict"])
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .expect("run empty hook");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(empty.status.code(), Some(2));

    let malformed = Command::new(binary)
        .args(["bindings", "--json", "--limit", "nope"])
        .env_clear()
        .output()
        .expect("run malformed JSON argument");
    assert_eq!(malformed.status.code(), Some(2));
    assert_eq!(malformed.stderr, b"");
    let envelope: Value = serde_json::from_slice(&malformed.stdout).expect("usage JSON envelope");
    assert_eq!(envelope["command"], "bindings");
    assert_eq!(envelope["status"], "usage_error");
    assert_eq!(envelope["complete"], true);
}

#[test]
fn installed_shim_names_the_install_command_when_the_rust_binary_is_missing() {
    let scratch = Scratch::new();
    let bin = scratch.0.join("bin");
    fs::create_dir_all(&bin).expect("create bin");
    fs::copy(repo_file("bin/attention"), bin.join("attention")).expect("copy shim");
    let output = Command::new(bin.join("attention"))
        .arg("doctor")
        .output()
        .expect("run shim without Rust binary");
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stdout, b"");
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr"),
        "attention: Rust binary is missing; run scripts/install-cli.sh and retry.\n"
    );
}

#[test]
fn installed_default_shim_claims_without_python_on_path() {
    let scratch = Scratch::new();
    let bin = scratch.0.join("bin");
    let libexec = scratch.0.join("libexec");
    fs::create_dir_all(&bin).expect("create bin");
    fs::create_dir_all(&libexec).expect("create libexec");
    fs::copy(repo_file("bin/attention"), bin.join("attention")).expect("copy shim");
    fs::copy(
        env!("CARGO_BIN_EXE_attention"),
        libexec.join("attention-rs"),
    )
    .expect("copy Rust binary");
    fs::set_permissions(
        libexec.join("attention-rs"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("chmod Rust binary");
    let socket_path = scratch.0.join("mux.sock");
    let _listener = UnixListener::bind(&socket_path).expect("bind socket");
    let state = scratch.0.join("state");
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
    let claim_stdin = unsafe { fs::File::from_raw_fd(libc::dup(slave)) };
    let output = Command::new(bin.join("attention"))
        .args(["hooks", "claim", "--json"])
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("HOME", &scratch.0)
        .env("WEZTERM_ATTENTION_DIR", &state)
        .env("WEZTERM_UNIX_SOCKET", &socket_path)
        .env("WEZTERM_PANE", "42")
        .env(
            "WEZTERM_ATTENTION_LAUNCH_ID",
            "00000000-0000-4000-8000-000000000777",
        )
        .stdin(Stdio::from(claim_stdin))
        .output()
        .expect("run default shim claim");
    unsafe {
        libc::close(master);
        libc::close(slave);
    }
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("claim envelope");
    assert_eq!(envelope["result"]["publication"], "published");
    assert_eq!(count_named_files(&state, "claim.json"), 1);
}

#[test]
fn installer_creates_libexec_in_a_fresh_checkout() {
    let scratch = Scratch::new();
    let scripts = scratch.0.join("scripts");
    let tools = scratch.0.join("tools");
    fs::create_dir_all(&scripts).expect("create scripts");
    fs::create_dir_all(&tools).expect("create tools");
    fs::copy(
        repo_file("scripts/install-cli.sh"),
        scripts.join("install-cli.sh"),
    )
    .expect("copy installer");
    let cargo = tools.join("cargo");
    fs::write(
        &cargo,
        "#!/bin/sh\nset -eu\nmkdir -p target/release\nprintf '#!/bin/sh\\nexit 0\\n' > target/release/attention\nchmod 755 target/release/attention\n",
    )
    .expect("write cargo stand-in");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).expect("chmod cargo");
    assert!(!scratch.0.join("libexec").exists());
    let output = Command::new("sh")
        .arg(scripts.join("install-cli.sh"))
        .env("PATH", format!("{}:/usr/bin:/bin", tools.to_string_lossy()))
        .output()
        .expect("run installer");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(scratch.0.join("libexec/attention-rs").is_file());
}

#[test]
fn json_publication_and_binding_output_report_bounded_completeness() {
    let scratch = Scratch::new();
    let socket_path = scratch.0.join("mux.sock");
    let _listener = UnixListener::bind(&socket_path).expect("bind socket");
    let wezterm = scratch.0.join("wezterm");
    let panes: Vec<_> = (0..60)
        .map(|index| json!({"pane_id":index.to_string(),"tty_name":"/dev/does-not-exist"}))
        .collect();
    fs::write(
        &wezterm,
        format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", json!(panes)),
    )
    .expect("write fake wezterm");
    fs::set_permissions(&wezterm, fs::Permissions::from_mode(0o755)).expect("chmod wezterm");
    let state = scratch.0.join("state");
    let path = format!("{}:/usr/bin:/bin", scratch.0.display());
    let publish = Command::new(env!("CARGO_BIN_EXE_attention"))
        .args([
            "hooks",
            "publish",
            "--realm",
            socket_path.to_str().expect("socket path"),
            "--json",
        ])
        .env_clear()
        .env("PATH", &path)
        .env("WEZTERM_ATTENTION_DIR", &state)
        .output()
        .expect("run bounded publish");
    assert_eq!(publish.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&publish.stdout).expect("publish JSON");
    assert_eq!(envelope["diagnostics"].as_array().map(Vec::len), Some(50));
    assert_eq!(envelope["complete"], false);

    let address = json!({"realm_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","incarnation_id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","pane_id":"42"});
    let launch_id = "00000000-0000-4000-8000-000000000701";
    for index in 1..=2 {
        let binding_id = if index == 1 {
            "c".repeat(64)
        } else {
            "d".repeat(64)
        };
        let binding_dir = state
            .join("v2/realms")
            .join("a".repeat(64))
            .join("incarnations")
            .join("b".repeat(64))
            .join("panes/42/launches")
            .join(launch_id)
            .join("bindings")
            .join(&binding_id);
        fs::create_dir_all(&binding_dir).expect("create binding directory");
        fs::write(
            binding_dir.join("binding.json"),
            serde_json::to_vec(&json!({
                "kind":"binding","schema":2,"address":address,"launch_id":launch_id,
                "binding_id":binding_id,"event_id":format!("00000000-0000-4000-8000-00000000070{index}"),
                "provider":"claude","provider_session_id":format!("session-{index}"),
                "start_source":"startup","observed_mono_ns":format!("0000000000000000070{index}"),
                "written_at_unix_ns":"00000000001000000000","writer_version":"2.0.0"
            }))
            .expect("binding JSON"),
        )
        .expect("write binding");
    }
    let bindings = Command::new(env!("CARGO_BIN_EXE_attention"))
        .args(["bindings", "--json", "--limit", "1"])
        .env_clear()
        .env("WEZTERM_ATTENTION_DIR", &state)
        .output()
        .expect("run bounded bindings");
    let envelope: Value = serde_json::from_slice(&bindings.stdout).expect("bindings JSON");
    assert_eq!(envelope["result"]["scanned"], 2);
    assert_eq!(envelope["result"]["returned"], 1);
    assert_eq!(envelope["result"]["truncated"], true);
    assert_eq!(envelope["complete"], false);
}
