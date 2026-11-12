#![allow(clippy::panic, clippy::unwrap_used)]

use std::sync::Barrier;
use std::thread;

use sha2::{Digest, Sha256};

use super::*;

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("launcher-releases-{}", Uuid::new_v4())))
    }

    fn open(&self) -> ReleaseService {
        ReleaseService::open(&self.0).unwrap()
    }

    fn site(&self, id: &str) -> PathBuf {
        self.0.join("builds").join(id).join("site")
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn request(app: &str) -> ReleaseRequest {
    ReleaseRequest { app: app.to_owned(), source_commit: "a".repeat(40), source_tree: "b".repeat(40) }
}

fn file(path: &str, bytes: &[u8]) -> ManifestFile {
    ManifestFile { path: path.into(), size: bytes.len() as u64, sha256: format!("{:x}", Sha256::digest(bytes)) }
}

fn manifest(source: ReleaseRequest, files: Vec<ManifestFile>) -> Manifest {
    Manifest { app: source.app, source_commit: source.source_commit, source_tree: source.source_tree, files }
}

fn stage(service: &ReleaseService, worker: &str, claim: &Claim, html: &[u8]) {
    let id = &claim.release.id;
    let token = &claim.claim_id;
    let source = claim.release.source.clone();
    let files = || vec![file("index.html", html), file("assets/editor.wasm", b"wasm")];
    assert!(matches!(service.finalize(id, worker, token), Err(ReleaseError::Incomplete)));
    service.submit_manifest(id, worker, token, manifest(source.clone(), files())).unwrap();
    let mut reversed = files();
    reversed.reverse();
    service.submit_manifest(id, worker, token, manifest(source, reversed)).unwrap();
    assert!(matches!(service.finalize(id, worker, token), Err(ReleaseError::Incomplete)));
    service.upload(id, worker, token, "index.html", html).unwrap();
    service.upload(id, worker, token, "index.html", html).unwrap();
    service.upload(id, worker, token, "assets/editor.wasm", b"wasm").unwrap();
}

#[test]
fn ready_sites_require_preview_permission_until_activation_and_survive_rollback() {
    let root = TestRoot::new();
    let service = root.open();
    let first = service.queue(request("photocraft")).unwrap();
    let claim = service.claim("worker-1").unwrap().unwrap();
    assert_eq!(claim.release.id, first.id);
    stage(&service, "worker-1", &claim, b"first");
    let first = service.finalize(&first.id, "worker-1", &claim.claim_id).unwrap();
    assert_eq!(first.state, "ready");
    assert!(!first.published);
    assert_eq!(service.finalize(&first.id, "worker-1", &claim.claim_id).unwrap().id, first.id);
    assert!(service.active("photocraft").unwrap().is_none());
    assert!(matches!(service.asset_path("photocraft", &first.id, "index.html", false), Err(ReleaseError::NotFound)));
    assert_eq!(fs::read(service.asset_path("photocraft", &first.id, "index.html", true).unwrap()).unwrap(), b"first");
    assert!(matches!(service.activate("wordcraft", &first.id), Err(ReleaseError::NotFound)));
    assert!(service.activate("photocraft", &first.id).unwrap().published);
    assert!(matches!(service.upload(&first.id, "worker-1", &claim.claim_id, "index.html", b"other"), Err(ReleaseError::Conflict)));

    let second = service.queue(request("photocraft")).unwrap();
    let claim = service.claim("worker-1").unwrap().unwrap();
    stage(&service, "worker-1", &claim, b"second");
    service.finalize(&second.id, "worker-1", &claim.claim_id).unwrap();
    service.activate("photocraft", &second.id).unwrap();
    service.activate("photocraft", &second.id).unwrap();
    assert!(matches!(service.rollback("photocraft", &first.id), Err(ReleaseError::Conflict)));
    assert_eq!(service.rollback("photocraft", &second.id).unwrap().id, first.id);
    assert!(matches!(service.rollback("photocraft", &second.id), Err(ReleaseError::Conflict)));

    let reopened = root.open();
    assert_eq!(reopened.active("photocraft").unwrap().unwrap().id, first.id);
    assert_eq!(fs::read(reopened.asset_path("photocraft", &second.id, "index.html", false).unwrap()).unwrap(), b"second");
}

#[test]
fn rejects_wrong_source_and_invalid_inventories_and_audits_uploaded_tree() {
    let root = TestRoot::new();
    let service = root.open();
    assert!(matches!(service.queue(request("lightcraft")), Err(ReleaseError::Invalid)));
    let job = service.queue(request("wordcraft")).unwrap();
    let claim = service.claim("worker").unwrap().unwrap();
    let id = &job.id;
    let token = &claim.claim_id;
    let entry = || file("index.html", b"valid");
    let mut other = job.source.clone();
    other.source_tree = "c".repeat(40);
    assert!(matches!(service.submit_manifest(id, "worker", token, manifest(other, vec![entry()])), Err(ReleaseError::Conflict)));
    for path in ["../x", "a//x", "CON.txt", "a/COM1.txt", "/index.html", "a\\b", "%2e.txt"] {
        let mut inventory = manifest(job.source.clone(), vec![entry(), file(path, b"bad")]);
        assert!(matches!(inventory.validate(), Err(ReleaseError::Invalid)), "{path}");
    }
    let mut collision = manifest(job.source.clone(), vec![entry(), file("A/x", b""), file("a/y", b"")]);
    assert!(matches!(collision.validate(), Err(ReleaseError::Invalid)));
    let mut oversized = file("big.wasm", b"");
    oversized.size = MAX_FILE_BYTES as u64 + 1;
    let mut oversized = manifest(job.source.clone(), vec![entry(), oversized]);
    assert!(matches!(oversized.validate(), Err(ReleaseError::Limit)));

    service.submit_manifest(id, "worker", token, manifest(job.source, vec![entry()])).unwrap();
    assert!(matches!(service.upload(id, "worker", token, "extra", b"extra"), Err(ReleaseError::Invalid)));
    assert!(matches!(service.upload(id, "worker", token, "index.html", b"wrong"), Err(ReleaseError::Invalid)));
    service.upload(id, "worker", token, "index.html", b"valid").unwrap();
    let site = root.site(id);
    let extra = site.join("extra");
    fs::write(&extra, b"unlisted").unwrap();
    assert!(matches!(service.finalize(id, "worker", token), Err(ReleaseError::Invalid)));
    fs::remove_file(extra).unwrap();
    fs::write(site.join("index.html"), b"wrong").unwrap();
    assert!(matches!(service.finalize(id, "worker", token), Err(ReleaseError::Invalid)));
}

#[cfg(unix)]
#[test]
fn rejects_a_symlink_even_with_matching_target_bytes() {
    use std::os::unix::fs::symlink;

    let root = TestRoot::new();
    let service = root.open();
    let job = service.queue(request("cadcraft")).unwrap();
    let claim = service.claim("worker").unwrap().unwrap();
    let id = &job.id;
    let token = &claim.claim_id;
    stage(&service, "worker", &claim, b"same");
    let site = root.site(id);
    let target = root.0.join("target");
    fs::write(&target, b"same").unwrap();
    fs::remove_file(site.join("index.html")).unwrap();
    symlink(target, site.join("index.html")).unwrap();
    assert!(matches!(service.finalize(id, "worker", token), Err(ReleaseError::Invalid)));
}

#[test]
fn simultaneous_workers_have_one_owner_and_expired_claims_are_terminal() {
    let root = TestRoot::new();
    let service = root.open();
    let job = service.queue(request("gridcraft")).unwrap();
    let start = Barrier::new(2);
    let (first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| {
            start.wait();
            service.claim("worker-1").unwrap()
        });
        let second = scope.spawn(|| {
            start.wait();
            service.claim("worker-2").unwrap()
        });
        (first.join().unwrap(), second.join().unwrap())
    });
    let (worker, wrong_worker, claim) = match (first, second) {
        (Some(claim), None) => ("worker-1", "worker-2", claim),
        (None, Some(claim)) => ("worker-2", "worker-1", claim),
        _ => panic!("one worker must own the only job"),
    };
    let id = &job.id;
    let token = &claim.claim_id;
    assert_eq!(service.claim(worker).unwrap().unwrap().claim_id, *token);
    assert!(matches!(service.renew(id, wrong_worker, token), Err(ReleaseError::Conflict)));
    assert!(service.renew(id, worker, token).unwrap() >= claim.expires_at);
    stage(&service, worker, &claim, b"staged");
    service.connect().unwrap().execute("UPDATE release_jobs SET expires_at = 0 WHERE id = ?1", [id]).unwrap();
    assert!(matches!(service.renew(id, worker, token), Err(ReleaseError::Conflict)));
    let status = record(&service.connect().unwrap(), id).unwrap();
    assert_eq!(status.state, "interrupted");
    assert_eq!(status.reason.as_deref(), Some("worker claim expired"));
    assert!(service.claim(wrong_worker).unwrap().is_none());
    assert!(!root.site(id).exists());

    let failed = service.queue(request("gridcraft")).unwrap();
    let claim = service.claim(worker).unwrap().unwrap();
    assert_eq!(service.fail(&failed.id, worker, &claim.claim_id).unwrap().state, "failed");
    let cancelled = service.queue(request("gridcraft")).unwrap();
    let claim = service.claim(worker).unwrap().unwrap();
    assert_eq!(service.cancel(&cancelled.id).unwrap().state, "cancelled");
    assert!(matches!(service.renew(&cancelled.id, worker, &claim.claim_id), Err(ReleaseError::Conflict)));
}

#[test]
fn queue_admission_stays_bounded() {
    let root = TestRoot::new();
    let service = root.open();
    for _ in 0..MAX_PENDING {
        service.queue(request("cadcraft")).unwrap();
    }
    assert!(matches!(service.queue(request("cadcraft")), Err(ReleaseError::Limit)));
    let claim = service.claim("worker").unwrap().unwrap();
    assert!(matches!(service.queue(request("cadcraft")), Err(ReleaseError::Limit)));
    service.cancel(&claim.release.id).unwrap();
    service.queue(request("cadcraft")).unwrap();
    assert_eq!(service.list().unwrap().len() as i64, MAX_PENDING + 1);
}

#[test]
fn capacity_retirement_keeps_active_previous_and_recent_published() {
    let root = TestRoot::new();
    let service = root.open();
    let mut ids = Vec::new();
    for _ in 0..4 {
        let release = service.queue(request("photocraft")).unwrap();
        let claim = service.claim("worker").unwrap().unwrap();
        stage(&service, "worker", &claim, b"published");
        service.finalize(&release.id, "worker", &claim.claim_id).unwrap();
        service.activate("photocraft", &release.id).unwrap();
        ids.push(release.id);
    }

    let connection = service.connect().unwrap();
    // Trigger byte pressure without writing a multi-gigabyte fixture.
    connection.execute("UPDATE release_jobs SET bytes = ?2, updated_at = 0 WHERE id = ?1", params![ids[0], MAX_STORE_BYTES as i64]).unwrap();
    let release = service.queue(request("photocraft")).unwrap();
    let claim = service.claim("worker").unwrap().unwrap();
    service.submit_manifest(&release.id, "worker", &claim.claim_id, manifest(release.source, vec![file("index.html", b"new")])).unwrap();
    assert!(!root.site(&ids[0]).exists());
    assert!(matches!(service.asset_path("photocraft", &ids[0], "index.html", false), Err(ReleaseError::NotFound)));
    for id in &ids[1..] {
        assert!(service.asset_path("photocraft", id, "index.html", false).is_ok());
    }
    assert_eq!(service.rollback("photocraft", &ids[3]).unwrap().id, ids[2]);
}

#[test]
fn a_failed_cleanup_keeps_capacity_reserved_without_blocking_writes() {
    let root = TestRoot::new();
    let service = root.open();
    let release = service.queue(request("wordcraft")).unwrap();
    service.cancel(&release.id).unwrap();
    let build = root.0.join("builds").join(&release.id);
    fs::write(&build, b"unexpected file instead of a build directory").unwrap();
    let connection = service.connect().unwrap();
    connection.execute("UPDATE release_jobs SET bytes = 123 WHERE id = ?1", [&release.id]).unwrap();
    assert!(service.queue(request("wordcraft")).is_ok());
    let reserved: (i64, i64) =
        connection.query_row("SELECT garbage, bytes FROM release_jobs WHERE id = ?1", [&release.id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(reserved, (1, 123));
    fs::remove_file(build).unwrap();
    assert!(service.claim("worker").unwrap().is_some());
    let reserved: (i64, i64) =
        connection.query_row("SELECT garbage, bytes FROM release_jobs WHERE id = ?1", [&release.id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(reserved, (0, 0));
}
