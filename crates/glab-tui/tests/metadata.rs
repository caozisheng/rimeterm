use std::path::Path;

#[test]
fn crate_keeps_upstream_license_and_source_metadata() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let license = std::fs::read_to_string(root.join("LICENSE")).expect("MIT license");
    let upstream = std::fs::read_to_string(root.join("UPSTREAM.md")).expect("upstream note");
    assert!(license.contains("MIT License"));
    assert!(upstream.contains("c11c244a43d9cc1c71952ab887d09c9bba9476f3"));
}

#[test]
fn crate_keeps_the_full_upstream_module_surface() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/app.rs",
        "src/backend/glab.rs",
        "src/backend/gh.rs",
        "src/domain/issues.rs",
        "src/domain/mr.rs",
        "src/domain/pipelines.rs",
        "src/domain/notifications.rs",
        "src/handlers/tabs.rs",
        "src/ui/mod.rs",
        "src/ui/diff.rs",
        "tests/fixtures/issues.json",
        "tests/fixtures/mrs.json",
        "tests/fixtures/pipelines.json",
    ] {
        assert!(
            root.join(relative).is_file(),
            "missing upstream file {relative}"
        );
    }
}

#[test]
fn manifest_exposes_only_a_library_target() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("manifest");
    assert!(manifest.contains("[lib]"));
    assert!(!manifest.contains("[[bin]]"));
}
