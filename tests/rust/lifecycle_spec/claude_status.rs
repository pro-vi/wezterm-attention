//! Claude Code's per-process status file, as `claude_status` reads it. The
//! fixtures are reduced copies of files Claude Code 2.1.300 wrote, with their
//! values replaced; each keeps fields the reader does not read.

use super::*;
use wezterm_attention::claude_status::{Foreign, Reading, Unreadable, read, status_path};

const SESSION: &str = "s";

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude_status")
        .join(format!("{name}.json"));
    serde_json::from_slice(&fs::read(&path).expect("fixture")).expect("fixture JSON")
}

/// Read `text` as the status file of session "s" in process `AGENT_PID`.
fn read_text(text: &[u8]) -> Reading {
    let scratch = Scratch::new();
    let path = scratch.0.join(format!("{AGENT_PID}.json"));
    fs::write(&path, text).expect("status file");
    read(&path, SESSION, AGENT_PID)
}

fn read_value(value: &Value) -> Reading {
    read_text(&serde_json::to_vec(value).expect("status JSON"))
}

/// The `busy` fixture with `field` set to `value`, or removed for `None`.
fn busy_with(field: &str, value: Option<Value>) -> Value {
    let mut file = fixture("busy");
    match value {
        Some(value) => file[field] = value,
        None => {
            file.as_object_mut().expect("object").remove(field);
        }
    }
    file
}

#[test]
fn each_state_claude_code_writes_maps_to_its_reading() {
    let version = Some("2.1.300".to_owned());
    for (name, expected) in [
        (
            "busy",
            Reading::Working {
                updated_at: 1792023000100,
            },
        ),
        (
            "waiting-permission",
            Reading::Working {
                updated_at: 1792023000200,
            },
        ),
        (
            "waiting-dialog",
            Reading::Working {
                updated_at: 1792023000300,
            },
        ),
        (
            "idle",
            Reading::Ended {
                updated_at: 1792023000400,
                version: version.clone(),
            },
        ),
        (
            "shell",
            Reading::Ended {
                updated_at: 1792023000500,
                version: version.clone(),
            },
        ),
    ] {
        assert_eq!(read_value(&fixture(name)), expected, "{name}");
    }
}

#[test]
fn fields_the_reader_does_not_read_change_nothing() {
    let mut idle = fixture("idle");
    idle["somethingNew"] = json!({"nested": [1, 2]});
    idle["waitingFor"] = json!("permission prompt");
    assert!(matches!(read_value(&idle), Reading::Ended { .. }));
    for waiting_for in [None, Some(json!("anything at all")), Some(json!(7))] {
        let mut waiting = fixture("waiting-permission");
        match waiting_for {
            Some(value) => waiting["waitingFor"] = value,
            None => {
                waiting
                    .as_object_mut()
                    .expect("object")
                    .remove("waitingFor");
            }
        }
        assert!(
            matches!(read_value(&waiting), Reading::Working { .. }),
            "{waiting}"
        );
    }
}

#[test]
fn unknown_claude_status_is_unreadable() {
    assert_eq!(
        read_value(&busy_with("status", Some(json!("paused")))),
        Reading::Unreadable {
            reason: Unreadable::UnknownState,
            version: Some("2.1.300".to_owned()),
        }
    );
}

#[test]
fn a_file_the_reader_cannot_understand_is_unreadable() {
    let full = serde_json::to_vec(&fixture("idle")).expect("JSON");
    for (name, text, reason) in [
        (
            "status missing",
            busy_with("status", None),
            Unreadable::MissingField,
        ),
        (
            "statusUpdatedAt a string",
            busy_with("statusUpdatedAt", Some(json!("1792023000100"))),
            Unreadable::MissingField,
        ),
        (
            "pid not an integer",
            busy_with("pid", Some(json!(4000.5))),
            Unreadable::MissingField,
        ),
        (
            "sessionId missing",
            busy_with("sessionId", None),
            Unreadable::MissingField,
        ),
    ] {
        assert!(
            matches!(read_value(&text), Reading::Unreadable { reason: found, .. } if found == reason),
            "{name}"
        );
    }
    assert_eq!(
        read_text(&full[..full.len() / 2]),
        Reading::Unreadable {
            reason: Unreadable::NotJson,
            version: None,
        },
        "a truncated file"
    );
    assert!(matches!(
        read_text(b"[1,2]"),
        Reading::Unreadable {
            reason: Unreadable::NotJson,
            ..
        }
    ));
    let mut padded = fixture("idle");
    padded["padding"] = json!(" ".repeat(64 << 10));
    assert!(matches!(
        read_value(&padded),
        Reading::Unreadable {
            reason: Unreadable::TooLarge,
            ..
        }
    ));
    // A version that is not a safe label is left out, and the reading kept.
    assert_eq!(
        read_value(&busy_with("version", Some(json!("2.1\u{7}")))).clone(),
        Reading::Working {
            updated_at: 1792023000100
        }
    );
    let mut idle = fixture("idle");
    idle["version"] = json!(2);
    assert_eq!(
        read_value(&idle),
        Reading::Ended {
            updated_at: 1792023000400,
            version: None,
        }
    );
}

#[test]
fn another_session_s_or_process_s_file_is_foreign() {
    assert_eq!(
        read_value(&busy_with("sessionId", Some(json!("other")))),
        Reading::Foreign(Foreign::OtherSession)
    );
    assert_eq!(
        read_value(&busy_with("pid", Some(json!(AGENT_PID + 1)))),
        Reading::Foreign(Foreign::OtherProcess)
    );
    let scratch = Scratch::new();
    let real = scratch.0.join("real.json");
    fs::write(&real, serde_json::to_vec(&fixture("busy")).unwrap()).unwrap();
    let link = scratch.0.join(format!("{AGENT_PID}.json"));
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(
        read(&link, SESSION, AGENT_PID),
        Reading::Foreign(Foreign::NotOpened)
    );
    let directory = scratch.0.join("directory.json");
    fs::create_dir(&directory).unwrap();
    assert_eq!(
        read(&directory, SESSION, AGENT_PID),
        Reading::Foreign(Foreign::NotRegularFile)
    );
    // A FIFO with no writer is refused at once, not held open until one comes.
    let fifo = scratch.0.join("fifo.json");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `name` is a live NUL-terminated path for the call to read.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (sent, received) = std::sync::mpsc::channel();
    let reader = fifo.clone();
    thread::spawn(move || sent.send(read(&reader, SESSION, AGENT_PID)));
    let refused = received.recv_timeout(Duration::from_secs(5));
    if refused.is_err() {
        // Release the blocked open before failing.
        let _ = fs::OpenOptions::new().write(true).open(&fifo);
    }
    assert_eq!(
        refused.ok(),
        Some(Reading::Foreign(Foreign::NotRegularFile))
    );
    // A file another user owns needs root to make; /etc/hosts is root's.
    assert_eq!(
        read(std::path::Path::new("/etc/hosts"), SESSION, AGENT_PID),
        Reading::Foreign(Foreign::NotOwned)
    );
}

#[test]
fn no_file_is_gone() {
    let scratch = Scratch::new();
    assert_eq!(
        read(&scratch.0.join("4000.json"), SESSION, AGENT_PID),
        Reading::Gone
    );
    assert_eq!(
        read(
            &scratch.0.join("missing/sessions/4000.json"),
            SESSION,
            AGENT_PID
        ),
        Reading::Gone
    );
}

#[test]
fn the_file_is_the_process_s_in_the_configuration_directory() {
    assert_eq!(
        status_path(None, "/Users/u", 4000),
        PathBuf::from("/Users/u/.claude/sessions/4000.json")
    );
    assert_eq!(
        status_path(Some("/Users/u/.claude2"), "/Users/u", 4000),
        PathBuf::from("/Users/u/.claude2/sessions/4000.json")
    );
}
