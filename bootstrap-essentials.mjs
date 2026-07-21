#!/usr/bin/env node
// Populate `target/{debug,release}/essentials/` from the pins at
// essentials/VERSIONS.toml so `cargo run` / `cargo run --release`
// gets a complete C21.5 first-launch experience.
//
// Release CI does the same work automatically (see
// .github/workflows/release.yml + .github/scripts/fetch-essentials.mjs).
// This script exists for local development only.
//
// Usage:
//   node bootstrap-essentials.mjs              # both debug + release
//   node bootstrap-essentials.mjs debug        # debug only
//   node bootstrap-essentials.mjs release      # release only

import fsSync from "node:fs";
import fs from "node:fs/promises";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = __dirname;
const fetchScript = path.join(repoRoot, ".github", "scripts", "fetch-essentials.mjs");

if (!fsSync.existsSync(fetchScript)) {
  console.error(`fetch-essentials.mjs not found at ${fetchScript}`);
  process.exit(2);
}

// Detect current target triple via `rustc -vV`.
const rustc = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
if (rustc.status !== 0) {
  console.error("cannot invoke rustc — is rustup on PATH?");
  console.error(rustc.stderr);
  process.exit(3);
}
const triple = rustc.stdout
  .split(/\r?\n/)
  .find((l) => l.startsWith("host:"))
  ?.split(/\s+/)[1];
if (!triple) {
  console.error("cannot resolve host target triple via rustc -vV");
  process.exit(3);
}
console.log(`host target: ${triple}`);

const mode = process.argv[2] ?? "both";
const dirs =
  mode === "debug"
    ? [path.join(repoRoot, "target", "debug")]
    : mode === "release"
      ? [path.join(repoRoot, "target", "release")]
      : mode === "both"
        ? [
            path.join(repoRoot, "target", "debug"),
            path.join(repoRoot, "target", "release"),
          ]
        : null;
if (!dirs) {
  console.error(`unknown mode: ${mode}. use debug|release|both`);
  process.exit(2);
}

for (const dir of dirs) {
  await fs.mkdir(dir, { recursive: true });
  const target = path.join(dir, "essentials");
  console.log(`→ populating ${target}`);
  await runFetch(triple, target);
}

console.log(
  "\ndone. ~/.rimeterm/bin/ will be populated on the next `cargo run` /"
);
console.log(
  "`cargo run --release`. Delete the fingerprint marker to force"
);
console.log("re-extraction:");
console.log(
  process.platform === "win32"
    ? "  Remove-Item $HOME/.rimeterm/bin/.rimeterm-essentials-version"
    : "  rm ~/.rimeterm/bin/.rimeterm-essentials-version"
);

function runFetch(triple, dest) {
  return new Promise((resolve, reject) => {
    const child = spawn(
      process.execPath,
      [fetchScript, triple, dest],
      { stdio: "inherit" }
    );
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) resolve();
      else reject(new Error(`fetch-essentials exited ${code}`));
    });
  });
}
