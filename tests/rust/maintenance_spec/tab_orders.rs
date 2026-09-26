use super::*;
use wezterm_attention::query::read_tab_publications;

fn write_tab_text(root: &Path, window_id: u64, text: &str) -> PathBuf {
    let tabs = root.join("tabs");
    fs::create_dir_all(&tabs).expect("create tabs directory");
    let path = tabs.join(format!("{window_id}.json"));
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "published_at_ms": 1, "schema": 1, "window_id": window_id,
            "tabs": [{"marker_ids": ["17"], "number": 1, "text": text}]
        }))
        .expect("tab order JSON"),
    )
    .expect("write tab order");
    path
}

/// Tab text is drawn into a terminal by whoever reads it back, so a C1 control
/// such as U+009B (a one-character CSI) is as unsafe as ESC. Every character
/// Rust calls a control is refused, not only the C0 range.
#[test]
fn tab_text_with_any_control_character_is_refused() {
    let scratch = Scratch::new();
    let root = scratch.0.join("state");
    for (window_id, text) in [
        (1, "plain"),
        (2, "csi \u{9b}31m"),
        (3, "next line \u{85}"),
        (4, "escape \u{1b}[31m"),
        (5, "delete \u{7f}"),
    ] {
        write_tab_text(&root, window_id, text);
    }
    let (windows, diagnostics) = read_tab_publications(&root).expect("tab publications");
    assert_eq!(
        windows.iter().map(|w| w.window_id).collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(diagnostics.len(), 4, "{diagnostics:?}");
    assert!(diagnostics.iter().all(|d| d.code == "record_invalid"));
}

/// A tab order names its GUI source only from schema 2 on, and names it in
/// full. A `"source": null` is a source that is not one: refused in a schema
/// 1 file as in a schema 2 one, rather than read as no source.
#[test]
fn a_null_tab_source_is_refused_in_either_schema() {
    let scratch = Scratch::new();
    let root = scratch.0.join("state");
    let tabs = root.join("tabs");
    fs::create_dir_all(&tabs).expect("create tabs directory");
    let incarnation = "b".repeat(64);
    for (name, schema) in [("7".to_owned(), 1), (format!("{incarnation}-8"), 2)] {
        let window_id: u64 = name.rsplit('-').next().unwrap().parse().unwrap();
        fs::write(
            tabs.join(format!("{name}.json")),
            serde_json::to_vec(&json!({
                "schema": schema, "window_id": window_id, "published_at_ms": 1,
                "tabs": [], "source": null
            }))
            .expect("tab order JSON"),
        )
        .expect("write tab order");
    }
    write_tab_text(&root, 9, "plain");
    let (windows, diagnostics) = read_tab_publications(&root).expect("tab publications");
    assert_eq!(
        windows.iter().map(|w| w.window_id).collect::<Vec<_>>(),
        vec![9]
    );
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert!(diagnostics.iter().all(|d| d.code == "record_invalid"));
}

/// A `tabs/` that is a symlink points the tab-order collection at a directory
/// outside the state root. Nothing there is read, and nothing is deleted.
#[test]
fn a_symlinked_tabs_directory_is_refused_and_nothing_behind_it_is_deleted() {
    let setup = Setup::new();
    let root = setup.root();
    fs::create_dir_all(&root).expect("create state root");
    let outside = setup._scratch.0.join("outside-tabs");
    // An empty order is one sweep would collect, were it inside the root.
    let planted = write_tab_order(&outside, 7, &[]);
    let planted = outside.join(planted.file_name().expect("tab order name"));
    fs::rename(outside.join("tabs").join("7.json"), &planted).expect("flatten");
    symlink(&outside, root.join("tabs")).expect("link tabs outside the state root");

    let (result, diagnostics) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000901"));
    assert!(planted.exists(), "a file outside the state root survives");
    assert!(
        result
            .details
            .iter()
            .all(|detail| detail["kind"] != "tab_order_collection")
    );
    assert!(diagnostics.iter().any(|d| d.code == "record_invalid"));

    let error = read_tab_publications(&root).expect_err("a symlinked tabs directory");
    assert_eq!(error.diagnostic.code, "record_invalid");
}

/// Lists the fixture's pane, and rewrites one tab order the first time it is
/// asked, the way a live GUI republishes while a sweep is deciding.
struct RepublishingPanes {
    path: PathBuf,
    republished: AtomicU8,
}

impl PaneLister for RepublishingPanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        if self.republished.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut order: Value =
                serde_json::from_slice(&fs::read(&self.path).expect("tab order")).expect("JSON");
            order["published_at_ms"] = json!(123_456_789);
            fs::write(&self.path, serde_json::to_vec(&order).expect("JSON")).expect("republish");
        }
        Ok(vec![PaneRow {
            pane_id: "42".to_owned(),
            tty_name: Some("/dev/ttys888".to_owned()),
        }])
    }
}

/// The decision to collect a tab order is made from the file as it was read.
/// A file that changed while the panes were probed is a newer draw, and it is
/// kept rather than deleted on the old file's evidence.
#[test]
fn a_tab_order_rewritten_during_the_decision_is_kept() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.processes.set(Presence::Absent);
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let gone = format!("v2:{}:{}:43", address.realm_id, address.incarnation_id);
    let path = write_tab_order(&root, 7, &[&gone]);
    let panes = RepublishingPanes {
        path: path.clone(),
        republished: AtomicU8::new(0),
    };
    let (result, _) = sweep(
        &root,
        None,
        true,
        Some("00000000-0000-4000-8000-000000000902"),
        &setup.clock,
        &panes,
        Some(&setup.processes),
    )
    .expect("sweep");
    assert!(path.exists(), "the republished order survives");
    let order: Value = serde_json::from_slice(&fs::read(&path).expect("tab order")).expect("JSON");
    assert_eq!(order["published_at_ms"], 123_456_789);
    let detail = result
        .details
        .iter()
        .find(|detail| detail["kind"] == "tab_order_collection")
        .expect("tab order detail");
    assert_eq!(detail["action"], "keep");
    assert_eq!(detail["reason"], "changed");
}

/// A tab order naming a pane whose socket is gone is kept, as one naming an
/// unanswered pane is, and the gone socket is reported with the rest of the
/// kept history, by the incarnation that holds the pane.
#[test]
fn a_tab_order_naming_a_pane_of_a_gone_socket_is_kept_and_named() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let marker = format!("v2:{}:{}:42", address.realm_id, address.incarnation_id);
    let path = write_tab_order(&root, 5, &[&marker]);
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    for (apply, operation) in [
        (false, None),
        (true, Some("00000000-0000-4000-8000-000000000903")),
    ] {
        let (result, diagnostics) = setup.run_sweep(apply, operation);
        assert!(path.exists());
        let detail = result
            .details
            .iter()
            .find(|detail| detail["kind"] == "tab_order_collection")
            .expect("tab order detail");
        assert_eq!(detail["action"], "keep");
        assert!(
            !diagnostics.iter().any(|d| d.code == "probe_unavailable"),
            "{diagnostics:?}"
        );
        let gone: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.code == "socket_gone")
            .collect();
        assert_eq!(gone.len(), 1, "{diagnostics:?}");
        assert_eq!(
            gone[0].context.get("incarnations"),
            Some(&json!([{
                "realm_id": address.realm_id,
                "incarnation_id": address.incarnation_id,
                "path": format!("v2/realms/{}/incarnations/{}", address.realm_id, address.incarnation_id),
                "pane_count": 1,
            }])),
            "{diagnostics:?}"
        );
    }
}
