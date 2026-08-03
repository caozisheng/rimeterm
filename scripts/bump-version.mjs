#!/usr/bin/env node
// scripts/bump-version.mjs — single source of truth for the workspace version.
//
// Why this exists: `env!("CARGO_PKG_VERSION")` bakes in from
// [workspace.package].version at compile time. It's read by the Upgrade
// dialog's "Current version" line, by updater::select_release_for to decide
// which GitHub tag is newer, and by cargo-deb for the .deb version stamp.
// From v0.2.12..v0.2.19 that field sat at 0.2.11 while release tags kept
// moving, so every locally-built binary reported itself as 0.2.11 and the
// upgrade modal permanently flagged "new version available". The release
// workflow used to patch it in-flight with `sed`; that only fixed CI
// binaries and left local builds broken.
//
// This script fixes the drift at the source. Run it before tagging:
//
//   node scripts/bump-version.mjs 0.2.20        # explicit
//   node scripts/bump-version.mjs patch         # 0.2.19 -> 0.2.20
//   node scripts/bump-version.mjs minor         # 0.2.19 -> 0.3.0
//   node scripts/bump-version.mjs major         # 0.2.19 -> 1.0.0
//   node scripts/bump-version.mjs --dry-run 0.2.20
//   node scripts/bump-version.mjs --check       # verify workspace == Cargo.lock
//
// Then commit both Cargo.toml + Cargo.lock, tag `v<version>`, push.
//
// What it touches:
//   * [workspace.package].version in ./Cargo.toml
//   * The rimeterm* + rimectl package entries in ./Cargo.lock
//     (these mirror version.workspace = true and MUST stay in lockstep,
//     otherwise `cargo build --locked` fails)
//
// What it deliberately leaves alone:
//   * crates/fast-resume/Cargo.toml + pyproject.toml (independent release
//     train — PyPI package versioned 2.x)
//   * crates/tuxedo/Cargo.toml (independent release train — 2026.x)
//   * [workspace.dependencies] version keys (caret ranges like "0.2"; only
//     need touching on a MAJOR bump — script warns instead of guessing)

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CARGO_TOML = resolve(REPO_ROOT, "Cargo.toml");
const CARGO_LOCK = resolve(REPO_ROOT, "Cargo.lock");

// Workspace members whose Cargo.toml uses `version.workspace = true` and
// therefore share the workspace version. Kept explicit (not auto-detected)
// so an accidental `version.workspace = true` in a future independent crate
// still requires a conscious edit here.
const WORKSPACE_VERSIONED_CRATES = [
  "rimeterm",
  "rimectl",
  "rimeterm-config",
  "rimeterm-core",
  "rimeterm-ipc",
  "rimeterm-markdown",
  "rimeterm-models",
  "rimeterm-pty",
  "rimeterm-stock",
  "rimeterm-tui",
  "rimeterm-zones",
];

// SemVer x.y.z with optional -prerelease. Matches what the release
// workflow accepts and what cargo/WiX/pkgbuild all agree parses.
const SEMVER_RE = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.\-]+))?$/;

function die(msg) {
  console.error(`bump-version: ${msg}`);
  process.exit(1);
}

function parseArgs(argv) {
  const args = { dryRun: false, check: false, target: null };
  for (const a of argv) {
    if (a === "--dry-run" || a === "-n") args.dryRun = true;
    else if (a === "--check") args.check = true;
    else if (a === "--help" || a === "-h") {
      console.log(
        [
          "Usage: node scripts/bump-version.mjs [--dry-run] <version|patch|minor|major>",
          "       node scripts/bump-version.mjs --check",
          "",
          "Bumps [workspace.package].version in Cargo.toml and syncs Cargo.lock",
          "for every crate that declares `version.workspace = true`.",
          "",
          "Independent crates (fast-resume, tuxedo) are left untouched.",
        ].join("\n"),
      );
      process.exit(0);
    } else if (a.startsWith("--")) die(`unknown flag ${a}`);
    else if (args.target !== null) die(`unexpected extra arg ${a}`);
    else args.target = a;
  }
  if (!args.check && args.target === null)
    die("missing target — pass a version, `patch`, `minor`, `major`, or `--check`");
  return args;
}

function readWorkspaceVersion(cargoToml) {
  // Match `version = "..."` inside the [workspace.package] section only.
  // Non-greedy up to the next top-level section header.
  const m = cargoToml.match(
    /\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
  );
  if (!m) die("cannot find [workspace.package].version in Cargo.toml");
  return m[1];
}

function bump(current, kind) {
  const m = current.match(SEMVER_RE);
  if (!m) die(`current version "${current}" is not semver`);
  let [, major, minor, patch] = m;
  [major, minor, patch] = [major, minor, patch].map(Number);
  switch (kind) {
    case "patch":
      return `${major}.${minor}.${patch + 1}`;
    case "minor":
      return `${major}.${minor + 1}.0`;
    case "major":
      return `${major + 1}.0.0`;
    default:
      die(`unknown bump kind "${kind}"`);
  }
}

function resolveTarget(current, arg) {
  if (["patch", "minor", "major"].includes(arg)) return bump(current, arg);
  if (!SEMVER_RE.test(arg))
    die(`"${arg}" is not MAJOR.MINOR.PATCH[-prerelease] and not a bump keyword`);
  return arg;
}

function updateCargoToml(text, next) {
  // Replace ONLY the version line inside [workspace.package]. Using a
  // section-scoped regex so [workspace.dependencies] entries like
  // `rimeterm-core = { path = "...", version = "0.2" }` stay untouched.
  const re =
    /(\[workspace\.package\][\s\S]*?^version\s*=\s*")([^"]+)(")/m;
  if (!re.test(text))
    die("Cargo.toml layout unexpected — regex miss on [workspace.package]");
  return text.replace(re, `$1${next}$3`);
}

function updateCargoLock(text, next, crates) {
  // Each workspace crate entry in Cargo.lock looks like:
  //   [[package]]
  //   name = "rimeterm-tui"
  //   version = "0.2.11"
  // We rewrite the `version = "..."` line that immediately follows the
  // matching `name = "..."` line (there's always exactly one version line
  // between name and dependencies for workspace members).
  let out = text;
  const missing = [];
  for (const crate of crates) {
    const re = new RegExp(
      `(^name\\s*=\\s*"${crate.replace(/[-]/g, "\\-")}"\\s*\\n\\s*version\\s*=\\s*")([^"]+)(")`,
      "m",
    );
    if (!re.test(out)) {
      missing.push(crate);
      continue;
    }
    out = out.replace(re, `$1${next}$3`);
  }
  if (missing.length)
    die(
      `Cargo.lock entries missing for: ${missing.join(", ")} (run \`cargo update -p ${missing[0]}\` first)`,
    );
  return out;
}

function readLockVersions(text, crates) {
  const versions = {};
  for (const crate of crates) {
    const re = new RegExp(
      `^name\\s*=\\s*"${crate.replace(/[-]/g, "\\-")}"\\s*\\n\\s*version\\s*=\\s*"([^"]+)"`,
      "m",
    );
    const m = text.match(re);
    versions[crate] = m ? m[1] : null;
  }
  return versions;
}

function warnMajorBump(current, next) {
  const [cMaj, cMin] = current.match(SEMVER_RE).slice(1, 3).map(Number);
  const [nMaj, nMin] = next.match(SEMVER_RE).slice(1, 3).map(Number);
  if (nMaj !== cMaj || (cMaj === 0 && nMin !== cMin)) {
    // 0.x is treated as breaking on every minor per Cargo semver.
    console.warn(
      [
        "",
        `⚠  breaking bump ${current} -> ${next}`,
        "   [workspace.dependencies] internal path-deps still pin the old range:",
        "   rimeterm-core = { path = \"...\", version = \"0.2\" }",
        "   Cargo won't resolve those against the new version — bump those",
        "   ranges in Cargo.toml too before the build succeeds.",
        "",
      ].join("\n"),
    );
  }
}

function main() {
  const args = parseArgs(process.argv.slice(2));

  const cargoTomlText = readFileSync(CARGO_TOML, "utf8");
  const cargoLockText = readFileSync(CARGO_LOCK, "utf8");
  const current = readWorkspaceVersion(cargoTomlText);

  if (args.check) {
    // CI + local safety net: workspace version, every workspace-versioned
    // Cargo.lock entry MUST agree. Exit non-zero on any drift.
    const lockVersions = readLockVersions(cargoLockText, WORKSPACE_VERSIONED_CRATES);
    const drifted = Object.entries(lockVersions).filter(([, v]) => v !== current);
    if (drifted.length === 0) {
      console.log(`✓ workspace + Cargo.lock aligned at ${current}`);
      process.exit(0);
    }
    console.error(`✗ version drift detected (workspace: ${current}):`);
    for (const [c, v] of drifted) console.error(`  ${c}: Cargo.lock has ${v ?? "(missing)"}`);
    console.error("run `node scripts/bump-version.mjs " + current + "` to resync.");
    process.exit(1);
  }

  const next = resolveTarget(current, args.target);
  if (next === current) {
    console.log(`workspace already at ${current}, nothing to do`);
    process.exit(0);
  }

  warnMajorBump(current, next);

  const newToml = updateCargoToml(cargoTomlText, next);
  const newLock = updateCargoLock(cargoLockText, next, WORKSPACE_VERSIONED_CRATES);

  if (args.dryRun) {
    console.log(`[dry-run] Cargo.toml: ${current} -> ${next}`);
    console.log(
      `[dry-run] Cargo.lock: ${WORKSPACE_VERSIONED_CRATES.length} crate entries would flip to ${next}`,
    );
    process.exit(0);
  }

  writeFileSync(CARGO_TOML, newToml);
  writeFileSync(CARGO_LOCK, newLock);
  console.log(`✓ bumped workspace ${current} -> ${next}`);
  console.log(`✓ synced Cargo.lock for ${WORKSPACE_VERSIONED_CRATES.length} crates`);
  console.log("");
  console.log("next:");
  console.log(`  git add Cargo.toml Cargo.lock`);
  console.log(`  git commit -m \"chore(release): ${next}\"`);
  console.log(`  git tag v${next}`);
  console.log(`  git push && git push --tags`);
}

main();
