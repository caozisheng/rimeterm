pub(crate) fn editor_name() -> String {
    std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "helix".to_string())
}

/// Stub — Task 8 will replace this with a HostAction-based flow.
pub(crate) fn edit_in_editor(_current_val: &str) -> Option<String> {
    None
}

/// Stub — Task 8 will replace this with a HostAction-based flow.
pub(crate) fn edit_in_editor_with_suffix(_current_val: &str, _suffix: &str) -> Option<String> {
    None
}
