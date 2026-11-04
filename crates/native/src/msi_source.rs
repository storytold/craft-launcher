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
                        let _ = client.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = client.set_write_timeout(Some(Duration::from_secs(2)));
                        let _ = serve(&mut client, &session, &mut files, &done);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => break,
                }
            }
        });
        Ok(Self { port, stopped, worker: Some(worker) })
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(client: &mut TcpStream, session: &str, files: &mut [Option<std::fs::File>], stopped: &AtomicBool) -> std::io::Result<()> {
    let mut header = Vec::new();
    let mut buffer = [0u8; 1024];
    while !header.ends_with(b"\r\n\r\n") {
        let n = client.read(&mut buffer)?;
        if n == 0 || header.len() + n > 8192 || stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        header.extend_from_slice(&buffer[..n]);
    }
    let line = std::str::from_utf8(&header).ok().and_then(|s| s.lines().next()).unwrap_or_default();
    let index = line
        .strip_prefix(&format!("GET /{session}/"))
        .and_then(|s| s.strip_suffix(" HTTP/1.1"))
        .filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        .and_then(|s| s.parse::<usize>().ok());
    let Some(mut file) = index.and_then(|i| files.get_mut(i)).and_then(Option::take) else {
        return client.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    };
    let size = file.metadata()?.len();
    file.rewind()?;
    write!(client, "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n")?;
    let mut buffer = [0u8; 65536];
    let mut remaining = size;
    while remaining > 0 && !stopped.load(Ordering::SeqCst) {
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let n = file.read(&mut buffer[..limit])?;
        if n == 0 {
            break;
        }
        client.write_all(&buffer[..n])?;
        remaining = remaining.saturating_sub(n as u64);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
