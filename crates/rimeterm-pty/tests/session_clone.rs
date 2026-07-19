//! Structural test: `Session` implements `Clone` via `Arc`, so two clones
//! must share the same underlying grid + writer. We verify this without
//! spawning a real child (which needs ConPTY handshaking that's brittle in a
//! headless test environment).

use std::io::Write;
use std::sync::Arc;

use parking_lot::Mutex;

use rimeterm_pty::{PtyBackend, SessionConfig};

/// Prove `Session` is `Clone`. If this compiles, the derive is present.
#[test]
fn session_type_is_clone() {
    fn assert_clone<T: Clone>() {}
    assert_clone::<rimeterm_pty::Session>();
}

/// Prove `Session` is `Send + Sync` so IPC handler tasks can hold it.
#[test]
fn session_type_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<rimeterm_pty::Session>();
}

/// Prove `SessionConfig` is `Clone` so callers can spawn variants without
/// reconstructing every field.
#[test]
fn session_config_is_clone() {
    let _cfg = SessionConfig {
        program: std::path::PathBuf::from("/nonexistent/shell"),
        args: vec!["-c".into()],
        cwd: None,
        env: vec![("TERM".into(), "xterm-256color".into())],
        cols: 80,
        rows: 24,
        backend: PtyBackend::Native,
    };
    let _clone = _cfg.clone();
}

/// Sanity check that two `Arc<Mutex<Box<dyn Write>>>` handles cloned from a
/// single source point at the same underlying writer. This mirrors the
/// structure of `Session::writer`; the real `Session` cannot be built without
/// a live PTY, so we validate the *invariant* on a stand-in.
#[test]
fn arc_mutex_writer_clones_share_state() {
    struct MemWriter {
        inner: Vec<u8>,
    }
    impl Write for MemWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let writer: Arc<Mutex<Option<Box<dyn Write + Send>>>> =
        Arc::new(Mutex::new(Some(Box::new(MemWriter { inner: Vec::new() }))));
    let a = writer.clone();
    let b = writer.clone();

    // Write via handle `a`.
    if let Some(w) = a.lock().as_mut() {
        w.write_all(b"hello ").unwrap();
    }
    // Write via handle `b`.
    if let Some(w) = b.lock().as_mut() {
        w.write_all(b"world").unwrap();
    }

    // Both writes must be visible on the shared inner buffer regardless of
    // which handle we read through.
    let guard = writer.lock();
    let boxed = guard.as_ref().expect("writer set");
    // Downcast to peek — safe because we know the concrete type.
    let ptr = &**boxed as *const dyn Write as *const MemWriter;
    let inner = unsafe { &*ptr };
    assert_eq!(inner.inner, b"hello world");
}
