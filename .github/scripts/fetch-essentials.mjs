#!/usr/bin/env node
// Fetch prebuilt essentials binaries into a staging dir for the C21.5
// release pipeline.
//
// Usage: node fetch-essentials.mjs <target-triple> <dest-dir>
//
// Reads pins from essentials/VERSIONS.toml (at repo root) and downloads
// each tool's platform-specific archive from GitHub Releases, verifies
// SHA-256 when a hash is pinned (bootstrap-friendly: empty hash accepts
// and records the download), extracts declared binaries into <dest-dir>.
//
// The script exits non-zero on:
// - unknown target triple
// - HTTP fetch failure
// - archive corruption
// - SHA-256 mismatch against a pinned hash
//
// On success it also copies VERSIONS.toml into <dest-dir> so runtime
// tests (rimeterm-release-check) can verify pins.

import fs from "node:fs/promises";
import fsSync from "node:fs";
import path from "node:path";
import os from "node:os";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

async function main() {
  const [triple, dest] = process.argv.slice(2);
  if (!triple || !dest) {
    console.error("usage: node fetch-essentials.mjs <target-triple> <dest-dir>");
    process.exit(2);
  }

  await fs.mkdir(dest, { recursive: true });

  const repoRoot = path.resolve(__dirname, "..", "..");
  const versionsPath = path.join(repoRoot, "essentials", "VERSIONS.toml");
  if (!fsSync.existsSync(versionsPath)) {
    console.error(`essentials/VERSIONS.toml not found at ${versionsPath}`);
    process.exit(2);
  }
  const pins = parseToml(await fs.readFile(versionsPath, "utf8"));

  for (const tool of ["yazi", "gitui", "bottom"]) {
    await fetchOneTool(tool, pins[tool], triple, dest);
  }

  // Copy VERSIONS.toml sidecar for the runtime golden test.
  await fs.copyFile(versionsPath, path.join(dest, "VERSIONS.toml"));

  console.log(`\nessentials staged to ${dest}:`);
  for (const entry of await fs.readdir(dest)) {
    const stat = await fs.stat(path.join(dest, entry));
    console.log(`  ${entry}  (${stat.size} bytes)`);
  }
}

async function fetchOneTool(tool, pin, triple, dest) {
  if (!pin) throw new Error(`no [${tool}] section in VERSIONS.toml`);
  const ext = pin.ext_per_target?.[triple];
  if (!ext) {
    console.error(`no ext_per_target entry for ${tool} → ${triple}`);
    process.exit(3);
  }
  const asset = pin.assets_per_target?.[triple];
  if (!asset) {
    console.error(`no assets_per_target entry for ${tool} → ${triple}`);
    process.exit(3);
  }
  const tagPrefix = pin.tag_prefix ?? "v";
  const url = `https://github.com/${pin.repo}/releases/download/${tagPrefix}${pin.version}/${asset}`;
  console.log(`== ${tool} ${tagPrefix}${pin.version} → ${asset}`);

  const tmpBase = await fs.mkdtemp(path.join(os.tmpdir(), `rimeterm-fetch-${tool}-`));
  try {
    const archive = path.join(tmpBase, asset);
    await download(url, archive);

    // SHA-256 verification.
    const actual = await sha256(archive);
    const pinned = pin.sha256?.[asset];
    if (pinned && pinned !== actual) {
      console.error(`SHA-256 mismatch for ${asset}: expected ${pinned}, got ${actual}`);
      process.exit(4);
    }
    console.log(`  sha256=${actual}`);

    const extracted = path.join(tmpBase, "x");
    await fs.mkdir(extracted, { recursive: true });
    await extract(archive, extracted, ext);

    for (const binary of pin.binaries?.files ?? []) {
      const targetName = triple.includes("windows") ? `${binary}.exe` : binary;
      const found = await findFile(extracted, targetName);
      if (!found) {
        console.error(`binary ${targetName} not found inside ${asset}`);
        process.exit(6);
      }
      const outPath = path.join(dest, targetName);
      await fs.copyFile(found, outPath);
      if (process.platform !== "win32") {
        await fs.chmod(outPath, 0o755);
      }
      console.log(`  copied ${targetName}`);
    }
  } finally {
    await fs.rm(tmpBase, { recursive: true, force: true });
  }
}

/// Per-download hard timeout (ms). Sized for GitHub Releases + CDN
/// redirect on the largest asset (yazi ~33 MB) over a modest home
/// connection — 180s handles ~200 KB/s worst case. Anything worse
/// than that is a genuine network problem and should fail out rather
/// than hang CI or bootstrap indefinitely.
const DOWNLOAD_TIMEOUT_MS = 180_000;

async function download(url, dest) {
  const attempts = 3;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    // Single AbortController per attempt covers headers AND body — a
    // truncated stream that stops sending bytes must also fail out,
    // not just a slow-to-connect server.
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(new Error("timeout")), DOWNLOAD_TIMEOUT_MS);
    try {
      const res = await fetch(url, { redirect: "follow", signal: ctrl.signal });
      if (!res.ok) throw new Error(`HTTP ${res.status} ${res.statusText}`);
      const { Readable } = await import("node:stream");
      const { pipeline } = await import("node:stream/promises");
      // Stream body → disk so 30-MB archives don't sit in memory.
      // The pipeline observes `ctrl.signal` via `Readable.fromWeb`
      // which forwards the abort into the underlying `ReadableStream`.
      await pipeline(Readable.fromWeb(res.body), fsSync.createWriteStream(dest));
      return;
    } catch (e) {
      const reason = ctrl.signal.aborted
        ? `timeout after ${DOWNLOAD_TIMEOUT_MS}ms`
        : (e.message ?? String(e));
      if (attempt === attempts) {
        // Clean up partial download so a retry isn't confused by it.
        try { await fs.unlink(dest); } catch {}
        console.error(`download failed after ${attempts} attempts: ${url}: ${reason}`);
        process.exit(7);
      }
      try { await fs.unlink(dest); } catch {}
      // Exponential backoff: 1s, 2s. Timeout dominates wall clock;
      // no point in long sleeps between retries.
      const backoff = 1000 * attempt;
      console.error(`  attempt ${attempt}/${attempts} failed (${reason}); retrying in ${backoff}ms`);
      await new Promise((r) => setTimeout(r, backoff));
    } finally {
      clearTimeout(timer);
    }
  }
}

async function sha256(file) {
  const hash = createHash("sha256");
  hash.update(await fs.readFile(file));
  return hash.digest("hex");
}

async function extract(archive, dest, ext) {
  // Pick a tar binary that (a) understands the format and (b) parses
  // Windows drive-letter paths as file paths, not `hostname:path`.
  //
  // - Windows runners have Git-for-Windows tar first on PATH; it's
  //   GNU tar and (i) can't read `.zip`, (ii) treats `C:\...` as an
  //   SSH host because of the colon. `%WINDIR%\System32\tar.exe` is
  //   bsdtar, fixes both.
  // - Linux runners have GNU tar; it handles tar.gz / tar.xz fine but
  //   can't read `.zip`, so route zip through `unzip` (preinstalled).
  // - macOS runners have bsdtar as `tar`; it handles everything.
  if (ext === "zip") {
    if (process.platform === "win32") {
      await run(win32Tar(), ["-xf", archive, "-C", dest]);
    } else {
      await run("unzip", ["-q", "-o", archive, "-d", dest]);
    }
    return;
  }
  const args =
    ext === "tar.gz"
      ? ["-xzf", archive, "-C", dest]
      : ext === "tar.xz"
        ? ["-xJf", archive, "-C", dest]
        : null;
  if (!args) {
    console.error(`unsupported ext: ${ext}`);
    process.exit(5);
  }
  const bin = process.platform === "win32" ? win32Tar() : "tar";
  await run(bin, args);
}

function win32Tar() {
  return path.join(
    process.env.SystemRoot ?? "C:\\Windows",
    "System32",
    "tar.exe",
  );
}

function run(cmd, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, args, { stdio: "inherit" });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) resolve();
      else reject(new Error(`${cmd} exited ${code}`));
    });
  });
}

async function findFile(root, name) {
  const entries = await fs.readdir(root, { withFileTypes: true });
  for (const e of entries) {
    const full = path.join(root, e.name);
    if (e.isFile() && e.name === name) return full;
    if (e.isDirectory()) {
      const nested = await findFile(full, name);
      if (nested) return nested;
    }
  }
  return null;
}

// ── Minimal TOML reader ────────────────────────────────────────────
//
// Handles exactly the shape essentials/VERSIONS.toml uses:
// - `[section]` and `[section.subsection]` headers
// - `key = "value"` and `"key" = "value"`
// - `key = ["a", "b"]`
// - `# comment` at end-of-line or as a whole line
//
// Returns nested plain objects. No dep — keeping CI zero-install.
function parseToml(text) {
  const out = {};
  let cursor = out;
  let sectionPath = [];

  for (let raw of text.split(/\r?\n/)) {
    // Strip trailing comment (but not inside a quoted string — VERSIONS.toml
    // doesn't have `#` inside values, so a naive strip is fine here).
    const commentIdx = raw.indexOf("#");
    if (commentIdx >= 0) raw = raw.slice(0, commentIdx);
    const line = raw.trim();
    if (!line) continue;

    const header = line.match(/^\[([^\]]+)\]$/);
    if (header) {
      sectionPath = header[1].split(".").map((s) => s.trim());
      cursor = out;
      for (const part of sectionPath) {
        cursor[part] ??= {};
        cursor = cursor[part];
      }
      continue;
    }

    const kv = line.match(/^"?([A-Za-z0-9_.\-+]+)"?\s*=\s*(.*)$/);
    if (!kv) continue;
    const key = kv[1];
    cursor[key] = parseValue(kv[2].trim());
  }
  return out;
}

function parseValue(v) {
  if (v.startsWith("[") && v.endsWith("]")) {
    // Array of strings.
    return v
      .slice(1, -1)
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean)
      .map((s) => s.replace(/^"|"$/g, ""));
  }
  if (v.startsWith('"') && v.endsWith('"')) return v.slice(1, -1);
  return v;
}

main().catch((e) => {
  console.error(e.stack ?? e.message ?? e);
  process.exit(1);
});
