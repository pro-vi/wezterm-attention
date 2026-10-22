//! `bindings` names a state root that does not exist, with the variable that
//! chose it, so a program reading the wrong root does not take "nothing
//! there" for "no agents". It changes neither `complete` nor the exit code: a
//! fresh machine has no root either.

use super::*;

fn bindings(home: &Path, extra: &[(&str, &Path)]) -> (Value, Option<i32>) {
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .envs(extra.iter().copied())
        .args(["bindings", "--json"])
        .output()
        .expect("run bindings");
    let response = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    (response, output.status.code())
}

fn missing(response: &Value) -> Vec<&Value> {
    response["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "state_root_missing")
        .collect()
}

#[test]
fn bindings_names_a_missing_root_and_the_variable_that_chose_it() {
    let scratch = Scratch::new();
    let explicit = scratch.0.join("explicit");
    let xdg = scratch.0.join("xdg");
    for (variable, value, root) in [
        ("WEZTERM_ATTENTION_DIR", Some(&explicit), explicit.clone()),
        ("XDG_STATE_HOME", Some(&xdg), xdg.join("wezterm-attention")),
        (
            "HOME",
            None,
            scratch.0.join(".local/state/wezterm-attention"),
        ),
    ] {
        let extra: Vec<(&str, &Path)> = value
            .map(|path| (variable, path.as_path()))
            .into_iter()
            .collect();
        let (response, code) = bindings(&scratch.0, &extra);
        assert_eq!(response["status"], "findings", "{variable}: {response}");
        assert_eq!(response["complete"], true, "{variable}: {response}");
        assert_eq!(code, Some(0), "{variable}");
        let found = missing(&response);
        assert_eq!(found.len(), 1, "{variable}: {response}");
        assert_eq!(
            found[0]["context"]["state_root"],
            root.display().to_string()
        );
        assert_eq!(found[0]["context"]["variable"], variable);
    }
}

#[test]
fn an_existing_root_with_nothing_in_it_answers_ok() {
    let scratch = Scratch::new();
    let root = scratch.0.join("explicit");
    fs::create_dir_all(&root).expect("root");
    let (response, code) = bindings(&scratch.0, &[("WEZTERM_ATTENTION_DIR", &root)]);
    assert_eq!(response["status"], "ok", "{response}");
    assert!(missing(&response).is_empty());
    assert_eq!(code, Some(0));
}
