//! One local MCP connection for the installed Craft App Suite.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod discovery;
mod gateway;
mod transport;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{Value, json};

const USAGE: &str = "ArtCraft Launcher MCP gateway\n\nUsage: artcraft-launcher-mcp [--cli-dir DIR]...\n\nRegister this executable once in an MCP client. apps_list scans installed Craft Apps on each\nrequest. app_connect opens a live FigCraft, DesignCraft or VectorCraft session, or an explicitly\nrequested headless session in another app. app_tools discovers the connected app's schemas;\napp_read and app_call dispatch its tools. --cli-dir adds a trusted folder of installed CLI binaries.\nSee docs/mcp.md for configuration, capabilities and limitations.\n";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ArtCraft Launcher MCP: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut dirs = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--version" | "-V" => {
                println!("artcraft-launcher-mcp {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--cli-dir" => {
                let dir = args.next().ok_or("--cli-dir needs a folder")?;
                let dir = PathBuf::from(dir);
                if !dir.is_dir() {
                    return Err(format!("CLI folder does not exist: {}", dir.display()));
                }
                dirs.push(dir.canonicalize().map_err(|e| e.to_string())?);
            }
            _ => return Err(format!("unknown option {arg}\n{USAGE}")),
        }
    }
    serve(&mut io::stdin().lock(), &mut io::stdout().lock(), &mut gateway::Gateway::new(dirs))
}

fn serve(input: &mut impl io::BufRead, output: &mut impl Write, gateway: &mut gateway::Gateway) -> Result<(), String> {
    while let Some(line) = transport::read_frame(input)? {
        let reply = match serde_json::from_slice::<Value>(&line) {
            Ok(request) => gateway.handle(request),
            Err(_) => Some(rpc_error(Value::Null, -32700, "Invalid JSON")),
        };
        if let Some(reply) = reply {
            serde_json::to_writer(&mut *output, &reply).map_err(|e| e.to_string())?;
            output.write_all(b"\n").and_then(|()| output.flush()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code, "message":message}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_json_does_not_prevent_the_next_request() {
        let input = b"{bad}\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n";
        let mut output = Vec::new();
        serve(&mut input.as_slice(), &mut output, &mut gateway::Gateway::new(Vec::new())).unwrap();
        let replies: Vec<Value> = output.split(|b| *b == b'\n').filter(|b| !b.is_empty()).map(|line| serde_json::from_slice(line).unwrap()).collect();
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert_eq!(replies[1]["id"], 7);
        assert_eq!(replies[1]["result"], json!({}));
    }
}
