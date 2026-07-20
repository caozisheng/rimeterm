//! Local IPC for rimeterm: `rimectl` client + in-process server.
//!
//! See §11 of the design doc. v1 supports one request/response per connection
//! over a Windows named pipe or Unix domain socket, framed with
//! line-delimited JSON. The command surface is whatever the running rimeterm
//! process has registered in its [`rimeterm_core::CommandRegistry`].

pub mod client;
pub mod endpoint;
pub mod pid_liveness;
pub mod protocol;
pub mod server;

pub use client::{discover_latest_pid, send_once};
pub use endpoint::{endpoint_display_for_pid, lockfile_dir, lockfile_for_pid};
pub use pid_liveness::{PidLiveness, probe};
pub use protocol::{Request, Response, encode_request, encode_response};
pub use server::{Handler, spawn};
