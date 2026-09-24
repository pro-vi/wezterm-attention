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
