# Repository instructions

## Commits

- Follow Conventional Commits for every commit subject.
- Use `<type>(<optional-scope>): <description>`.
- Allowed types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`.
- Keep the complete commit header ≤ 72 characters.
- `commitlint.config.mjs` (under `crates/fast-resume/`) enforces the rules mechanically for that subtree; the same shape is the workspace-wide convention — check the recent `git log` for canonical examples.
- PR titles follow the same rules so squash-merged commit titles stay consistent. `feat(pi): add Pi session support (#71)` — not `Add Pi session support (#71)`.

## Versioning

The **workspace version is the single source of truth**. Everything that
reports a version reads it — the Upgrade dialog's *Current version* line,
`updater::select_release_for` (decides whether a GitHub tag is newer), the
`.deb` metadata, `cargo-wix` MSI, `cargo-pkg` payload. All of these bake in
`env!("CARGO_PKG_VERSION")` at compile time, which resolves from
`[workspace.package].version` in the root `Cargo.toml`.

### The rule

Bump the workspace version **in a commit, before the tag exists**. Never let
a `vX.Y.Z` tag point at a tree whose `Cargo.toml` says something else.

Historical drift (v0.2.12..v0.2.19 all shipped as `0.2.11`) came from
tagging without bumping and papering over it with a CI `sed`. The CI check
below now refuses to build a tag whose workspace version doesn't match, so
this class of bug cannot come back silently.

### How to release

```bash
node scripts/bump-version.mjs patch          # 0.2.19 -> 0.2.20
# or `minor` / `major` / an explicit `0.2.20`

git add Cargo.toml Cargo.lock
git commit -m "chore(release): 0.2.20"
git tag v0.2.20
git push && git push --tags
```

`scripts/bump-version.mjs` (dependency-free Node, no `package.json` needed):

- Updates `[workspace.package].version` in `Cargo.toml`.
- Syncs the 11 workspace-versioned entries in `Cargo.lock` so
  `cargo build --locked` still resolves.
- Leaves independent crates alone — `fast-resume` (PyPI, `2.x`) and
  `tuxedo` (`2026.x`) have their own release cadence.
- Leaves `[workspace.dependencies]` caret ranges alone (`version = "0.2"`).
  On a major bump the script warns and you MUST bump those ranges too.
- `--check` verifies workspace and Cargo.lock agree; used by CI.
- `--dry-run` previews without writing.

### What's forbidden

- Editing `[workspace.package].version` by hand or with `sed` in a commit.
  Use the script — it also fixes `Cargo.lock`.
- Bumping any workspace crate's version out of band. All eleven
  `rimeterm*` + `rimectl` crates share the workspace version via
  `version.workspace = true`; they move as a unit.
- Committing a `Cargo.toml` version that doesn't match `Cargo.lock`.
  `node scripts/bump-version.mjs --check` catches this and CI runs the
  same check on every tag build.

### `fast-resume` and `tuxedo`

Independent release trains. Bump their `Cargo.toml` (and, for
`fast-resume`, `pyproject.toml`) manually, keep the versions in sync
between the two files, and — for `fast-resume` — remember it also ships to
PyPI via the maturin build.

## Toolchain

Rust toolchain is pinned in `rust-toolchain.toml` (currently `1.90`). The
release workflow pins the same version. Bump both files together —
`rust-toolchain.toml` for developers, `.github/workflows/release.yml` for
CI runners.
