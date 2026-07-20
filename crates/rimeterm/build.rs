//! Embed a Windows Explorer / taskbar icon in the rimeterm binary.
//!
//! Uses `winresource` (0.1+, actively maintained fork of the deprecated
//! `winres`) to compile `assets/rimeterm.ico` into the .exe as a Win32
//! ICON resource. On non-Windows targets this build script is a no-op.
//!
//! Convert the source PNG to a multi-resolution ICO with:
//!
//! ```sh
//! python -c "from PIL import Image; \
//!   Image.open('rimeterm-icon.png').convert('RGBA').save( \
//!     'crates/rimeterm/assets/rimeterm.ico', format='ICO', \
//!     sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])"
//! ```
//!
//! We ship the .ico in-tree (small, ~110 KB) rather than regenerating it
//! at build time so `cargo build` doesn't need Python + Pillow.

fn main() {
    // Re-run only when the icon changes.
    println!("cargo:rerun-if-changed=assets/rimeterm.ico");

    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/rimeterm.ico");
        // Nice-to-have metadata that shows up in Explorer's file properties.
        res.set("ProductName", "rimeterm");
        res.set("FileDescription", "rimeterm — terminal for coding agents");
        if let Err(e) = res.compile() {
            // Don't hard-fail non-critical embedding — a build without an icon
            // is still runnable. Emit a warning so `cargo build -vv` surfaces it.
            println!("cargo:warning=winresource compile failed: {e}");
        }
    }
}
