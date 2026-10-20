//! A minimal HTTP client interface, implemented with the `curl` binary.
//!
//! `curl` is on every CI runner, honors proxy variables and the system's
//! certificate store, and keeps TLS out of this crate. Requests are passed
//! through `curl --config -` on stdin, so tokens never appear in the
//! process list.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::PlatformError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: Vec<(&'a str, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub trait Http {
    fn send(&self, request: &Request<'_>) -> Result<Response, PlatformError>;
}

/// [`Http`] by running `curl`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Curl;

/// Marks the status code at the end of curl's output.
const STATUS_MARK: &str = "\n__witness_http_status__=";

impl Http for Curl {
    fn send(&self, request: &Request<'_>) -> Result<Response, PlatformError> {
        let config = curl_config(request);
        let mut child = Command::new("curl")
            .args(["--silent", "--show-error", "--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| PlatformError::Http(format!("could not run curl: {err}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(config.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(PlatformError::Http(format!(
                "curl failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (body, status) = text
            .rsplit_once(STATUS_MARK)
            .ok_or_else(|| PlatformError::Http("curl printed no status".to_owned()))?;
        let status = status
            .trim()
            .parse()
            .map_err(|_| PlatformError::Http(format!("bad HTTP status {status:?}")))?;
        Ok(Response {
            status,
            body: body.to_owned(),
        })
    }
}

/// The request as a curl config file.
fn curl_config(request: &Request<'_>) -> String {
    let mut config = String::new();
    let mut line = |key: &str, value: &str| {
        config.push_str(key);
        config.push_str(" = \"");
        config.push_str(&quote(value));
        config.push_str("\"\n");
    };
    line("url", request.url);
    line("request", request.method);
    for (name, value) in &request.headers {
        line("header", &format!("{name}: {value}"));
    }
    if let Some(body) = &request.body {
        line("data-binary", body);
    }
    line("write-out", &format!("{STATUS_MARK}%{{http_code}}"));
    config
}

/// Escapes a value for a double-quoted curl config string.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_quotes_values() {
        let config = curl_config(&Request {
            method: "POST",
            url: "https://api.example.com/x",
            headers: vec![("Authorization", "Bearer t0k\"en".to_owned())],
            body: Some("{\"body\":\"a\\nb\"}\nline".to_owned()),
        });
        assert!(config.contains("url = \"https://api.example.com/x\"\n"));
        assert!(config.contains("request = \"POST\"\n"));
        assert!(config.contains("header = \"Authorization: Bearer t0k\\\"en\"\n"));
        assert!(config.contains("data-binary = \"{\\\"body\\\":\\\"a\\\\nb\\\"}\\nline\"\n"));
        assert!(config.contains("write-out = \"\\n__witness_http_status__=%{http_code}\"\n"));
    }
}
