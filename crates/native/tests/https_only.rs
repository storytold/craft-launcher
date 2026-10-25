//! The shipped `Http` agents refuse plain-HTTP URLs before connecting.
//!
//! This is an integration test, not a unit test inside the crate, so the library builds without
//! `cfg(test)` and `Http::new()` produces the production agents with `https_only` set. The
//! crate's own unit tests allow plain HTTP so their test server can speak loopback HTTP, which
//! means they can only pin ureq's behavior, not the agents the launcher ships.

#![cfg(not(feature = "local-mirror"))]

use std::net::TcpListener;

use artcraft_launcher_native::http::Http;

#[test]
fn production_agents_refuse_plain_http_before_connecting() {
    // Isolate proxy settings in a child process, without unsafe process-global environment
    // mutation in a parallel Rust test runner. System proxies already bypass loopback.
    if std::env::var_os("ARTCRAFT_HTTPS_ONLY_TEST_CHILD").is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"));
        child.args(["--exact", "production_agents_refuse_plain_http_before_connecting", "--nocapture"]);
        for key in ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
            child.env_remove(key);
        }
        let status = child.env("NO_PROXY", "*").env("no_proxy", "*").env("ARTCRAFT_HTTPS_ONLY_TEST_CHILD", "1").status().expect("run isolated test");
        assert!(status.success(), "isolated HTTPS-only regression test failed");
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    listener.set_nonblocking(true).expect("nonblocking listener");
    let url = format!("http://{}/releases/latest", listener.local_addr().expect("listener address"));

    let http = Http::new().expect("Http::new");
    let refuses_https_only = |message: &str| {
        assert!(message.contains("configured for https only:"), "unexpected refusal: {message}");
    };
    refuses_https_only(&http.get_api(&url, None).expect_err("plain-HTTP API request succeeded"));
    refuses_https_only(&http.get_feed(&url).expect_err("plain-HTTP feed request succeeded").why);
    refuses_https_only(&http.get_text(&url).expect_err("plain-HTTP GET succeeded"));
    refuses_https_only(&http.redirect_target(&url).expect_err("plain-HTTP redirect probe succeeded"));

    assert!(listener.accept().is_err(), "a plain-HTTP request reached the server");
}
