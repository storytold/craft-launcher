//! Bounded stdio JSON-RPC for an owned native CLI. Never replay a request.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use serde_json::{Value, json};

use crate::discovery::Program;

pub(crate) const MAX_FRAME: usize = 32 << 20;
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
enum Failure {
    Connection(String),
    Rejected(String),
}

pub(crate) fn read_frame(input: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    read_limited(input, MAX_FRAME)
}

fn read_limited(input: &mut impl BufRead, limit: usize) -> Result<Option<Vec<u8>>, String> {
    let mut frame = Vec::new();
    loop {
        let available = input.fill_buf().map_err(|e| e.to_string())?;
        if available.is_empty() {
            return if frame.is_empty() { Ok(None) } else { Err("Unterminated JSON-RPC frame".into()) };
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let count = newline.unwrap_or(available.len());
        if count > limit.saturating_sub(frame.len()) {
            return Err("JSON-RPC frame exceeds the size limit".into());
        }
        frame.extend_from_slice(available.get(..count).ok_or("Invalid frame boundary")?);
        input.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(frame));
        }
    }
}

pub(crate) struct Client {
    child: Child,
    input: SyncSender<Vec<u8>>,
    writes: Receiver<Result<(), String>>,
    replies: Receiver<Result<Value, String>>,
    next_id: u64,
    pub tools: Vec<Value>,
    pub healthy: bool,
}

impl Client {
    pub fn start(program: &Program) -> Result<Self, String> {
        let mut child = program
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Cannot start CLI: {e}"))?;
        let mut input = match child.stdin.take() {
            Some(input) => input,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("CLI stdin is unavailable".into());
            }
        };
        let (send_input, receive_input) = mpsc::sync_channel::<Vec<u8>>(1);
        let (send_write, writes) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            while let Ok(bytes) = receive_input.recv() {
                let result = input.write_all(&bytes).and_then(|()| input.write_all(b"\n")).and_then(|()| input.flush()).map_err(|e| e.to_string());
                let failed = result.is_err();
                if send_write.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let output = match child.stdout.take() {
            Some(output) => output,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("CLI stdout is unavailable".into());
            }
        };
        let (sender, replies) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            loop {
                let result = read_frame(&mut output).and_then(|line| {
                    line.ok_or_else(|| "CLI closed its output".to_string())
                        .and_then(|line| serde_json::from_slice(&line).map_err(|e| format!("Invalid CLI JSON: {e}")))
                });
                let failed = result.is_err();
                if sender.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let mut client = Self { child, input: send_input, writes, replies, next_id: 0, tools: Vec::new(), healthy: true };
        client.request("initialize", json!({"protocolVersion":"2025-06-18", "capabilities":{}, "clientInfo":{"name":"artcraft-launcher-mcp", "version":env!("CARGO_PKG_VERSION")}}))?;
        client.send(&json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))?;
        let mut cursor = None;
        let mut pages = 0_u16;
        let mut catalog_bytes = 0_usize;
        loop {
            pages += 1;
            if pages > 128 {
                return Err("CLI tool catalog exceeds 128 pages".into());
            }
            let params = cursor.as_ref().map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
            let result = client.request("tools/list", params)?;
            let tools = result.get("tools").and_then(Value::as_array).ok_or("CLI returned no tool catalog")?;
            if tools.len() > 4096_usize.saturating_sub(client.tools.len()) {
                return Err("CLI tool catalog exceeds 4096 tools".into());
            }
            let bytes = serde_json::to_vec(tools).map_err(|e| e.to_string())?.len();
            if bytes > MAX_FRAME.saturating_sub(catalog_bytes) {
                return Err("CLI tool catalog exceeds the size limit".into());
            }
            catalog_bytes += bytes;
            client.tools.extend(tools.iter().cloned());
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if cursor.as_deref() != Some(next) => cursor = Some(next.to_string()),
                Some(_) => return Err("CLI repeated its catalog cursor".into()),
                None => break,
            }
        }
        Ok(client)
    }

    fn send(&mut self, request: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FRAME {
            return Err("Request exceeds the frame limit".into());
        }
        self.input.try_send(bytes).map_err(|e| format!("CLI input queue unavailable: {e}"))?;
        self.writes.recv_timeout(TIMEOUT).map_err(|e| format!("CLI write timed out or stopped ({e}); inspect before retrying"))?
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if !self.healthy {
            return Err("Session connection failed; reconnect explicitly. An earlier edit may have completed; inspect before retrying.".into());
        }
        self.next_id = self.next_id.checked_add(1).ok_or("Request id exhausted")?;
        let id = self.next_id;
        match self.exchange(id, method, params) {
            Ok(result) => Ok(result),
            Err(Failure::Rejected(error)) => Err(error),
            Err(Failure::Connection(error)) => {
                self.healthy = false;
                Err(error)
            }
        }
    }

    fn exchange(&mut self, id: u64, method: &str, params: Value) -> Result<Value, Failure> {
        self.send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})).map_err(Failure::Connection)?;
        let deadline = std::time::Instant::now() + TIMEOUT;
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(Failure::Connection("CLI reply timed out; the operation may have completed. Inspect before retrying.".into()));
            }
            let wait = deadline.saturating_duration_since(std::time::Instant::now());
            let reply = self
                .replies
                .recv_timeout(wait)
                .map_err(|e| Failure::Connection(format!("CLI reply unavailable ({e}); the operation may have completed. Inspect before retrying.")))?
                .map_err(Failure::Connection)?;
            if let Some(result) = response_result(id, reply)? {
                return Ok(result);
            }
        }
    }
}

fn response_result(id: u64, reply: Value) -> Result<Option<Value>, Failure> {
    let invalid = |reason: &str| Failure::Connection(format!("{reason}; operation outcome is unknown"));
    if reply.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(invalid("Invalid CLI JSON-RPC envelope"));
    }
    if reply.get("method").and_then(Value::as_str).is_some() && reply.get("id").is_none() {
        // This increment does not advertise downstream progress or subscriptions.
        return Ok(None);
    }
    if reply.get("id").and_then(Value::as_u64) != Some(id) {
        return Err(invalid("CLI response id mismatch"));
    }
    match (reply.get("result"), reply.get("error")) {
        (Some(result), None) => Ok(Some(result.clone())),
        (None, Some(error)) => Err(Failure::Rejected(format!("CLI rejected the request: {error}"))),
        _ => Err(invalid("CLI reply needs exactly one result or error")),
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Own only the CLI. An attached desktop application is deliberately left open.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_require_newlines_and_are_bounded_before_allocation() {
        assert_eq!(read_limited(&mut b"{}\r\nrest".as_slice(), 3).unwrap(), Some(b"{}\r".to_vec()));
        assert!(read_limited(&mut b"12345\n".as_slice(), 4).is_err());
        assert!(read_limited(&mut b"{}".as_slice(), 4).is_err());
        assert_eq!(read_limited(&mut b"".as_slice(), 4).unwrap(), None);
    }

    #[test]
    fn mismatched_or_ambiguous_replies_are_connection_failures() {
        for reply in
            [json!({"jsonrpc":"2.0","id":2,"result":{}}), json!({"jsonrpc":"2.0","id":1,"result":{},"error":{}}), json!({"id":1,"result":{}})]
        {
            assert!(matches!(response_result(1, reply), Err(Failure::Connection(_))));
        }
    }

    #[test]
    fn rejections_are_distinct_from_lost_connections() {
        let reply = json!({"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"invalid argument"}});
        assert!(matches!(response_result(1, reply), Err(Failure::Rejected(_))));
        assert_eq!(response_result(1, json!({"jsonrpc":"2.0","id":1,"result":null})).unwrap(), Some(Value::Null));
        assert_eq!(response_result(1, json!({"jsonrpc":"2.0","method":"notifications/message","params":{}})).unwrap(), None);
    }
}
