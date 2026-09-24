//! A diagnostic from a query that walks many records names the record it is
//! about, relative to the state root, so a reader can find it without the
//! local home directory appearing in the output.

use super::*;
use wezterm_attention::query::read_tab_publications;

#[test]
fn a_realm_wide_bindings_diagnostic_names_its_record() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    let bad = setup
        .binding_dir()
        .parent()
        .expect("bindings directory")
        .join("f".repeat(64))
        .join("binding.json");
    fs::create_dir_all(bad.parent().expect("binding directory")).expect("create");
    fs::write(&bad, "not json").expect("write");
    fs::write(setup.binding_dir().join("end.json"), "not json").expect("write end");
    let (_, diagnostics) =
        read_bindings_with_ports(&root, Some(&setup.panes), Some(&setup.processes))
            .expect("bindings");
    let relative = |path: &Path| json!(path.strip_prefix(&root).unwrap().to_str().unwrap());
    for path in [bad.clone(), setup.binding_dir().join("end.json")] {
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "record_invalid"
                    && d.context.get("path") == Some(&relative(&path))),
            "no diagnostic names {}: {diagnostics:?}",
            path.display()
        );
    }
}

#[test]
fn a_tab_publication_diagnostic_names_its_file() {
    let setup = Setup::new();
    let root = setup.root();
    fs::create_dir_all(root.join("tabs")).expect("tabs");
    fs::write(root.join("tabs").join("9.json"), "not json").expect("write");
    fs::write(root.join("tabs").join("x.json"), "{}").expect("write");
    let (_, diagnostics) = read_tab_publications(&root).expect("tabs");
    let named: Vec<_> = diagnostics
        .iter()
        .map(|d| d.context.get("path").cloned())
        .collect();
    assert_eq!(named.len(), 2, "{diagnostics:?}");
    assert!(
        named.contains(&Some(json!("tabs/9.json"))),
        "{diagnostics:?}"
    );
    assert!(
        named.contains(&Some(json!("tabs/x.json"))),
        "{diagnostics:?}"
    );
}
