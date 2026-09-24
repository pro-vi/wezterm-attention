use std::fs;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

#[path = "support/executables.rs"]
mod executables;
#[path = "support/trusted_scratch.rs"]
mod trusted_scratch;

/// The build script itself, included so the identity it computes can be
/// checked against scratch repositories without a nested cargo build.
#[allow(dead_code)]
mod build_script {
    include!("../../build.rs");

    use super::Scratch;
    use std::fs;

    #[test]
    fn the_build_identity_names_only_this_checkout_and_watches_only_files_that_exist() {
        let scratch = Scratch::new();
        let git = |directory: &Path, args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        let crate_files = |directory: &Path| {
            for (path, text) in [
                ("src/main.rs", "fn main() {}\n"),
                ("protocol/v2.json", "{}\n"),
                ("build.rs", "fn main() {}\n"),
                ("Cargo.toml", "[package]\n"),
                ("Cargo.lock", "\n"),
                ("docs/notes.md", "notes\n"),
            ] {
                let path = directory.join(path);
                fs::create_dir_all(path.parent().expect("parent")).expect("create directory");
                fs::write(path, text).expect("write crate file");
            }
        };
        let every_watched_path_exists = |identity: &BuildIdentity| {
            for path in &identity.watched {
                assert!(
                    path.exists(),
                    "watching a missing path rebuilds every time: {path:?}"
                );
            }
        };
        let is_commit =
            |build: &str| build.len() == 12 && build.bytes().all(|b| b.is_ascii_hexdigit());

        let repository = scratch.0.join("repository");
        crate_files(&repository);
        git(&repository, &["init", "-q", "-b", "main"]);
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-q", "-m", "initial"]);
        let clean = build_identity(&repository);
        assert!(is_commit(&clean.build), "{}", clean.build);
        every_watched_path_exists(&clean);
        assert!(clean.watched.contains(&repository.join("src")));

        fs::write(repository.join("docs/notes.md"), "edited\n").expect("edit a document");
        assert_eq!(build_identity(&repository).build, clean.build);
        fs::write(repository.join("src/main.rs"), "fn main() { }\n").expect("edit a source");
        assert_eq!(
            build_identity(&repository).build,
            format!("{}-dirty", clean.build)
        );
        git(
            &repository,
            &["checkout", "-q", "--", "src/main.rs", "docs/notes.md"],
        );

        let vendored = repository.join("vendor/attention");
        crate_files(&vendored);
        let copy = build_identity(&vendored);
        assert_eq!(copy.build, "unknown", "a vendored copy names no commit");
        assert_eq!(copy.watched.len(), BINARY_INPUTS.len());

        let linked = scratch.0.join("linked");
        let linked_path = linked.to_str().expect("UTF-8 scratch path");
        git(
            &repository,
            &["worktree", "add", "-q", "-b", "linked", linked_path],
        );
        git(&repository, &["pack-refs", "--all"]);
        for checkout in [&repository, &linked] {
            let identity = build_identity(checkout);
            assert_eq!(identity.build, clean.build, "{checkout:?}");
            every_watched_path_exists(&identity);
            assert!(
                identity
                    .watched
                    .iter()
                    .any(|path| path.ends_with("logs/HEAD")),
                "a commit on a packed ref still moves a watched file: {:?}",
                identity.watched
            );
        }
    }
}

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
fn tab_source_matches_socket_identity_without_reading_or_writing_state() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let socket = scratch.0.join("gui.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let alias = scratch.0.join("alias.sock");
    symlink(&socket, &alias).unwrap();
    let state = scratch.0.join("absent-state");
    let (realm, incarnation, metadata) =
        wezterm_attention::identity::socket_identity(socket.to_str().unwrap()).unwrap();
    let run = |socket: &Path| {
        Command::new(env!("CARGO_BIN_EXE_attention"))
            .env_clear()
            .env("WEZTERM_ATTENTION_DIR", &state)
            .args(["tab-source", "--socket", socket.to_str().unwrap()])
            .output()
            .unwrap()
    };
    for path in [&socket, &alias] {
        let output = run(path);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            value["result"],
            json!({"socket_path":metadata.socket_path,"realm_id":realm,"incarnation_id":incarnation})
        );
        assert_eq!(value["command"], "tab-source");
        assert_eq!(value["complete"], true);
    }
    fs::remove_file(&socket).unwrap();
    let _replacement = UnixListener::bind(&socket).unwrap();
    let replacement: Value = serde_json::from_slice(&run(&socket).stdout).unwrap();
    assert_eq!(replacement["result"]["realm_id"], realm);
    assert_ne!(replacement["result"]["incarnation_id"], incarnation);
    for path in [Path::new("relative.sock"), &scratch.0, &state] {
        let output = run(path);
        assert!(!output.status.success());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_ne!(value["status"], "ok");
        assert_eq!(value["result"], json!({}));
    }
    assert!(!state.exists());
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
fn the_binary_names_the_commit_it_was_built_from() {
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .arg("--version")
        .env_clear()
        .output()
        .expect("run --version");
    assert!(output.status.success());
    let version = String::from_utf8_lossy(&output.stdout);
    let (name, rest) = version.trim().split_once(' ').expect("name then version");
    assert_eq!(name, "attention");
    let (crate_version, build) = rest.split_once(" (").expect("version then build");
    assert_eq!(crate_version, env!("CARGO_PKG_VERSION"));
    let build = build.strip_suffix(')').expect("closing paren");
    let commit = build.strip_suffix("-dirty").unwrap_or(build);
    assert!(
        commit == "unknown"
            || (commit.len() == 12 && commit.bytes().all(|b| b.is_ascii_hexdigit())),
        "build is a 12-hex commit or unknown, got {build:?}"
    );
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
    assert_eq!(operational.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&operational.stdout).expect("JSON envelope");
    assert_eq!(envelope["status"], "unavailable");
    assert_eq!(envelope["complete"], false);

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
    assert_eq!(empty.status.code(), Some(1));

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
    assert_eq!(envelope["complete"], false);
}

#[test]
fn query_defaults_errors_and_help_support_agent_composition() {
    use std::io::Write;
    let binary = env!("CARGO_BIN_EXE_attention");
    let run = |args: &[&str]| {
        Command::new(binary)
            .env_clear()
            .args(args)
            .output()
            .unwrap()
    };
    let default = run(&["hooks", "describe", "--provider", "claude"]);
    let explicit = run(&["hooks", "describe", "--provider", "claude", "--json"]);
    assert!(default.status.success() && explicit.status.success());
    assert_eq!(default.stdout, explicit.stdout);
    assert!(default.stderr.is_empty());
    let description: Value = serde_json::from_slice(&default.stdout).unwrap();
    assert_eq!(description["command"], "hooks describe");
    for (args, exit) in [
        (vec!["hooks", "describe", "--provider", "unsupported"], 1),
        (vec!["bindings", "--provider", "unsupported"], 2),
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(exit));
        let error: Value = serde_json::from_slice(&output.stdout).unwrap();
        let message = error["diagnostics"][0]["message"].as_str().unwrap();
        for provider in &wezterm_attention::protocol::manifest()
            .unwrap()
            .enums
            .providers
        {
            assert!(message.contains(provider));
        }
    }
    for (input, expected) in [
        ("", "stdin is empty"),
        ("{", "syntax is invalid"),
        ("{}", "scope requires address"),
        (
            "{\"unexpected\":\"synthetic-private-content\"}",
            "scope requires address",
        ),
    ] {
        let mut child = Command::new(binary)
            .env_clear()
            .args(["inspect", "--scope", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stderr.is_empty());
        let error: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            error["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains(expected)
        );
        assert_eq!(error["diagnostics"][0]["help"], "attention inspect --help");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-private-content"));
    }
    let malformed = run(&["hooks", "describe"]);
    assert_eq!(malformed.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&malformed.stdout).unwrap();
    assert_eq!(error["command"], "hooks describe");
    assert_eq!(
        error["diagnostics"][0]["help"],
        "attention hooks describe --help"
    );
    let help = run(&["inspect", "--help"]);
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("scope.json"));
    assert!(text.contains("pane_presence") && text.contains("binding_health"));
    assert!(text.contains("reader_confidence") && text.contains("current"));
    assert!(!text.contains("complete=true before selecting"));
    let help = run(&["sweep", "--help"]);
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(!text.contains("00000000-0000-4000-8000-000000000001"));
    assert!(text.contains("result.operation_id"));
    assert!(text.contains("canonical lowercase UUID"));
    assert!(text.contains("Preview is the default"));
    assert!(text.contains("--all-details"));
    assert!(text.contains("complete"));
    let help = run(&["hooks", "event", "--help"]);
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("required with --consumer") && text.contains("Retrying"));
    assert!(text.contains("stdout stays empty") && text.contains("default hooks exit zero"));
    assert!(text.contains("--include-prompt") && text.contains("transient consumer stdin"));
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
    assert_eq!(output.status.code(), Some(1));
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
            "--socket",
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
                "kind":"binding","schema":3,"address":address,"launch_id":launch_id,
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

#[test]
fn published_tab_orders_are_read_and_one_refused_file_does_not_withhold_the_others() {
    let scratch = Scratch::new();
    let state = scratch.0.join("state");
    let tabs = state.join("tabs");
    fs::create_dir_all(&tabs).expect("create tab publication directory");
    // Drawn order, not sorted order: the bar drew 11 before 4, and that is the
    // fact the file exists to carry. The first tab is a v2 cache key, the
    // second a pair of v1 marker ids — the mix `gui_tab_pane_ids` writes.
    let v2_id = concat!(
        "v2:",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ":",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ":16"
    );
    fs::write(
        tabs.join("0.json"),
        format!(
            r#"{{"published_at_ms":1789884000123,"schema":1,"tabs":[{{"marker_ids":["{v2_id}"],"number":11,"text":" 11: braid "}},{{"marker_ids":["4","9"],"number":4,"text":" 4: construal "}}],"window_id":0}}"#
        ),
    )
    .expect("write window 0");
    fs::write(
        tabs.join("12.json"),
        r#"{"published_at_ms":1789884000456,"schema":1,"tabs":[],"window_id":12}"#,
    )
    .expect("write window 12");
    // A later publisher's shape, a file filed under a window it does not name,
    // a field this schema does not have, and a name that is not a window ID.
    fs::write(
        tabs.join("3.json"),
        r#"{"published_at_ms":1789884000789,"schema":3,"tabs":[],"window_id":3}"#,
    )
    .expect("write future window");
    fs::write(
        tabs.join("7.json"),
        r#"{"published_at_ms":1789884000789,"schema":1,"tabs":[],"window_id":8}"#,
    )
    .expect("write misfiled window");
    fs::write(
        tabs.join("9.json"),
        r#"{"published_at_ms":1789884000789,"schema":1,"tabs":[],"window_id":9,"focused":true}"#,
    )
    .expect("write window with an unknown field");
    fs::write(
        tabs.join("08.json"),
        r#"{"published_at_ms":1789884000789,"schema":1,"tabs":[],"window_id":8}"#,
    )
    .expect("write non-canonical name");
    fs::write(
        tabs.join("5.json"),
        r#"{"published_at_ms":1789884000789,"schema":1,"tabs":[{"marker_ids":["not-an-id"],"number":1,"text":" 1: x "}],"window_id":5}"#,
    )
    .expect("write window with an unusable marker id");
    fs::write(tabs.join("notes.txt"), "not a publication").expect("write unrelated file");

    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .arg("tabs")
        .env_clear()
        .env("WEZTERM_ATTENTION_DIR", &state)
        .output()
        .expect("run tabs");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("tabs JSON");
    assert_eq!(envelope["schema"], 1);
    assert_eq!(envelope["command"], "tabs");
    assert_eq!(envelope["status"], "findings");
    assert_eq!(envelope["complete"], false);
    let shown = envelope["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .len();
    assert!(shown > 0);
    assert_eq!(envelope["result"]["diagnostic_count"], shown);
    assert_eq!(envelope["result"]["total_diagnostic_count"], shown);

    let windows = envelope["result"]["windows"]
        .as_array()
        .expect("windows array");
    assert_eq!(windows.len(), 2, "{windows:?}");
    assert_eq!(windows[0]["window_id"], 0);
    assert_eq!(windows[1]["window_id"], 12);
    assert_eq!(windows[0]["published_at_ms"], 1789884000123u64);
    let drawn = windows[0]["tabs"].as_array().expect("tabs array");
    assert_eq!(drawn.len(), 2);
    assert_eq!(drawn[0]["number"], 11);
    assert_eq!(drawn[0]["text"], " 11: braid ");
    assert_eq!(drawn[0]["marker_ids"], json!([v2_id]));
    assert_eq!(drawn[1]["number"], 4);
    assert_eq!(drawn[1]["marker_ids"], json!(["4", "9"]));

    let codes: Vec<&str> = envelope["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .map(|item| item["code"].as_str().expect("diagnostic code"))
        .collect();
    assert_eq!(
        codes
            .iter()
            .filter(|code| **code == "future_schema")
            .count(),
        1
    );
    assert_eq!(
        codes
            .iter()
            .filter(|code| **code == "record_invalid")
            .count(),
        4
    );
    assert_eq!(codes.len(), 5, "{codes:?}");
}

#[test]
fn attention_tabs_reads_a_file_the_plugin_encoder_wrote() {
    let scratch = Scratch::new();
    let state = scratch.0.join("state");
    fs::create_dir_all(&state).expect("create state root");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let encoded = Command::new("luajit")
        .arg(repo.join("tests/lua/support/write_tab_publication.lua"))
        .env_clear()
        .env("WEZTERM_ATTENTION_DIR", &state)
        .env("ATTENTION_REPO", repo)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .expect("run plugin encoder");
    assert!(
        encoded.status.success(),
        "plugin encoder failed: {}",
        String::from_utf8_lossy(&encoded.stderr)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .arg("tabs")
        .env_clear()
        .env("WEZTERM_ATTENTION_DIR", &state)
        .output()
        .expect("run tabs");
    assert_eq!(output.status.code(), Some(0), "{:?}", output);
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("tabs JSON");
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["complete"], true);
    let v2_id = concat!(
        "v2:",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ":",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ":16"
    );
    let windows = envelope["result"]["windows"]
        .as_array()
        .expect("windows array");
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["window_id"], 0);
    assert_eq!(windows[0]["tabs"][0]["marker_ids"], json!([v2_id]));
    assert_eq!(windows[0]["tabs"][1]["marker_ids"], json!(["4", "9"]));
}

#[test]
fn lua_tab_publication_round_trips_identity() {
    let scratch = Scratch::new();
    let socket = scratch.0.join("gui.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let state = scratch.0.join("state");
    fs::create_dir_all(&state).unwrap();
    let descriptor = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .args(["tab-source", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(descriptor.status.success());
    let response: Value = serde_json::from_slice(&descriptor.stdout).unwrap();
    let encoded = Command::new("wezterm")
        .args([
            "--config-file",
            &repo_file("tests/lua/support/write_tab_publication.lua"),
            "show-keys",
            "--lua",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("WEZTERM_ATTENTION_DIR", &state)
        .env("ATTENTION_REPO", env!("CARGO_MANIFEST_DIR"))
        .env(
            "ATTENTION_TAB_SOURCE_RESPONSE",
            String::from_utf8(descriptor.stdout).unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    let (windows, diagnostics) = wezterm_attention::query::read_tab_publications(&state).unwrap();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        windows.len(),
        1,
        "encoder stderr: {}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    let window = serde_json::to_value(&windows[0]).unwrap();
    assert_eq!(window["source"], response["result"]);
    assert_eq!(window["tabs"][0]["number"], 11);
    assert_eq!(window["tabs"][1]["number"], 4);
    assert!(window.get("relative_path").is_none());
    let file = state.join("tabs").join(format!(
        "{}-0.json",
        response["result"]["incarnation_id"].as_str().unwrap()
    ));
    assert!(file.exists());
    // Same-number legacy data remains distinct, even when the new writer exists.
    fs::write(
        state.join("tabs/0.json"),
        r#"{"schema":1,"window_id":0,"published_at_ms":1,"tabs":[]}"#,
    )
    .unwrap();
    assert_eq!(
        wezterm_attention::query::read_tab_publications(&state)
            .unwrap()
            .0
            .len(),
        2
    );
    let mut bad: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    bad["source"]["realm_id"] = json!("0".repeat(64));
    fs::write(&file, serde_json::to_vec(&bad).unwrap()).unwrap();
    let (valid, refused) = wezterm_attention::query::read_tab_publications(&state).unwrap();
    assert_eq!(valid.len(), 1);
    assert_eq!(refused.len(), 1);
}

#[test]
fn tabs_cli_checks_exact_source_without_autostart_or_global_failure() {
    let scratch = Scratch::new();
    let socket = scratch.0.join("gui.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let source = serde_json::to_value(
        wezterm_attention::query::read_tab_source(socket.to_str().unwrap()).unwrap(),
    )
    .unwrap();
    let state = scratch.0.join("state");
    fs::create_dir_all(state.join("tabs")).unwrap();
    for id in [0, 1] {
        fs::write(
            state.join("tabs").join(format!(
                "{}-{id}.json",
                source["incarnation_id"].as_str().unwrap()
            )),
            serde_json::to_vec(
                &json!({"schema":2,"source":source,"window_id":id,"published_at_ms":7,"tabs":[]}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    let executable = scratch.0.join("wezterm");
    let calls = scratch.0.join("calls");
    let header = format!(
        "#!/bin/sh\n[ \"$*\" = '--skip-config cli --prefer-mux --no-auto-start list --format json' ] || exit 8\n[ \"$WEZTERM_UNIX_SOCKET\" = '{}' ] || exit 9\n[ -z \"$UNRELATED_TEST_VALUE\" ] || exit 10\nprintf x >> '{}'\n",
        source["socket_path"].as_str().unwrap(),
        calls.display()
    );
    for (body, status, reason) in [
        (
            "printf '%s' '[{\"pane_id\":1,\"window_id\":0}]'",
            "present",
            None,
        ),
        (
            "printf '%s' '[{\"pane_id\":1}]'",
            "unavailable",
            Some("inventory_invalid"),
        ),
        (
            "printf '%s' 'bad json'",
            "unavailable",
            Some("inventory_invalid"),
        ),
        ("exit 7", "unavailable", Some("probe_unavailable")),
        (
            "exec /bin/sleep 30",
            "unavailable",
            Some("probe_unavailable"),
        ),
    ] {
        fs::write(&executable, format!("{header}{body}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&calls, "").unwrap();
        let started = Instant::now();
        let output = Command::new(env!("CARGO_BIN_EXE_attention"))
            .env_clear()
            .env("WEZTERM_ATTENTION_DIR", &state)
            .env("PATH", &scratch.0)
            .env("WEZTERM_UNIX_SOCKET", "/wrong/caller.sock")
            .env("UNRELATED_TEST_VALUE", "must-not-cross")
            .arg("tabs")
            .output()
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "inventory deadline did not bound the query"
        );
        assert!(output.status.success());
        assert_eq!(
            fs::read_to_string(&calls).unwrap(),
            "x",
            "one lookup for both windows"
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["complete"], true);
        assert_eq!(value["status"], "ok");
        assert_eq!(value["diagnostics"], json!([]));
        let windows = value["result"]["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["window_check"]["status"], status);
        assert_eq!(
            windows[1]["window_check"]["status"],
            if reason.is_some() {
                "unavailable"
            } else {
                "not_listed"
            }
        );
        assert_eq!(
            windows[0]["window_check"]["checked_at_ms"],
            windows[1]["window_check"]["checked_at_ms"]
        );
        if let Some(reason) = reason {
            assert_eq!(windows[0]["window_check"]["reason"], reason);
        } else {
            assert!(windows[0]["window_check"].get("reason").is_none());
        }
        assert_eq!(windows[0]["published_at_ms"], 7);
    }
}

#[test]
#[ignore = "opens a disposable GUI with isolated configuration and state"]
fn disposable_gui_publishes_its_own_source() {
    let wezterm = executables::resolve("wezterm");
    let child_path = executables::child_path(&[], &[&wezterm]);
    struct Gui(std::process::Child);
    impl Drop for Gui {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let scratch = Scratch::new();
    let package = scratch.0.join("package");
    for directory in ["plugin", "protocol", "bin", "libexec"] {
        fs::create_dir_all(package.join(directory)).unwrap();
    }
    for entry in fs::read_dir(repo_file("plugin")).unwrap() {
        let entry = entry.unwrap();
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "lua")
        {
            fs::copy(entry.path(), package.join("plugin").join(entry.file_name())).unwrap();
        }
    }
    fs::copy(
        repo_file("protocol/v2.json"),
        package.join("protocol/v2.json"),
    )
    .unwrap();
    fs::copy(repo_file("bin/attention"), package.join("bin/attention")).unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_attention"),
        package.join("libexec/attention-rs"),
    )
    .unwrap();
    let config = scratch.0.join("config.lua");
    let report = scratch.0.join("report.json");
    let gui_log = scratch.0.join("gui.log");
    let state = scratch.0.join("state");
    fs::write(
        &config,
        r#"
local wezterm = require('wezterm')
local probe = { spawned=0 }
local spawn = wezterm.run_child_process
wezterm.run_child_process = function(args)
  probe.spawned = probe.spawned + 1
  local ok, out, err = spawn(args)
  probe.success = ok
  return ok, out, err
end
wezterm.log_error = function(message) probe.log = message end
package.path = os.getenv('ATTENTION_GUI_PACKAGE') .. '/?/init.lua;' .. package.path
local attention = require('plugin')
local config = {
  check_for_updates=false, automatically_reload_config=false,
  status_update_interval=100, initial_cols=60, initial_rows=12,
  window_close_confirmation='NeverPrompt', default_prog={'/bin/sleep','120'},
}
attention.apply_to_config(config, {
  dir=os.getenv('ATTENTION_GUI_STATE'), review_key=false, auto_clear={}, request_redraw=false,
})
wezterm.on('update-status', function(window)
  local source = attention._internal.tab_source()
  if source then window:set_right_status('source acquired') end
    local file = assert(io.open(os.getenv('ATTENTION_GUI_REPORT'), 'w'))
    file:write(wezterm.json_encode({source=source, probe=probe, socket=os.getenv('WEZTERM_UNIX_SOCKET'), window_id=window:window_id()}))
    file:close()
end)
return config
"#,
    )
    .unwrap();
    let mut gui = Gui(Command::new(&wezterm)
        .env_clear()
        .env("PATH", &child_path)
        .env("ATTENTION_GUI_PACKAGE", &package)
        .env("ATTENTION_GUI_STATE", &state)
        .env("ATTENTION_GUI_REPORT", &report)
        .args([
            "--config-file",
            config.to_str().unwrap(),
            "start",
            "--always-new-process",
            "--no-auto-connect",
            "--class",
        ])
        .arg(format!("attention-test-{}", Uuid::new_v4()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(&gui_log).unwrap())
        .spawn()
        .unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    let (observation, windows) = loop {
        if let Ok(bytes) = fs::read(&report)
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            && let Ok((windows, diagnostics)) =
                wezterm_attention::query::read_tab_publications(&state)
            && diagnostics.is_empty()
            && windows.iter().any(|window| window.source.is_some())
        {
            break (value, windows);
        }
        assert!(
            gui.0.try_wait().unwrap().is_none(),
            "disposable GUI exited before publication"
        );
        assert!(
            Instant::now() < deadline,
            "GUI did not publish a source: report={}, log={}",
            fs::read_to_string(&report).unwrap_or_default(),
            fs::read_to_string(&gui_log).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let source = &observation["source"];
    let listing = Command::new(&wezterm)
        .env_clear()
        .env(
            "WEZTERM_UNIX_SOCKET",
            source["socket_path"].as_str().unwrap(),
        )
        .args([
            "--skip-config",
            "cli",
            "--no-auto-start",
            "list",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(listing.status.success());
    let rows: Vec<Value> = serde_json::from_slice(&listing.stdout).unwrap();
    assert!(
        rows.iter()
            .any(|row| row["window_id"] == observation["window_id"])
    );
    assert!(
        windows
            .iter()
            .any(|window| serde_json::to_value(&window.source).unwrap() == *source)
    );
    println!("GUI source acquisition, real formatter publication and exact-socket window ID agree");
    assert_eq!(observation["probe"]["spawned"], 1);
    assert_eq!(observation["probe"]["success"], true);
    let cli = |args: &[&str]| {
        Command::new(&wezterm)
            .env_clear()
            .env(
                "WEZTERM_UNIX_SOCKET",
                source["socket_path"].as_str().unwrap(),
            )
            .args(["--skip-config", "cli", "--no-auto-start"])
            .args(args)
            .output()
            .unwrap()
    };
    let spawned = cli(&["spawn", "--new-window", "--", "/bin/sleep", "120"]);
    assert!(spawned.status.success());
    let pane = String::from_utf8(spawned.stdout).unwrap().trim().to_owned();
    let rows: Vec<Value> =
        serde_json::from_slice(&cli(&["list", "--format", "json"]).stdout).unwrap();
    let second_window = rows
        .iter()
        .find(|row| row["pane_id"].as_u64().map(|id| id.to_string()).as_deref() == Some(&pane))
        .unwrap()["window_id"]
        .as_u64()
        .unwrap();
    let second_file = state.join("tabs").join(format!(
        "{}-{second_window}.json",
        source["incarnation_id"].as_str().unwrap()
    ));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !second_file.exists() {
        assert!(
            Instant::now() < deadline,
            "second GUI window did not publish"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let snapshots = scratch.0.join("snapshots");
    fs::create_dir_all(snapshots.join("tabs")).unwrap();
    for entry in fs::read_dir(state.join("tabs")).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().is_some_and(|ext| ext == "json") {
            fs::copy(entry.path(), snapshots.join("tabs").join(entry.file_name())).unwrap();
        }
    }
    assert!(cli(&["kill-pane", "--pane-id", &pane]).status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rows: Vec<Value> =
            serde_json::from_slice(&cli(&["list", "--format", "json"]).stdout).unwrap();
        if rows
            .iter()
            .all(|row| row["window_id"].as_u64() != Some(second_window))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "closed test window remains listed"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let checked = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .env("WEZTERM_ATTENTION_DIR", &snapshots)
        .env("PATH", wezterm.parent().expect("wezterm directory"))
        .arg("tabs")
        .output()
        .unwrap();
    assert!(checked.status.success());
    let value: Value = serde_json::from_slice(&checked.stdout).unwrap();
    let windows = value["result"]["windows"].as_array().unwrap();
    assert!(windows.iter().any(|w| w["window_id"] == second_window
        && w["source"] == *source
        && w["window_check"]["status"] == "not_listed"));
    assert!(
        windows
            .iter()
            .any(|w| w["window_id"] == observation["window_id"]
                && w["source"] == *source
                && w["window_check"]["status"] == "present")
    );
    println!(
        "Saved publications distinguish the closed window from the surviving window in the same GUI"
    );
}

#[test]
fn a_state_root_that_has_published_no_tab_order_answers_completely() {
    let scratch = Scratch::new();
    let state = scratch.0.join("state");
    fs::create_dir_all(&state).expect("create state root");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_attention"))
            .args(args)
            .env_clear()
            .env("WEZTERM_ATTENTION_DIR", &state)
            .output()
            .expect("run tabs")
    };
    let default = run(&["tabs"]);
    let explicit = run(&["tabs", "--json"]);
    assert_eq!(default.status.code(), Some(0));
    assert_eq!(default.stdout, explicit.stdout, "tabs is a read command");
    let envelope: Value = serde_json::from_slice(&default.stdout).expect("tabs JSON");
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["complete"], true);
    assert_eq!(envelope["result"]["windows"], json!([]));
    assert!(
        !state.join("tabs").exists(),
        "a read command creates no state directory"
    );
}

#[test]
fn a_variable_that_is_not_utf8_is_skipped_instead_of_stopping_every_command() {
    use std::os::unix::ffi::OsStrExt;
    let scratch = Scratch::new();
    let state = scratch.0.join("state");
    fs::create_dir_all(&state).expect("create state root");
    let unreadable = std::ffi::OsStr::from_bytes(b"latin-1 \xe9t\xe9");
    let tabs = Command::new(env!("CARGO_BIN_EXE_attention"))
        .arg("tabs")
        .env_clear()
        .env("WEZTERM_ATTENTION_DIR", &state)
        .env("SOME_LOCALE_VALUE", unreadable)
        .output()
        .expect("run tabs");
    assert_eq!(tabs.status.code(), Some(0), "{tabs:?}");
    let envelope: Value = serde_json::from_slice(&tabs.stdout).expect("tabs JSON");
    assert_eq!(envelope["complete"], true);

    let hook = Command::new(env!("CARGO_BIN_EXE_attention"))
        .args(["hooks", "event", "claude", "Stop"])
        .env_clear()
        .env("WEZTERM_ATTENTION_DIR", &state)
        .env("SOME_LOCALE_VALUE", unreadable)
        .stdin(Stdio::null())
        .output()
        .expect("run hook");
    assert_eq!(hook.status.code(), Some(0), "{hook:?}");
    assert!(!String::from_utf8_lossy(&hook.stderr).contains("panicked"));
}

/// Run `hooks publish --socket` for a disposable socket with only `extra` in
/// the environment besides the state root, and return the output and how
/// long it took.
fn publish_socket_with(extra: &[(&str, &Path)]) -> (std::process::Output, Duration) {
    let scratch = Scratch::new();
    let socket = scratch.0.join("mux.sock");
    let _listener = UnixListener::bind(&socket).expect("bind disposable socket");
    let mut command = Command::new(env!("CARGO_BIN_EXE_attention"));
    command
        .args(["hooks", "publish", "--json", "--socket"])
        .arg(&socket)
        .env_clear()
        .env("HOME", &scratch.0)
        .env("WEZTERM_ATTENTION_DIR", scratch.0.join("state"))
        .env("PATH", "/usr/bin:/bin");
    for (name, value) in extra {
        command.env(name, value);
    }
    let started = Instant::now();
    let output = command.output().expect("run hooks publish");
    (output, started.elapsed())
}

#[test]
fn a_pane_listing_runs_the_cli_beside_the_mux_server_and_never_starts_a_server() {
    let installed = trusted_scratch::TrustedScratch::new();
    let server_ran = installed.0.join("server-ran");
    let arguments = installed.0.join("arguments");
    let server = installed.executable(
        "wezterm-mux-server",
        &format!("printf ran > '{}'", server_ran.display()),
    );
    installed.executable(
        "wezterm",
        &format!(
            "printf '%s' \"$*\" > '{}'\nprintf '[]'",
            arguments.display()
        ),
    );
    let (output, _) = publish_socket_with(&[("WEZTERM_EXECUTABLE", &server)]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(!server_ran.exists(), "the mux server must never be run");
    assert_eq!(
        fs::read_to_string(&arguments).expect("the CLI ran"),
        "--skip-config cli --prefer-mux --no-auto-start list --format json"
    );
}

#[test]
fn a_failed_pane_listing_names_the_executable_and_how_it_failed() {
    let installed = trusted_scratch::TrustedScratch::new();
    let cli = installed.executable("wezterm", "exit 7");
    let (output, _) = publish_socket_with(&[("WEZTERM_EXECUTABLE_DIR", &installed.0)]);
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("error envelope");
    let message = envelope["diagnostics"][0]["message"]
        .as_str()
        .expect("message");
    assert_eq!(envelope["diagnostics"][0]["code"], "realm_unavailable");
    assert_eq!(
        message,
        format!(
            "wezterm cli list via {} exited with status 7",
            cli.display()
        )
    );
}

#[test]
fn a_descendant_holding_the_listing_output_open_is_killed_at_the_deadline() {
    let installed = trusted_scratch::TrustedScratch::new();
    let holder = installed.0.join("holder");
    let cli = installed.executable(
        "wezterm",
        &format!(
            "/bin/sleep 30 &\nprintf '%s' $! > '{}'\nexit 0",
            holder.display()
        ),
    );
    let (output, elapsed) = publish_socket_with(&[("WEZTERM_EXECUTABLE_DIR", &installed.0)]);
    assert!(
        elapsed < Duration::from_secs(10),
        "the listing deadline did not bound a held pipe: {elapsed:?}"
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("error envelope");
    assert_eq!(
        envelope["diagnostics"][0]["message"],
        format!(
            "wezterm cli list via {} timed out after 5000 ms",
            cli.display()
        )
    );
    let pid = fs::read_to_string(&holder).expect("holder pid");
    let alive = Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .expect("probe holder");
    assert!(!alive.success(), "the descendant outlived the deadline");
}

#[test]
fn a_reader_that_closes_stdout_early_does_not_make_the_cli_panic() {
    let mut ends = [0; 2];
    assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
    unsafe { libc::close(ends[0]) };
    let closed = unsafe { Stdio::from_raw_fd(ends[1]) };
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .args(["hooks", "describe", "--provider", "claude"])
        .env_clear()
        .stdout(closed)
        .stderr(Stdio::piped())
        .output()
        .expect("run with a closed stdout");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn printed_json_escapes_c1_control_characters_and_keeps_their_value() {
    let argument = "--unknown\u{9b}31m";
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .args(["bindings", argument])
        .env_clear()
        .output()
        .expect("run with a C1 character in an argument");
    assert!(
        !output.stdout.windows(2).any(|pair| pair == [0xc2, 0x9b]),
        "raw C1 reached stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let text = String::from_utf8(output.stdout).expect("UTF-8 JSON");
    assert!(text.contains("\\u009b"), "{text}");
    let envelope: Value = serde_json::from_str(&text).expect("still valid JSON");
    let message = envelope["diagnostics"][0]["message"]
        .as_str()
        .expect("message");
    assert!(message.contains(argument), "{message}");
}

#[test]
fn a_query_that_could_not_run_is_incomplete_and_exits_one() {
    let scratch = Scratch::new();
    let run = |args: &[&str], root: &str| {
        Command::new(env!("CARGO_BIN_EXE_attention"))
            .args(args)
            .env_clear()
            .env("HOME", &scratch.0)
            .env("WEZTERM_ATTENTION_DIR", root)
            .output()
            .expect("run query")
    };
    for (args, root) in [
        (vec!["tabs"], "relative/state"),
        (vec!["bindings", "--all"], "relative/state"),
        (
            vec!["bindings", "--socket", "/missing.sock"],
            "/unused/state",
        ),
    ] {
        let output = run(&args, root);
        let envelope: Value = serde_json::from_slice(&output.stdout).expect("error envelope");
        assert_eq!(envelope["complete"], false, "{args:?}: {envelope}");
        assert_eq!(output.status.code(), Some(1), "{args:?}: {envelope}");
    }
}

#[test]
fn doctor_findings_beside_a_complete_report_exit_zero() {
    let scratch = Scratch::new();
    let state = scratch.0.join("state");
    fs::create_dir_all(&state).expect("create state root");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).expect("shared state root");
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .args(["doctor", "--json"])
        .env_clear()
        .env("HOME", &scratch.0)
        .env("WEZTERM_ATTENTION_DIR", &state)
        .output()
        .expect("run doctor");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(envelope["status"], "findings", "{envelope}");
    assert_eq!(envelope["complete"], true, "{envelope}");
    assert_eq!(output.status.code(), Some(0), "{envelope}");
}

#[test]
fn a_hook_command_never_exits_two_and_says_why_on_stderr() {
    let scratch = Scratch::new();
    let run = |args: &[&str], stdin: &[u8]| {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_attention"))
            .args(args)
            .env_clear()
            .env("HOME", &scratch.0)
            .env("WEZTERM_ATTENTION_DIR", scratch.0.join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run hook command");
        let _ = child.stdin.take().expect("stdin").write_all(stdin);
        child.wait_with_output().expect("hook output")
    };
    let event = ["hooks", "event", "claude", "Stop"];
    let payload = br#"{"hook_event_name":"Stop","session_id":"s"}"#.as_slice();
    for (extra, stdin, label) in [
        (vec!["--no-such-flag"], payload, "unknown flag"),
        (vec!["--consumer", "relative"], payload, "relative consumer"),
        (
            vec!["--consumer", "/bin/true", "--consumer-timeout-ms", "0"],
            payload,
            "zero consumer timeout",
        ),
        (vec![], b"".as_slice(), "empty stdin"),
        (vec![], b"{".as_slice(), "invalid JSON"),
    ] {
        for strict in [false, true] {
            let mut args = event.to_vec();
            args.extend(&extra);
            if strict {
                args.push("--strict");
            }
            let output = run(&args, stdin);
            assert_eq!(
                output.status.code(),
                Some(i32::from(strict)),
                "{label}, strict={strict}: {output:?}"
            );
            assert!(output.stdout.is_empty(), "{label}: {output:?}");
            assert!(!output.stderr.is_empty(), "{label}: no reason given");
        }
    }
    for (args, expected) in [
        (vec!["hooks", "event", "claude"], 0),
        (vec!["hooks", "event", "claude", "--strict"], 1),
        (vec!["hooks", "evnet", "claude", "Stop"], 1),
        (vec!["hooks", "claim", "--no-such-flag"], 1),
        (vec!["hooks", "publish", "--json", "--quiet"], 1),
        (vec!["hooks", "describe", "--provider", "unsupported"], 1),
    ] {
        let output = run(&args, b"");
        assert_eq!(output.status.code(), Some(expected), "{args:?}: {output:?}");
    }
}

#[test]
fn doctor_that_had_nothing_to_look_at_says_unobserved_not_ok() {
    let scratch = Scratch::new();
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .args(["doctor", "--json"])
        .env_clear()
        .env("HOME", &scratch.0)
        .env("WEZTERM_ATTENTION_DIR", scratch.0.join("never-created"))
        .output()
        .expect("run doctor");
    assert_eq!(output.status.code(), Some(0));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(envelope["status"], "unobserved");
    assert_eq!(envelope["complete"], true);
}

#[test]
fn doctor_in_text_mode_gives_its_reasons_on_stderr() {
    let scratch = Scratch::new();
    let state = scratch.0.join("state");
    fs::create_dir_all(&state).expect("create state root");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).expect("shared state root");
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .arg("doctor")
        .env_clear()
        .env("HOME", &scratch.0)
        .env("WEZTERM_ATTENTION_DIR", &state)
        .output()
        .expect("run doctor");
    assert_eq!(output.stdout, b"findings\n");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "attention: state_permissions: state directory is accessible to other users\n"
    );
}
