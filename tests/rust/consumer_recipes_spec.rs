use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn public_checkpoint_inspector_and_reply_sink_recipes_execute() {
    let mut setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "recipe",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let fake = setup._scratch.0.join("wezterm");
    fs::write(
        &fake,
        "#!/bin/sh\nprintf '%s\\n' '[{\"pane_id\":\"42\",\"tab_id\":7}]'\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    setup
        .env
        .insert("WEZTERM_EXECUTABLE".into(), fake.to_str().unwrap().into());
    setup
        .env
        .insert("PATH".into(), "/opt/homebrew/bin:/usr/bin:/bin".into());
    // Put the synthetic executable first, so production resolution cannot reach
    // a user server even if a PATH-installed wezterm exists.
    setup.env.insert(
        "PATH".into(),
        format!(
            "{}:/opt/homebrew/bin:/usr/bin:/bin",
            setup._scratch.0.display()
        ),
    );
    let bindings = rust_command(&setup)
        .args([
            "bindings",
            "--socket",
            &setup.env["WEZTERM_UNIX_SOCKET"],
            "--json",
        ])
        .output()
        .unwrap();
    assert!(bindings.status.success());
    let bindings: Value = serde_json::from_slice(&bindings.stdout).unwrap();
    let row = bindings["result"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["current"] == true)
        .unwrap();
    let scope = json!({"address":row["address"],"launch_id":row["launch_id"],"binding_id":row["binding_id"]});
    let mut child = rust_command(&setup)
        .args(["inspect", "--scope", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(scope.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let inspect: Value = serde_json::from_slice(&output.stdout).unwrap();
    let input = setup._scratch.0.join("public-envelopes.json");
    fs::write(
        &input,
        json!({"bindings":bindings,"inspect":inspect}).to_string(),
    )
    .unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("/opt/homebrew/bin/node")
        .env_clear()
        .envs(&setup.env)
        .arg(root.join("tests/consumer_recipes.mjs"))
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
    let checkpoint = setup._scratch.0.join("checkpoint.json");
    let output = Command::new("/opt/homebrew/bin/node")
        .env_clear()
        .envs(&setup.env)
        .arg(root.join("examples/checkpoint.mjs"))
        .arg(env!("CARGO_BIN_EXE_attention"))
        .arg(&setup.env["WEZTERM_UNIX_SOCKET"])
        .arg(&fake)
        .arg(&checkpoint)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    println!(
        "Real CLI checkpoint recipe: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let output = Command::new("/opt/homebrew/bin/node")
        .env_clear()
        .envs(&setup.env)
        .arg(root.join("examples/inspect.mjs"))
        .arg(env!("CARGO_BIN_EXE_attention"))
        .arg(&setup.env["WEZTERM_UNIX_SOCKET"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["panes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let reply = setup._scratch.0.join("reply.json");
    setup.env.insert(
        "ATTENTION_REPLY_FILE".into(),
        reply.to_str().unwrap().into(),
    );
    let mut child = rust_command(&setup)
        .args([
            "hooks",
            "event",
            "claude",
            "Stop",
            "--consumer",
            root.join("examples/reply-sink.mjs").to_str().unwrap(),
            "--include-reply",
            "--consumer-timeout-ms",
            "2000",
            "--strict",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let text = "SYNTHETIC RECIPE\n中文\n";
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({"hook_event_name":"Stop","session_id":"recipe","last_assistant_message":text})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let stored: Value = serde_json::from_slice(&fs::read(&reply).unwrap()).unwrap();
    assert_eq!(stored["text"], text);
    assert_eq!(stored["scope"]["launch_id"], scope["launch_id"]);
    assert_eq!(
        fs::metadata(reply).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
