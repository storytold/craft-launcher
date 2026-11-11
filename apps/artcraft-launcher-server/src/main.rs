//! Optional self-hosted web workspace for ArtCraft Launcher.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
mod artifact_paths;
#[cfg(not(target_arch = "wasm32"))]
mod auth;
#[cfg(not(target_arch = "wasm32"))]
mod config;
#[cfg(not(target_arch = "wasm32"))]
mod http;
#[cfg(not(target_arch = "wasm32"))]
mod https;
#[cfg(not(target_arch = "wasm32"))]
mod releases;
#[cfg(not(target_arch = "wasm32"))]
mod store;

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ArtCraft Launcher server: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn run() -> Result<(), String> {
    use std::path::PathBuf;

    let mut args = std::env::args_os().skip(1);
    let flag = args.next();
    let file = args.next().map(PathBuf::from);
    if flag.as_deref() != Some(std::ffi::OsStr::new("--config")) || args.next().is_some() {
        return Err(String::from("Usage: artcraft-launcher-server --config <file>"));
    }
    let file = file.ok_or_else(|| String::from("A configuration path is required"))?;
    let Some(settings) = config::load(&file)? else {
        println!("ArtCraft Launcher server mode is disabled.");
        return Ok(());
    };
    let auth = auth::Auth::initialize(&settings.origin, settings.auth.clone()).await?;
    let store = store::Store::open(
        &settings.data_root.join("projects.sqlite3"),
        store::StoreLimits { per_user_bytes: settings.per_user_bytes, total_file_bytes: settings.total_file_bytes },
    )
    .map_err(|error| format!("Cannot start the project store: {error}"))?;
    let releases =
        releases::ReleaseService::open(&settings.artifact_root).map_err(|error| format!("Cannot start the editor release store: {error}"))?;
    let worker_token = config::read_secret(&settings.worker_token_file)?;
    if worker_token.len() < 32 || !worker_token.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
        return Err(String::from("The PC worker token must contain at least 32 URL-safe characters"));
    }
    let app = http::App::new(&settings, auth, store, releases, worker_token);
    let server = http::router(&settings, app);
    let listener =
        tokio::net::TcpListener::bind(settings.listen).await.map_err(|error| format!("Cannot listen on the configured address: {error}"))?;
    println!("ArtCraft Launcher server listening on {}", settings.listen);
    axum::serve(listener, server).with_graceful_shutdown(shutdown()).await.map_err(|error| format!("Server failed: {error}"))
}

#[cfg(not(target_arch = "wasm32"))]
async fn shutdown() {
    #[cfg(unix)]
    if let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        tokio::select! {
            _ = signal.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(target_arch = "wasm32")]
fn main() {}
