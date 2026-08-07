use std::path::Path;

fn run_git(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args).current_dir(root);
    if let Some((key, value)) = rimeterm_config::paths::augmented_path_env() {
        cmd.env(key, value);
    }
    cmd.output().ok()
}

pub(crate) fn get_current_branch(root: &Path) -> Option<String> {
    let output = run_git(root, &["symbolic-ref", "--short", "HEAD"])?;
    if output.status.success() {
        return Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    let output = run_git(root, &["branch", "--show-current"])?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Extracts the `namespace/project` path from a git remote URL.
///
/// Accepts `scheme://[user[:pass]@]host[:port]/namespace/project[.git]` and
/// scp-style `git@host:namespace/project[.git]`. Every segment after the host
/// is preserved, because GitLab namespaces can nest arbitrarily deep
/// (`group/subgroup/subsubgroup/project`).
///
/// Returns `None` when the URL has no parseable namespace.
pub(crate) fn parse_project_path(url: &str) -> Option<String> {
    let url = url.trim();
    // Drop everything up to and including the host, keeping the rest intact.
    // Splitting on "://" must be tried first, since those URLs also contain ':'.
    let path = if let Some((_scheme, rest)) = url.split_once("://") {
        rest.split_once('/')?.1
    } else if let Some((_host, rest)) = url.split_once(':') {
        rest
    } else {
        return None;
    };

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    path.contains('/').then(|| path.to_string())
}

pub(crate) fn slugify(s: &str) -> String {
    let mut slug = String::with_capacity(s.len());
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if c.is_ascii() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

pub(crate) fn get_default_branch(root: &Path) -> Option<String> {
    let output = run_git(root, &["rev-parse", "--abbrev-ref", "origin/HEAD"])?;
    if output.status.success() {
        return String::from_utf8_lossy(&output.stdout)
            .trim()
            .strip_prefix("origin/")
            .map(str::to_string);
    }
    None
}

pub(crate) fn get_branches(root: &Path) -> Vec<String> {
    let output = match run_git(root, &["branch", "-a"]) {
        Some(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .map(|s| {
            s.trim_start_matches("* ")
                .trim_start_matches("remotes/")
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Returns a list of workflow/CI files available in the repo.
/// For GitHub repos: scans `.github/workflows/*.yml` and `*.yaml`.
/// For GitLab repos: returns `.gitlab-ci.yml` if it exists, else empty.
pub(crate) fn get_workflow_files(root: &Path, is_github: bool) -> Vec<String> {
    if is_github {
        let dir = root.join(".github").join("workflows");
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        return entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let ext = path.extension().and_then(|e| e.to_str());
                if !path.is_file() || !matches!(ext, Some("yml") | Some("yaml")) {
                    return None;
                }
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
    }
    root.join(".gitlab-ci.yml")
        .is_file()
        .then(|| vec![".gitlab-ci.yml".to_string()])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::parse_project_path;

    #[test]
    fn keeps_nested_subgroups_over_https() {
        assert_eq!(
            parse_project_path("https://gitlab.example.com/dev/cbr/salesforce/salesforce.git")
                .as_deref(),
            Some("dev/cbr/salesforce/salesforce")
        );
    }

    #[test]
    fn keeps_nested_subgroups_over_scp_style_ssh() {
        assert_eq!(
            parse_project_path("git@gitlab.example.com:dev/cbr/salesforce/salesforce.git")
                .as_deref(),
            Some("dev/cbr/salesforce/salesforce")
        );
    }

    #[test]
    fn parses_single_namespace_https() {
        assert_eq!(
            parse_project_path("https://gitlab.com/group/repo.git").as_deref(),
            Some("group/repo")
        );
    }

    #[test]
    fn parses_ssh_scheme_with_port() {
        assert_eq!(
            parse_project_path("ssh://git@gitlab.example.com:2222/group/sub/repo.git").as_deref(),
            Some("group/sub/repo")
        );
    }

    #[test]
    fn parses_https_with_port() {
        assert_eq!(
            parse_project_path("https://gitlab.example.com:8443/group/sub/repo.git").as_deref(),
            Some("group/sub/repo")
        );
    }

    #[test]
    fn ignores_embedded_credentials() {
        assert_eq!(
            parse_project_path("https://user:token@gitlab.example.com/group/sub/repo.git")
                .as_deref(),
            Some("group/sub/repo")
        );
    }

    #[test]
    fn tolerates_missing_git_suffix_and_trailing_slash() {
        assert_eq!(
            parse_project_path("https://gitlab.example.com/group/sub/repo/").as_deref(),
            Some("group/sub/repo")
        );
    }

    #[test]
    fn preserves_project_names_containing_git() {
        assert_eq!(
            parse_project_path("https://gitlab.example.com/group/my.github.git").as_deref(),
            Some("group/my.github")
        );
    }

    #[test]
    fn rejects_urls_without_a_namespace() {
        assert_eq!(parse_project_path("https://gitlab.example.com/"), None);
        assert_eq!(parse_project_path("not-a-url"), None);
        assert_eq!(parse_project_path(""), None);
    }
}
