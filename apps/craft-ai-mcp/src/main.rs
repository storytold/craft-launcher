//! `craft-ai-mcp`: the gateway through which MCP clients (Claude, Cursor, LM Studio) reach every
//! installed Craft app. The ArtCraft Launcher ships it beside itself; Settings › AI agents copies
//! it into `ArtCraft/mcp/bin` and registers that copy (`artcraft_launcher_native::agents`). The
//! program itself is `craft_ai_mcp::cli` from storytold/craft-ai-integrations.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    craft_ai_mcp::cli::main(std::env::args().skip(1).collect())
}

/// Desktop only: nothing to run in the browser build.
#[cfg(target_arch = "wasm32")]
fn main() {}
