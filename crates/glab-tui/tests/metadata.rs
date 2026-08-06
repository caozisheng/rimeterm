use std::path::Path;

#[test]
fn crate_keeps_upstream_license_and_source_metadata() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let license = std::fs::read_to_string(root.join("LICENSE")).expect("MIT license");
    let upstream = std::fs::read_to_string(root.join("UPSTREAM.md")).expect("upstream note");
    assert!(license.contains("MIT License"));
    assert!(upstream.contains("c11c244a43d9cc1c71952ab887d09c9bba9476f3"));
}
