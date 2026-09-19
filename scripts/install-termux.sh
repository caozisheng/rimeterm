#!/data/data/com.termux/files/usr/bin/sh
# One-line Termux install for rimeterm:
#
#   curl -fsSL https://raw.githubusercontent.com/caozisheng/rimeterm/main/scripts/install-termux.sh | sh
#
# Downloads the `termux-aarch64` tarball from the latest GitHub release
# (cross-compiled against Bionic on CI) and installs both binaries into
# $PREFIX/bin.
#
# Note: clipboard is unavailable on Android — arboard has no Android
# backend, so rimeterm no-ops clipboard operations there.
set -eu

REPO="${RIMETERM_REPO:-caozisheng/rimeterm}"
PREFIX="${PREFIX:-/data/data/com.termux/files/usr}"

say() { printf '\n  %s\n' "$*"; }

# ─── 1. Environment guard ────────────────────────────────────────────
if [ "$(uname -o 2>/dev/null || true)" != "Android" ] || [ -z "${TERMUX_VERSION:-}" ]; then
    say "This installer targets Termux on Android. Run it inside Termux."
    exit 1
fi
if [ "$(uname -m)" != "aarch64" ]; then
    say "Only aarch64 devices are packaged right now (found: $(uname -m))."
    exit 1
fi

# ─── 2. Resolve the latest release asset ─────────────────────────────
say "Looking up the latest release..."
ASSETS=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest") || {
    say "Could not reach GitHub. Check the connection and retry."
    exit 1
}
URL=$(printf '%s\n' "$ASSETS" \
    | sed -n 's/.*"browser_download_url":[[:space:]]*"\([^"]*termux-aarch64\.tar\.gz\)".*/\1/p' \
    | head -n 1)
[ -n "$URL" ] || {
    say "No termux-aarch64 tarball in the latest release yet."
    say "Build from source instead: see the README's Android / Termux section."
    exit 1
}
say "Downloading: $URL"

# ─── 3. Download + extract ───────────────────────────────────────────
WORK="${TMPDIR:-$PREFIX/tmp}/rimeterm-install.$$"
mkdir -p "$WORK"
trap 'rm -rf "$WORK"' EXIT
curl -fsSL -o "$WORK/pkg.tar.gz" "$URL"
tar xzf "$WORK/pkg.tar.gz" -C "$WORK"

# ─── 4. Install into $PREFIX/bin ─────────────────────────────────────
DIR=$(find "$WORK" -mindepth 1 -type d -name "rimeterm-*" | head -n 1)
install -m 0755 "$DIR/rimeterm" "$PREFIX/bin/rimeterm"
install -m 0755 "$DIR/rimectl"  "$PREFIX/bin/rimectl"

say "Installed: rimeterm + rimectl -> $PREFIX/bin"
say "Note: clipboard is unavailable on Android (no OS backend for terminals)."
say "Done — run \`rimeterm\` to start."
