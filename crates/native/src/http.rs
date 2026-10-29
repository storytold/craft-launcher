//! HTTPS requests: craft-update's [`Http`], made the launcher's way.

pub use craft_update::http::*;

/// Names the launcher and its version, and nothing about the person using it.
pub(crate) const USER_AGENT: &str = concat!("ArtCraftLauncher/", env!("CARGO_PKG_VERSION"));

/// The launcher's client. `ARTCRAFT_LAUNCHER_GITHUB_TOKEN`, when set, raises GitHub's limit from
/// 60 to 5000 requests an hour; craft-update only ever sends it to api.github.com.
pub fn for_launcher() -> Result<Http, String> {
    Ok(Http::new(USER_AGENT)?.with_github_token(std::env::var("ARTCRAFT_LAUNCHER_GITHUB_TOKEN").ok()))
}

/// A fake HTTP server for the tests of the modules that download (craft-update keeps its own).
#[cfg(test)]
pub(crate) mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use sha2::{Digest, Sha256};

    use super::hex;

    /// How the fake server answers one request.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Reply {
        /// The whole file.
        Full,
        /// 404.
        Missing,
    }

    /// Serves `body` over plain HTTP following `script`, one reply per connection; records the
    /// `Range` header each request carried.
    pub(crate) fn serve(body: Vec<u8>, script: Vec<Reply>) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.bin", listener.local_addr().unwrap());
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let seen = ranges.clone();
        std::thread::spawn(move || {
            for reply in script {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let mut range = None;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                seen.lock().unwrap().push(range.map(|r| r.to_string()));
                let len = body.len();
                let _ = match reply {
                    Reply::Full => {
                        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n").and_then(|()| stream.write_all(&body))
                    }
                    Reply::Missing => write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
                };
            }
        });
        (url, ranges)
    }

    pub(crate) fn body() -> (Vec<u8>, String) {
        let body: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
        let sha = hex(&Sha256::digest(&body));
        (body, sha)
    }
}
