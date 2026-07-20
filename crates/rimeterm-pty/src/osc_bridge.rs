//! OSC 1337 → rimeterm bridge scanner (§5.5 of the design doc, C18-D).
//!
//! PTY children speak to the kernel via a custom OSC escape:
//!
//! ```text
//! ESC ] 1337 ; rimeterm ; <json-payload> ST
//! ```
//!
//! where `ST` is either the two-byte ST terminator `ESC \` (0x1B 0x5C) or
//! the legacy BEL terminator (0x07). Payload is UTF-8 JSON; parsing into a
//! [`KernelEvent`] happens in the App — this crate only extracts the raw
//! payload string so the PTY layer stays event-model-agnostic.
//!
//! **Cross-chunk state**: the reader emits arbitrary byte splits (8 KiB
//! chunks on Windows, ~4 KiB pipe buffers on Unix). An escape could land
//! astride a chunk boundary. [`OscScanner`] is a small state machine that
//! survives across `feed` calls; a partial escape leaves it in an
//! intermediate state and the next chunk resumes decoding.
//!
//! **Payload cap**: `OSC_MAX_PAYLOAD_BYTES = 65_536` — a runaway child
//! writing an unterminated OSC MUST NOT balloon our buffer forever. When
//! the cap is hit we abort the current match, discard the partial
//! payload, and drop back to the scanning state. The child gets no
//! diagnostic (the byte stream is one-way write-only for OSCs); a
//! well-behaved integration keeps payloads under ~1 KiB.
//!
//! Kept as its own module — no dependency on alacritty, tokio, or the
//! wider PTY runtime — so tests are just `feed → collect → assert`.
//!
//! Non-goals: this scanner does NOT interfere with other OSC codes.
//! Anything that doesn't start with the exact `\x1b]1337;rimeterm;`
//! prefix is passed through untouched to the alacritty parser (i.e. we
//! don't remove the bytes from the read buffer — the scan is
//! non-destructive; alacritty sees the OSC and simply doesn't act on
//! `1337` codes it doesn't recognize).

/// Envelope prefix, in ASCII bytes. `Ps=1337`, `Pt` starts with `rimeterm;`.
/// The trailing `;` after `rimeterm` is included so payloads never begin
/// with a leading semicolon (which would decode as `""` and become
/// ambiguous with an empty payload).
pub(crate) const OSC_PREFIX: &[u8] = b"\x1b]1337;rimeterm;";

/// Ceiling on how much payload we're willing to buffer before giving up.
/// Bumped from a lower default because agents may send full JSON snapshots.
pub const OSC_MAX_PAYLOAD_BYTES: usize = 65_536;

/// Parser state. `Idle` means we're scanning for the next `\x1b`; every
/// intermediate state locks us into checking the next expected byte.
#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    /// Not currently inside any candidate escape.
    Idle,
    /// Matching `OSC_PREFIX` byte-by-byte at position `i`.
    MatchingPrefix { i: usize },
    /// Prefix matched — buffering payload bytes until a terminator.
    /// `esc_pending` = we just saw `\x1b` and are waiting for the
    /// second byte of an `ESC \` ST terminator.
    Payload { esc_pending: bool },
}

/// Rolling scanner. One per PTY session; feed every read chunk into
/// [`feed`] and drain complete payloads from the returned `Vec`.
///
/// State survives across `feed` calls (that's the point — cross-chunk
/// escapes are the common case, not the exception).
#[derive(Debug)]
pub struct OscScanner {
    state: State,
    /// Accumulated payload bytes for the in-flight escape. Reset on
    /// match completion or abort.
    buf: Vec<u8>,
    /// Bumped whenever a payload is dropped for exceeding `OSC_MAX_PAYLOAD_BYTES`.
    /// Exposed for diagnostics — no functional effect.
    pub aborted_oversize: usize,
}

impl Default for OscScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl OscScanner {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            buf: Vec::new(),
            aborted_oversize: 0,
        }
    }

    /// Feed a chunk. Returns every rimeterm payload that completed
    /// inside this chunk, in the order they appeared. Payloads are
    /// UTF-8-lossy-decoded (invalid bytes become `U+FFFD` REPLACEMENT
    /// CHARACTER) so a mistyped child never crashes the scanner.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for &b in chunk {
            self.step(b, &mut out);
        }
        out
    }

    /// Consume a single byte. Split from `feed` so tests can exercise
    /// byte-by-byte state transitions.
    fn step(&mut self, b: u8, out: &mut Vec<String>) {
        match self.state {
            State::Idle => {
                if b == 0x1B {
                    // Start of a candidate escape.
                    self.state = State::MatchingPrefix { i: 1 };
                }
            }
            State::MatchingPrefix { i } => {
                if i >= OSC_PREFIX.len() {
                    // Shouldn't happen — we transition to Payload the
                    // moment i hits len. Defensive: bail to Idle.
                    self.state = State::Idle;
                    return;
                }
                if b == OSC_PREFIX[i] {
                    if i + 1 == OSC_PREFIX.len() {
                        // Full prefix matched. Enter payload state.
                        self.state = State::Payload { esc_pending: false };
                    } else {
                        self.state = State::MatchingPrefix { i: i + 1 };
                    }
                } else if b == 0x1B {
                    // Fresh escape restart mid-mismatch (e.g. `\x1b]1337;\x1b]…`).
                    self.state = State::MatchingPrefix { i: 1 };
                } else {
                    // Prefix broke — drop back to scanning.
                    self.state = State::Idle;
                }
            }
            State::Payload { esc_pending } => {
                if esc_pending {
                    // We're inside an ST detection: previous byte was ESC.
                    if b == b'\\' {
                        // Two-byte ST terminator complete.
                        let payload = std::mem::take(&mut self.buf);
                        let s = String::from_utf8_lossy(&payload).into_owned();
                        out.push(s);
                        self.state = State::Idle;
                    } else if b == 0x1B {
                        // Another ESC — keep waiting for its second byte.
                        // (Adversarial input like `\x1b\x1b\\` is
                        // exceedingly rare; alacritty behaves the same.)
                        self.state = State::Payload { esc_pending: true };
                    } else {
                        // ESC not followed by `\` → wasn't ST. The
                        // previous ESC becomes part of the payload;
                        // append it plus this byte and keep buffering.
                        // `push_payload` can abort on the size cap; never
                        // overwrite its `Idle` state after an abort.
                        if self.push_payload(0x1B) && self.push_payload(b) {
                            self.state = State::Payload { esc_pending: false };
                        }
                    }
                } else if b == 0x07 {
                    // BEL terminator (legacy but widely used).
                    let payload = std::mem::take(&mut self.buf);
                    let s = String::from_utf8_lossy(&payload).into_owned();
                    out.push(s);
                    self.state = State::Idle;
                } else if b == 0x1B {
                    // Could be the first byte of an ST terminator; hold.
                    self.state = State::Payload { esc_pending: true };
                } else {
                    let _ = self.push_payload(b);
                }
            }
        }
    }

    /// Append to the payload buffer, aborting the current match if the
    /// cap is hit. Returns `false` when the append aborted the escape;
    /// callers MUST NOT restore a Payload state after that.
    fn push_payload(&mut self, b: u8) -> bool {
        if self.buf.len() >= OSC_MAX_PAYLOAD_BYTES {
            self.buf.clear();
            self.aborted_oversize += 1;
            self.state = State::Idle;
            return false;
        }
        self.buf.push(b);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<String> {
        let mut s = OscScanner::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(s.feed(c));
        }
        out
    }

    #[test]
    fn detects_st_terminated_payload_in_one_chunk() {
        let bytes = b"\x1b]1337;rimeterm;{\"cwd\":\"/tmp\"}\x1b\\";
        assert_eq!(scan(&[bytes]), vec![r#"{"cwd":"/tmp"}"#.to_string()]);
    }

    #[test]
    fn detects_bel_terminated_payload() {
        // xterm/legacy: BEL 0x07 as terminator.
        let bytes = b"\x1b]1337;rimeterm;hello\x07";
        assert_eq!(scan(&[bytes]), vec!["hello".to_string()]);
    }

    #[test]
    fn payload_survives_arbitrary_chunk_split() {
        // Split the escape between the prefix and the payload;
        // between the payload chars; and between ESC and `\` of the ST.
        // Every split must yield the same single payload.
        let full: &[u8] = b"\x1b]1337;rimeterm;abc\x1b\\";
        for split in 1..full.len() {
            let (a, b) = full.split_at(split);
            assert_eq!(
                scan(&[a, b]),
                vec!["abc".to_string()],
                "split at {split} lost the payload"
            );
        }
    }

    #[test]
    fn multiple_payloads_in_one_chunk() {
        let bytes: &[u8] = b"\x1b]1337;rimeterm;first\x07plain-text\x1b]1337;rimeterm;second\x1b\\";
        assert_eq!(
            scan(&[bytes]),
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn irrelevant_osc_ignored() {
        // OSC 0 (title), OSC 7 (cwd), OSC 52 (clipboard) — all pass
        // through untouched, no false positives.
        let bytes: &[u8] = b"\x1b]0;window title\x07\x1b]7;file:///tmp\x1b\\\x1b]52;c;dGVzdA==\x07";
        assert!(scan(&[bytes]).is_empty());
    }

    #[test]
    fn non_rimeterm_1337_ignored() {
        // iTerm2 uses `\x1b]1337;` for a completely different protocol
        // (base64 file transfer, growl notifications). We only latch on
        // `1337;rimeterm;` — anything else on 1337 is passed through.
        let bytes: &[u8] = b"\x1b]1337;File=name=x.txt:AAAA\x07";
        assert!(scan(&[bytes]).is_empty());
    }

    #[test]
    fn ignores_broken_prefix_and_resyncs_on_next_esc() {
        // First `\x1b]1337;rimet<newline>...` breaks mid-prefix; then
        // a fresh `\x1b]1337;rimeterm;good\x07` should still be found.
        let bytes: &[u8] = b"\x1b]1337;rimet\n\x1b]1337;rimeterm;good\x07";
        assert_eq!(scan(&[bytes]), vec!["good".to_string()]);
    }

    #[test]
    fn empty_payload_is_valid() {
        let bytes: &[u8] = b"\x1b]1337;rimeterm;\x07";
        assert_eq!(scan(&[bytes]), vec![String::new()]);
    }

    #[test]
    fn invalid_utf8_becomes_replacement_char() {
        // 0xFF is not valid UTF-8; must not panic.
        let bytes: &[u8] = b"\x1b]1337;rimeterm;pre\xffpost\x07";
        let got = scan(&[bytes]);
        assert_eq!(got.len(), 1);
        // U+FFFD is 3 bytes in UTF-8; the payload is "pre" + FFFD + "post".
        assert!(got[0].starts_with("pre"), "got: {:?}", got[0]);
        assert!(got[0].ends_with("post"), "got: {:?}", got[0]);
    }

    #[test]
    fn oversize_payload_aborts_and_scanner_recovers() {
        // Bomb the scanner with an unterminated payload larger than the
        // cap, then send a well-formed payload — the second one must
        // still land.
        let mut s = OscScanner::new();
        s.feed(b"\x1b]1337;rimeterm;");
        // Push OSC_MAX_PAYLOAD_BYTES + 100 filler bytes to trigger abort.
        let filler = vec![b'A'; OSC_MAX_PAYLOAD_BYTES + 100];
        assert!(s.feed(&filler).is_empty());
        assert!(
            s.aborted_oversize >= 1,
            "expected abort; got {}",
            s.aborted_oversize
        );
        // Now send a fresh well-formed one.
        let out = s.feed(b"\x1b]1337;rimeterm;ok\x07");
        assert_eq!(out, vec!["ok".to_string()]);
    }

    #[test]
    fn esc_inside_payload_not_followed_by_backslash_stays_in_payload() {
        // Rare but legal: JSON with an escaped `\u001b`. The raw ESC
        // byte followed by anything except `\` must NOT terminate.
        let bytes: &[u8] = b"\x1b]1337;rimeterm;a\x1bB\x07";
        let got = scan(&[bytes]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].len(), 3);
        assert_eq!(got[0].as_bytes(), b"a\x1bB");
    }
}
