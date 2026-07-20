//! Wire protocol for rimeterm ↔ rimectl.
//!
//! Line-delimited JSON over the local transport (Windows named pipe on
//! Windows; Unix domain socket everywhere else).
//!
//! ## Framing
//!
//! - Each direction: one JSON object per `\n`.
//! - Request:  `{"cmd": "<command-id>", "args": {...}}`
//! - Response: `{"ok": true, "result": <any>}` or
//!   `{"ok": false, "error": "<message>"}`
//!
//! Only one request/response pair per connection in v1 (the simplest slice
//! that unblocks tooling). A future version can keep the connection alive.

use serde::{Deserialize, Serialize};

/// Client → server: run one registered command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub cmd: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// Server → client: outcome of a single command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn success(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn empty_success() -> Self {
        Self {
            ok: true,
            result: None,
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(msg.into()),
        }
    }
}

/// Serialize a request as `<json>\n`. Callers write this verbatim to the pipe.
pub fn encode_request(req: &Request) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(req)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn encode_response(resp: &Response) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(resp)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let req = Request {
            cmd: "app.palette.open".into(),
            args: serde_json::json!({"n": 1}),
        };
        let bytes = encode_request(&req).unwrap();
        assert!(bytes.ends_with(b"\n"));
        let back: Request = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn response_success_omits_error_field() {
        let r = Response::success(serde_json::json!(42));
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("error"));
        assert!(s.contains("\"ok\":true"));
    }

    #[test]
    fn response_error_carries_message() {
        let r = Response::err("boom");
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"ok\":false"));
        assert!(s.contains("boom"));
    }

    #[test]
    fn empty_args_defaults_to_null() {
        let req: Request = serde_json::from_str(r#"{"cmd":"app.quit"}"#).unwrap();
        assert_eq!(req.args, serde_json::Value::Null);
    }
}
