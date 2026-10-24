//! Exercise an actual HTTPS response before rejecting its HTTP Location.
//! Linux's platform verifier accepts a child-scoped SSL_CERT_FILE; no host trust store is changed.
#![cfg(all(target_os = "linux", not(feature = "local-mirror")))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use artcraft_launcher_core::protocol::PAUSED;
use artcraft_launcher_native::http::{Http, Stop, Transfer};
use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose};

#[test]
fn production_agents_refuse_https_to_http_redirect() {
    if std::env::var_os("ARTCRAFT_TLS_REDIRECT_CHILD").is_none() {
        let dir = tempfile::tempdir().expect("temporary trust directory");
        let mut ca = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::DigitalSignature];
        let ca_key = KeyPair::generate().expect("CA key");
        let ca_cert = ca.self_signed(&ca_key).expect("CA certificate");
        let issuer = Issuer::new(ca, ca_key);
        let mut leaf = CertificateParams::new(vec!["localhost".into()]).expect("server params");
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate().expect("server key");
        let cert = leaf.signed_by(&key, &issuer).expect("signed server certificate");
        std::fs::write(dir.path().join("ca.pem"), ca_cert.pem()).expect("write root");
        std::fs::write(dir.path().join("server.der"), cert.der()).expect("write server cert");
        std::fs::write(dir.path().join("key.der"), key.serialize_der()).expect("write ephemeral test key");
        let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"));
        child.args(["--exact", "production_agents_refuse_https_to_http_redirect", "--nocapture"]);
        for key in ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "SSL_CERT_DIR"] {
            child.env_remove(key);
        }
        let status = child
            .env("NO_PROXY", "*")
            .env("no_proxy", "*")
            .env("SSL_CERT_FILE", dir.path().join("ca.pem"))
            .env("ARTCRAFT_TLS_REDIRECT_CHILD", dir.path())
            .status()
            .expect("isolated TLS test");
        assert!(status.success(), "isolated TLS redirect test failed");
        return;
    }
    let dir = std::path::PathBuf::from(std::env::var_os("ARTCRAFT_TLS_REDIRECT_CHILD").expect("test directory"));
    let cert = rustls::pki_types::CertificateDer::from(std::fs::read(dir.join("server.der")).expect("server cert"));
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(std::fs::read(dir.join("key.der")).expect("server key"));
    let config = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![cert], key.into()).expect("server TLS config");
    let plain = TcpListener::bind("127.0.0.1:0").expect("HTTP target");
    plain.set_nonblocking(true).expect("HTTP nonblocking");
    let target = format!("http://{}/untrusted", plain.local_addr().expect("HTTP address"));
    let tls = TcpListener::bind("127.0.0.1:0").expect("TLS listener");
    tls.set_nonblocking(true).expect("TLS nonblocking");
    let url = format!("https://localhost:{}/redirect", tls.local_addr().expect("TLS address").port());
    let location = target.clone();
    let plain_server = plain.try_clone().expect("HTTP target clone");
    let plain_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_plain = plain_requests.clone();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_done = done.clone();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut requests = 0;
        while !server_done.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < deadline {
            if let Ok((mut socket, _)) = plain_server.accept() {
                observed_plain.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                socket.set_write_timeout(Some(Duration::from_secs(1))).expect("HTTP write timeout");
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").expect("unexpected HTTP response");
            }
            match tls.accept() {
                Ok((socket, _)) => {
                    socket.set_read_timeout(Some(Duration::from_secs(3))).expect("read timeout");
                    socket.set_write_timeout(Some(Duration::from_secs(3))).expect("write timeout");
                    let session = rustls::ServerConnection::new(Arc::new(config.clone())).expect("TLS connection");
                    let mut stream = rustls::StreamOwned::new(session, socket);
                    let mut request = Vec::new();
                    let mut byte = [0];
                    while request.len() < 16384 && !request.ends_with(b"\r\n\r\n") {
                        stream.read_exact(&mut byte).expect("verified TLS request");
                        request.push(byte[0]);
                    }
                    assert!(request.ends_with(b"\r\n\r\n"), "bounded request headers");
                    write!(stream, "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .expect("redirect response");
                    stream.flush().expect("flush redirect");
                    requests += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => panic!("TLS accept: {e}"),
            }
        }
        requests
    });
    let http = Http::new().expect("production Http");
    let refused = |message: &str| assert!(message.contains("configured for https only:"), "wrong failure: {message}");
    refused(&http.get_api(&url, None).expect_err("API followed HTTP redirect"));
    refused(&http.get_feed(&url).expect_err("feed followed HTTP redirect").why);
    refused(&http.get_text(&url).expect_err("text followed HTTP redirect"));
    // The probe deliberately does not follow redirects: returning the destination is its contract.
    assert_eq!(http.redirect_target(&url).expect("HTTPS redirect probe"), Some(target));
    let download_dir = tempfile::tempdir().expect("download directory");
    let dest = download_dir.path().join("payload");
    let stop = Stop::default();
    let mut reconnected = false;
    // Reconnecting is emitted only after the verified TLS request was refused. Pause before
    // backoff, not before the request; the target would serve a real body if HTTPS_ONLY broke.
    let result = http.download(&url, &dest, 2, "2689367b205c16ce32ed4200942b8b8b1e262dfc70d9bc9fbc77c49699a4f1df", &stop, &mut |event| {
        if matches!(event, Transfer::Reconnecting { .. }) {
            reconnected = true;
            stop.pause();
        }
    });
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let requests = server.join().expect("TLS server");
    let error = result.expect_err("download followed HTTP redirect");
    assert_eq!(error, PAUSED);
    assert!(reconnected, "download must reject before reconnecting");
    assert!(!dest.exists(), "refused download must not install a file");
    assert_eq!(requests, 5, "all requests must reach verified TLS");
    assert_eq!(plain_requests.load(std::sync::atomic::Ordering::SeqCst), 0, "downgrade reached HTTP target");
    assert!(plain.accept().is_err(), "downgrade reached plain HTTP target");
}
