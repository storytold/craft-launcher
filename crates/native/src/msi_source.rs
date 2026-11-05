//! An unprivileged, loopback-only byte source for elevated MSI verification.
//!
//! The privileged process never opens a caller-owned filesystem path (including paths that can
//! race into a junction/UNC target). These public release bytes are still untrusted: the OS-side
//! policy independently authenticates them before Windows Installer sees them.

use std::io::{Read, Seek, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const SOURCE_HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const SOURCE_WRITE_TIMEOUT: Duration = Duration::from_secs(60);
const ACCEPT_POLL: Duration = Duration::from_millis(20);

struct TransferFailure {
    item_index: Option<usize>,
    expected_bytes: Option<u64>,
    transferred_bytes: u64,
    error: std::io::Error,
}

pub(crate) struct Source {
    pub port: u16,
    stopped: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Source {
    pub fn new(items: &[crate::msi_batch::Item], session: &str) -> Result<Self, String> {
        let mut files = Vec::new();
        for item in items {
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt as _;
                options.share_mode(1);
            }
            files.push(Some(options.open(&item.msi).map_err(|e| format!("couldn't read the downloaded MSI ({e})"))?));
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stopped = Arc::new(AtomicBool::new(false));
        let done = stopped.clone();
        let session = session.to_string();
        let worker = std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut client, _)) => {
                        // Windows may propagate listener mode to accepted sockets. The listener
                        // polls nonblocking, but response writes must block through backpressure.
                        let configured = client
                            .set_nonblocking(false)
                            .and_then(|()| client.set_read_timeout(Some(SOURCE_HEADER_TIMEOUT)))
                            .and_then(|()| client.set_write_timeout(Some(SOURCE_WRITE_TIMEOUT)));
                        if let Err(error) = configured {
                            log_source_failure(&session, None, None, 0, &error);
                            continue;
                        }
                        match serve(&mut client, &session, &mut files, &done) {
                            Ok((item_index, expected_bytes, transferred_bytes)) => {
                                if let (Some(item_index), Some(expected_bytes)) = (item_index, expected_bytes) {
                                    log::debug!(
                                        "MSI loopback transfer complete: session={} item_index={} expected_bytes={} transferred_bytes={}",
                                        session,
                                        item_index,
                                        expected_bytes,
                                        transferred_bytes
                                    );
                                }
                            }
                            Err(failure) => {
                                log_source_failure(&session, failure.item_index, failure.expected_bytes, failure.transferred_bytes, &failure.error)
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(ACCEPT_POLL),
                    Err(error) => {
                        log_source_failure(&session, None, None, 0, &error);
                        break;
                    }
                }
            }
        });
        Ok(Self { port, stopped, worker: Some(worker) })
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::error!("MSI loopback source worker panicked");
        }
    }
}

fn log_source_failure(session: &str, item_index: Option<usize>, expected_bytes: Option<u64>, transferred_bytes: u64, error: &std::io::Error) {
    let item_index = item_index.map_or_else(|| "unknown".to_string(), |index| index.to_string());
    let expected_bytes = expected_bytes.map_or_else(|| "unknown".to_string(), |bytes| bytes.to_string());
    log::warn!(
        "MSI loopback transfer failed: session={} item_index={} expected_bytes={} transferred_bytes={} error_kind={:?} error={}",
        session,
        item_index,
        expected_bytes,
        transferred_bytes,
        error.kind(),
        error
    );
}

fn serve(
    client: &mut TcpStream,
    session: &str,
    files: &mut [Option<std::fs::File>],
    stopped: &AtomicBool,
) -> Result<(Option<usize>, Option<u64>, u64), TransferFailure> {
    let mut item_index = None;
    let mut expected_bytes = None;
    let mut transferred_bytes = 0;
    let response = (|| -> std::io::Result<()> {
        let mut header = Vec::new();
        let mut buffer = [0u8; 1024];
        while !header.ends_with(b"\r\n\r\n") {
            let n = client.read(&mut buffer)?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "incomplete loopback request header"));
            }
            if header.len().saturating_add(n) > 8192 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "loopback request header is too large"));
            }
            if stopped.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "loopback source stopped during request"));
            }
            header.extend_from_slice(&buffer[..n]);
        }
        let line = std::str::from_utf8(&header).ok().and_then(|s| s.lines().next()).unwrap_or_default();
        let index = line
            .strip_prefix(&format!("GET /{session}/"))
            .and_then(|s| s.strip_suffix(" HTTP/1.1"))
            .filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|s| s.parse::<usize>().ok());
        item_index = index;
        let Some(mut file) = index.and_then(|i| files.get_mut(i)).and_then(Option::take) else {
            return client.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        };
        let size = file.metadata()?.len();
        expected_bytes = Some(size);
        file.rewind()?;
        write!(client, "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n")?;
        transfer_exact(&mut file, client, size, stopped, &mut transferred_bytes)
    })();
    response.map(|()| (item_index, expected_bytes, transferred_bytes)).map_err(|error| TransferFailure {
        item_index,
        expected_bytes,
        transferred_bytes,
        error,
    })
}

fn transfer_exact(
    source: &mut impl Read,
    destination: &mut impl Write,
    expected_bytes: u64,
    stopped: &AtomicBool,
    transferred_bytes: &mut u64,
) -> std::io::Result<()> {
    let mut buffer = [0u8; 65536];
    while *transferred_bytes < expected_bytes {
        if stopped.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "loopback source stopped during package transfer"));
        }
        let remaining = expected_bytes - *transferred_bytes;
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let n = source.read(&mut buffer[..limit])?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "local MSI source ended before Content-Length bytes were sent"));
        }
        write_all_counted(destination, &buffer[..n], transferred_bytes)?;
    }
    Ok(())
}

fn write_all_counted(destination: &mut impl Write, mut bytes: &[u8], transferred_bytes: &mut u64) -> std::io::Result<()> {
    while !bytes.is_empty() {
        match destination.write(bytes) {
            Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "loopback client accepted no package bytes")),
            Ok(written) => {
                *transferred_bytes = transferred_bytes.saturating_add(written as u64);
                bytes = &bytes[written..];
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn test_item(path: std::path::PathBuf) -> crate::msi_batch::Item {
        crate::msi_batch::Item {
            msi: path,
            sha256: "a".repeat(64),
            destination: None,
            release: crate::elevated::ReleaseIdentity { app: "photocraft".into(), tag: "v0.5.0".into(), asset: "photo.msi".into() },
        }
    }

    fn payload(seed: usize, bytes: usize) -> Vec<u8> {
        (0..bytes).map(|index| (index.wrapping_mul(37).wrapping_add(seed * 19) & 0xff) as u8).collect()
    }

    fn get_response(port: u16, path: &str, chunk_bytes: usize, pause: Option<Duration>) -> std::io::Result<(u16, Option<u64>, Vec<u8>)> {
        let stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut response = BufReader::new(stream);
        write!(response.get_mut(), "GET /{path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;

        let mut line = String::new();
        response.read_line(&mut line)?;
        let status = line.split_whitespace().nth(1).and_then(|code| code.parse().ok()).unwrap_or(0);
        let mut content_length = None;
        loop {
            line.clear();
            response.read_line(&mut line)?;
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().ok();
            }
        }
        let mut body = Vec::new();
        let mut buffer = vec![0u8; chunk_bytes.max(1)];
        loop {
            let n = response.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buffer[..n]);
            if let Some(pause) = pause {
                std::thread::sleep(pause);
            }
        }
        Ok((status, content_length, body))
    }

    #[test]
    fn loopback_source_serves_only_fixed_session_indices_and_stops_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("app.msi");
        std::fs::write(&file, b"untrusted package bytes").unwrap();
        let item = crate::msi_batch::Item {
            msi: file,
            sha256: "a".repeat(64),
            destination: None,
            release: crate::elevated::ReleaseIdentity { app: "photocraft".into(), tag: "v0.5.0".into(), asset: "photo.msi".into() },
        };
        let source = Source::new(&[item], "session").unwrap();
        let get = |path: &str| {
            let mut client = TcpStream::connect(("127.0.0.1", source.port)).unwrap();
            client.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            write!(client, "GET /{path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            response
        };
        assert!(get("session/0").ends_with("untrusted package bytes"));
        for bad in ["wrong/0", "session/1", "session/../0", "session/0?x=1"] {
            assert!(get(bad).starts_with("HTTP/1.1 404"), "{bad}");
        }
        let port = source.port;
        drop(source);
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    }

    #[test]
    fn serves_twelve_large_binary_packages_sequentially_with_slow_reader_and_matching_hashes() {
        use sha2::Digest as _;

        let temp = tempfile::tempdir().unwrap();
        let mut items = Vec::new();
        let mut expected = Vec::new();
        for index in 0..12 {
            let path = temp.path().join(format!("{index}.msi"));
            let size = if index == 0 { 8 * 1024 * 1024 } else { 1024 * 1024 };
            let bytes = payload(index, size);
            std::fs::write(&path, &bytes).unwrap();
            expected.push((size as u64, crate::http::sha256_file(&path).unwrap()));
            items.push(test_item(path));
        }
        let source = Source::new(&items, "largebinary0123456789abcdef01234567").unwrap();

        for (index, (expected_bytes, expected_hash)) in expected.iter().enumerate() {
            // Reading the first 8 MiB in small, paced chunks forces the sender to handle TCP
            // backpressure rather than relying on the local send buffer to absorb the package.
            let slow = (index == 0).then_some(Duration::from_millis(1));
            let (status, content_length, body) =
                get_response(source.port, &format!("largebinary0123456789abcdef01234567/{index}"), 8 * 1024, slow).unwrap();
            assert_eq!(status, 200, "item {index}");
            assert_eq!(content_length, Some(*expected_bytes), "item {index} advertised length");
            assert_eq!(body.len() as u64, *expected_bytes, "item {index} transferred length");
            assert_eq!(crate::http::hex(&sha2::Sha256::digest(&body)), *expected_hash, "item {index} SHA-256");
        }
    }

    #[test]
    fn a_broken_request_does_not_stop_later_authorized_items() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.msi");
        let second = temp.path().join("second.msi");
        std::fs::write(&first, payload(1, 1024 * 1024)).unwrap();
        std::fs::write(&second, b"next authorized item").unwrap();
        let source = Source::new(&[test_item(first), test_item(second)], "recoverable-session").unwrap();

        let mut broken = TcpStream::connect(("127.0.0.1", source.port)).unwrap();
        broken.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        broken.write_all(b"GET /recoverable-session/0 HTTP/1.1\r\nHost:").unwrap();
        broken.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        broken.read_to_end(&mut response).unwrap();

        let (status, content_length, body) = get_response(source.port, "recoverable-session/1", 1024, None).unwrap();
        assert_eq!(status, 200);
        assert_eq!(content_length, Some(b"next authorized item".len() as u64));
        assert_eq!(body, b"next authorized item");
    }

    #[test]
    fn a_reader_disconnect_during_package_body_fails_that_item_and_serves_the_next_one() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("large-first.msi");
        let second = temp.path().join("second.msi");
        let first_bytes = payload(4, 32 * 1024 * 1024);
        std::fs::write(&first, &first_bytes).unwrap();
        std::fs::write(&second, b"authorized follow-up item").unwrap();
        let source = Source::new(&[test_item(first), test_item(second)], "partial-body-session").unwrap();

        let stream = TcpStream::connect(("127.0.0.1", source.port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut response = BufReader::new(stream);
        write!(response.get_mut(), "GET /partial-body-session/0 HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut line = String::new();
        response.read_line(&mut line).unwrap();
        assert!(line.starts_with("HTTP/1.1 200"));
        let mut advertised = None;
        loop {
            line.clear();
            response.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                advertised = value.trim().parse::<u64>().ok();
            }
        }
        assert_eq!(advertised, Some(32 * 1024 * 1024));
        let mut partial = [0u8; 64 * 1024];
        response.read_exact(&mut partial).unwrap();
        drop(response); // Close before Content-Length, as if the elevated reader was interrupted.

        let (status, content_length, body) = get_response(source.port, "partial-body-session/1", 1024, None).unwrap();
        assert_eq!(status, 200, "the source worker must continue after the broken transfer");
        assert_eq!(content_length, Some(b"authorized follow-up item".len() as u64));
        assert_eq!(body, b"authorized follow-up item");
    }

    #[test]
    fn exact_transfer_reports_early_file_eof_and_partial_socket_writes() {
        let stopped = AtomicBool::new(false);
        let mut bytes_written = Vec::new();
        let mut transferred = 0;
        let error = transfer_exact(&mut &b"short"[..], &mut bytes_written, 10, &stopped, &mut transferred).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
        assert_eq!(transferred, 5);
        assert_eq!(bytes_written, b"short");

        struct FailsAfter(Vec<u8>, usize);
        impl Write for FailsAfter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.0.len() == self.1 {
                    return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "test client disconnected"));
                }
                let written = bytes.len().min(self.1 - self.0.len());
                self.0.extend_from_slice(&bytes[..written]);
                Ok(written)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut destination = FailsAfter(Vec::new(), 3);
        transferred = 0;
        let error = transfer_exact(&mut &b"complete package"[..], &mut destination, 16, &stopped, &mut transferred).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        assert_eq!(transferred, 3, "partial writes must be counted before the socket error");
        assert_eq!(destination.0.len() as u64, transferred);

        let stopped = AtomicBool::new(true);
        transferred = 0;
        let error = transfer_exact(&mut &b"package"[..], &mut Vec::new(), 7, &stopped, &mut transferred).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(transferred, 0);
    }

    #[cfg(windows)]
    #[test]
    fn source_handles_prevent_package_replacement_until_the_elevated_copy_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("app.msi");
        std::fs::write(&file, b"checked package").unwrap();
        let item = crate::msi_batch::Item {
            msi: file.clone(),
            sha256: "0".repeat(64),
            destination: None,
            release: crate::elevated::ReleaseIdentity { app: "photocraft".into(), tag: "v0.5.0".into(), asset: "photo.msi".into() },
        };
        let source = Source::new(&[item], "session").unwrap();
        assert!(std::fs::OpenOptions::new().write(true).open(&file).is_err());
        assert!(std::fs::rename(&file, temp.path().join("moved.msi")).is_err());
        assert!(std::fs::remove_file(&file).is_err());
        drop(source);
        assert!(std::fs::rename(&file, temp.path().join("moved.msi")).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn windows_policy_copies_loopback_bytes_but_rejects_unsigned_or_changed_packages() {
        use base64::Engine as _;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("unsigned.msi");
        std::fs::write(&path, b"unsigned MSI fixture").unwrap();
        let sha = crate::http::sha256_file(&path).unwrap();
        let item = crate::msi_batch::Item {
            msi: path,
            sha256: sha.clone(),
            destination: None,
            release: crate::elevated::ReleaseIdentity { app: "photocraft".into(), tag: "v0.5.0".into(), asset: "photo.msi".into() },
        };
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let session = crate::http::hex(&random);
        let source = Source::new(&[item.clone(), item], &session).unwrap();
        let script = format!(
            "{}\n$identity=[Security.Principal.WindowsIdentity]::GetCurrent()\n\
             $principal=[Security.Principal.WindowsPrincipal]::new($identity)\n\
             if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {{ exit 77 }}\n\
             $root=New-ProtectedRun '{session}'\n\
             try {{\n\
             $caught=$false; try {{ Copy-VerifiedPackage $null @{{sha='{sha}'}} $root 0 {} '{session}' }} \
             catch {{\n\
                 if ($script:ArtCraftValidationStage -cne 'authenticode-chain-and-publisher') {{ throw ('Unsigned stream failed at unexpected stage: ' + $script:ArtCraftValidationStage) }}\n\
                 $message=[string]$_.Exception.Message\n\
                 $prefix='Authenticode status is '\n\
                 $suffix='; a valid Windows certificate chain is required'\n\
                 if (-not $message.StartsWith($prefix,[StringComparison]::Ordinal) -or -not $message.EndsWith($suffix,[StringComparison]::Ordinal)) {{ throw ('Unexpected unsigned stream rejection: ' + $message) }}\n\
                 $status=$message.Substring($prefix.Length,$message.Length-$prefix.Length-$suffix.Length)\n\
                 $knownStatuses=[Enum]::GetNames([System.Management.Automation.SignatureStatus])\n\
                 if ($status -notin $knownStatuses -or $status -ieq 'Valid') {{ throw ('Invalid Authenticode rejection status: ' + $status) }}\n\
                 $caught=$true\n\
             }}\n\
             if (-not $caught) {{ throw 'Unsigned stream was accepted' }}\n\
             $caught=$false; try {{ Copy-VerifiedPackage $null @{{sha=('0'*64)}} $root 1 {} '{session}' }} \
             catch {{\n\
                 if ($script:ArtCraftValidationStage -cne 'staged-package-hash') {{ throw ('Changed stream failed at unexpected stage: ' + $script:ArtCraftValidationStage) }}\n\
                 if ($_.Exception.Message -cne 'Changed package') {{ throw }}\n\
                 $caught=$true\n\
             }}\n\
             if (-not $caught) {{ throw 'Changed stream was accepted' }}\n\
             }} finally {{ [IO.Directory]::Delete($root,$true) }}",
            crate::elevated::POLICY,
            source.port,
            source.port
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(script.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
        let mut child = std::process::Command::new(crate::elevated::system_powershell().unwrap())
            .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let command = format!(
            "$ErrorActionPreference='Stop'; try {{ & ([ScriptBlock]::Create([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{encoded}')))); exit 0 }} catch {{ [Console]::Error.WriteLine($_); exit 1 }}\n"
        );
        child.stdin.take().unwrap().write_all(command.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        if output.status.code() == Some(77) {
            eprintln!("skipping protected System32 staging checks without an elevated test shell");
            return;
        }
        assert!(output.status.success(), "stdout: {}\nstderr: {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }
}
