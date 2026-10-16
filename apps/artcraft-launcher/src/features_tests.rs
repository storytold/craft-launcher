use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use craft_ai_features::{AppStatus, AppUse, Licence, LocalRecipe, ModelFile, Source};
use sha2::{Digest, Sha256};

use super::*;

fn body() -> Vec<u8> {
    (0..700_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect()
}

/// Serves `data` to every request, in 50 KB pieces with a pause between them when `slow`.
fn serve(data: Vec<u8>, status: u16, slow: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let data = data.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", data.len());
                if stream.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for chunk in data.chunks(50_000) {
                    if stream.write_all(chunk).is_err() {
                        return;
                    }
                    if slow {
                        std::thread::sleep(Duration::from_millis(60));
                    }
                }
            });
        }
    });
    format!("http://127.0.0.1:{port}/m")
}

fn feature(data: &[u8], url: &str, licence: bool) -> Feature {
    let digest: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
    Feature {
        id: "t".into(),
        title: "Test".into(),
        summary: "Test".into(),
        capability: craft_ai_model::CapabilityId::parse("image.segment").unwrap(),
        inputs: vec![],
        uses: vec![AppUse { app: "A".into(), tool: "B".into(), status: AppStatus::Proposed }],
        local: Some(LocalRecipe {
            label: "Test model".into(),
            usable: true,
            workflow: "w".into(),
            models: vec![ModelFile {
                folder: "sam3".into(),
                filename: "m.safetensors".into(),
                size_bytes: data.len() as u64,
                sha256: digest,
                source: Source::Url { url: url.into() },
                licence: Licence { name: "L".into(), url: None, accept_required: licence, note: None },
            }],
            packs: vec![],
            min_vram_mb: None,
        }),
        cloud: vec![],
    }
}

fn comfy(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::create_dir_all(dir.join("custom_nodes")).unwrap();
    dir.to_owned()
}

/// Looks for ComfyUI afresh every time, as the real locator does, so a folder a download creates
/// is seen by the scan that follows it.
fn port(feature: Feature, install: Option<std::path::PathBuf>) -> DesktopFeatures {
    let config = DownloadConfig { allow_loopback: true, retry_delay: Duration::ZERO, ..DownloadConfig::default() };
    DesktopFeatures::with(
        vec![feature],
        Arc::new(move || install.as_deref().and_then(ComfyInstall::at)),
        config,
        Tools::new(Err("no store in this test".into())),
    )
}

fn settled(port: &mut DesktopFeatures) -> Snapshot {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let snap = port.snapshot();
        if !snap.scanning && !snap.plans.is_empty() && !lock(&port.shared).install_running {
            return snap;
        }
        assert!(Instant::now() < end, "timed out: {snap:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn state(snap: &Snapshot) -> State {
    snap.plans[0].state.clone()
}

#[test]
fn a_scan_turns_the_install_into_a_plan() {
    let data = body();
    let dir = tempfile::tempdir().unwrap();
    let mut p = port(feature(&data, "http://127.0.0.1:1/m", false), Some(comfy(dir.path())));
    p.rescan();
    p.rescan(); // a second request while one runs is harmless
    let snap = settled(&mut p);
    assert_eq!(state(&snap), State::NeedsInstall);
    assert_eq!(snap.plans[0].download_bytes, data.len() as u64);
}

#[test]
fn without_comfyui_there_is_nothing_to_install_into() {
    let data = body();
    let mut p = port(feature(&data, "http://127.0.0.1:1/m", false), None);
    p.rescan();
    assert_eq!(state(&settled(&mut p)), State::NoRuntime);
    assert!(p.install("t", true).unwrap_err().contains("nothing to download"));
}

#[test]
fn a_licence_must_be_accepted_and_the_shell_checks_it_itself() {
    let data = body();
    let dir = tempfile::tempdir().unwrap();
    let mut p = port(feature(&data, "http://127.0.0.1:1/m", true), Some(comfy(dir.path())));
    p.rescan();
    settled(&mut p);
    assert!(p.install("t", false).unwrap_err().contains("licence"));
    assert!(p.install("nope", true).is_err());
    assert!(dir.path().join("models").read_dir().unwrap().next().is_none(), "nothing was started");
}

#[test]
fn a_download_lands_in_the_models_folder_and_the_tool_becomes_ready() {
    let data = body();
    let url = serve(data.clone(), 200, false);
    let dir = tempfile::tempdir().unwrap();
    let mut p = port(feature(&data, &url, true), Some(comfy(dir.path())));
    p.rescan();
    settled(&mut p);
    p.install("t", true).unwrap();
    let snap = settled(&mut p);
    assert_eq!(snap.install, Install::Done { feature: "t".into() });
    assert_eq!(state(&snap), State::Ready);
    assert_eq!(std::fs::read(dir.path().join("models/sam3/m.safetensors")).unwrap(), data);
}

#[test]
fn a_failure_is_reported_and_leaves_the_tool_as_it_was() {
    let data = body();
    let url = serve(b"gone".to_vec(), 404, false);
    let dir = tempfile::tempdir().unwrap();
    let mut p = port(feature(&data, &url, false), Some(comfy(dir.path())));
    p.rescan();
    settled(&mut p);
    p.install("t", false).unwrap();
    let snap = settled(&mut p);
    assert!(matches!(&snap.install, Install::Failed { message, .. } if message.contains("no longer")), "{:?}", snap.install);
    assert_eq!(state(&snap), State::NeedsInstall);
    assert!(!dir.path().join("models/sam3/m.safetensors").exists());
}

#[test]
fn cancelling_stops_the_download_and_a_second_start_is_refused_while_it_runs() {
    let data = body();
    let url = serve(data.clone(), 200, true);
    let dir = tempfile::tempdir().unwrap();
    let mut p = port(feature(&data, &url, false), Some(comfy(dir.path())));
    p.rescan();
    settled(&mut p);
    p.install("t", false).unwrap();
    assert!(p.install("t", false).unwrap_err().contains("already running"));
    p.cancel_install();
    let snap = settled(&mut p);
    assert_eq!(snap.install, Install::Idle);
    assert_eq!(state(&snap), State::NeedsInstall);
    assert!(!dir.path().join("models/sam3/m.safetensors").exists());
}
