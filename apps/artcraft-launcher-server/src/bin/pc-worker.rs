//! One-shot PC builder for the optional ArtCraft Launcher release service.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    if let Some(result) = recipe_process::run_group_anchor() {
        return match result {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("ArtCraft Launcher recipe group anchor: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    run_worker()
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn run_worker() -> std::process::ExitCode {
    match native::run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ArtCraft Launcher PC worker: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
#[path = "../artifact_paths.rs"]
mod artifact_paths;

#[cfg(not(target_arch = "wasm32"))]
#[path = "../https.rs"]
mod https;

#[cfg(not(target_arch = "wasm32"))]
#[path = "../recipe_process.rs"]
mod recipe_process;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::collections::{BTreeMap, HashSet};
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, File, Metadata, OpenOptions};
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant, SystemTime};

    use jiff::Timestamp;
    use serde::{Deserialize, Serialize, de::DeserializeOwned};
    use sha2::{Digest, Sha256};
    use ureq::http::{Method, Request, Response, StatusCode, header, request::Builder};
    use url::Url;
    use uuid::Uuid;

    use crate::artifact_paths::{MAX_ARTIFACT_DEPTH, is_link, valid_path};
    use crate::https::Https;
    use crate::recipe_process::{self, Process, Spawned};

    const API: &str = "/api/server/v1/worker";
    const MAX_CONFIG: u64 = 64 * 1024;
    const MAX_TOKEN: u64 = 4096;
    const MAX_RESPONSE: usize = 64 * 1024;
    const MAX_MANIFEST: usize = 2 * 1024 * 1024;
    const MAX_FILE: u64 = 96 * 1024 * 1024;
    const MAX_SITE: u64 = 512 * 1024 * 1024;
    const MAX_FILES: usize = 4096;
    const MAX_ENTRIES: usize = 8192;
    const RENEW_INTERVAL: Duration = Duration::from_secs(5 * 60);
    const MAX_RECIPE_DURATION: Duration = Duration::from_secs(6 * 60 * 60);
    const RECIPE_POLL: Duration = Duration::from_secs(2);
    const RECIPE_FINISH_TIMEOUT: Duration = Duration::from_secs(2);
    const RECIPE_FINISH_POLL: Duration = Duration::from_millis(10);

    type Result<T> = std::result::Result<T, String>;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        origin: Url,
        token_file: PathBuf,
        scratch_root: PathBuf,
        git_program: PathBuf,
        apps: BTreeMap<String, App>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct App {
        repo: PathBuf,
        allowed_ref: String,
        build: Recipe,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Recipe {
        program: PathBuf,
        args: Vec<String>,
        output: String,
    }

    impl Config {
        fn load(path: &Path) -> Result<Self> {
            let path = file_path(path, "worker configuration")?;
            let base = path.parent().ok_or("Configuration needs a parent directory")?;
            let mut config: Self =
                serde_json::from_slice(&read_bounded(&path, MAX_CONFIG)?).map_err(|_| String::from("Invalid worker configuration JSON"))?;
            if !https_origin(&config.origin) {
                return Err("The worker origin must be an HTTPS origin without credentials or a path".into());
            }
            config.scratch_root = directory(&base.join(&config.scratch_root), "worker scratch root")?;
            config.token_file = file_path(&base.join(&config.token_file), "worker token file")?;
            config.git_program = executable(&config.git_program)?;
            if config.apps.is_empty() {
                return Err("Configure at least one editor build recipe".into());
            }
            for (name, app) in &mut config.apps {
                if !valid_app(name) || !allowed_ref(&app.allowed_ref) || !valid_path(&app.build.output) {
                    return Err("Invalid configured app, allowed Git ref, or relative output path".into());
                }
                app.repo = directory(&base.join(&app.repo), "editor repository")?;
                if app.repo.starts_with(&config.scratch_root) || config.scratch_root.starts_with(&app.repo) {
                    return Err("The worker scratch root and editor repository must be separate".into());
                }
                app.build.program = executable(&app.build.program)?;
            }
            Ok(config)
        }
    }

    fn https_origin(origin: &Url) -> bool {
        origin.scheme() == "https"
            && origin.host_str().is_some()
            && origin.username().is_empty()
            && origin.password().is_none()
            && origin.path() == "/"
            && origin.query().is_none()
            && origin.fragment().is_none()
    }

    fn valid_app(app: &str) -> bool {
        matches!(app, "wordcraft" | "gridcraft" | "photocraft" | "cadcraft")
    }

    fn allowed_ref(reference: &str) -> bool {
        ["refs/heads/", "refs/remotes/", "refs/tags/"].iter().any(|prefix| {
            reference
                .strip_prefix(prefix)
                .is_some_and(|name| !name.is_empty() && !name.starts_with('/') && !name.ends_with('/') && !name.contains("//"))
        }) && reference.len() <= 512
            && !reference.contains("..")
            && !reference.contains("@{")
            && reference.bytes().all(|byte| byte.is_ascii_graphic() && !b"~^:?*[\\ ".contains(&byte))
    }

    fn lowercase_hex(value: &str, len: usize) -> bool {
        value.len() == len && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    fn canonical_uuid(value: &str) -> bool {
        Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
    }

    #[derive(Deserialize)]
    struct ClaimResponse {
        #[serde(deserialize_with = "Option::deserialize")]
        claim: Option<Claim>,
    }

    // Deliberately select only the worker data fields. Extra server metadata has no executable meaning.
    #[derive(Deserialize)]
    struct Claim {
        id: String,
        claim_id: String,
        app: String,
        source_commit: String,
        source_tree: String,
        expires_at: i64,
    }

    impl Claim {
        fn validate(&self) -> Result<()> {
            if !canonical_uuid(&self.id)
                || !canonical_uuid(&self.claim_id)
                || !valid_app(&self.app)
                || !lowercase_hex(&self.source_commit, 40)
                || !lowercase_hex(&self.source_tree, 40)
            {
                return Err("The server returned an invalid worker claim".into());
            }
            Ok(())
        }
    }

    #[derive(Deserialize)]
    struct Renewal {
        expires_at: i64,
    }

    #[derive(Deserialize)]
    struct TerminalResponse {
        id: String,
        state: String,
    }

    #[derive(Serialize)]
    struct Manifest<'a> {
        app: &'a str,
        source_commit: &'a str,
        source_tree: &'a str,
        files: Vec<Entry>,
    }

    #[derive(Serialize)]
    struct Entry {
        path: String,
        size: u64,
        sha256: String,
    }

    struct WorkerHttp {
        client: Https,
        origin: Url,
        bearer: header::HeaderValue,
    }

    impl WorkerHttp {
        fn new(config: &Config) -> Result<Self> {
            let token = read_token(&config.token_file)?;
            let mut bearer = header::HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| String::from("Malformed worker token"))?;
            bearer.set_sensitive(true);
            Ok(Self { client: Https::new(Duration::from_secs(5 * 60)), origin: config.origin.clone(), bearer })
        }

        async fn claim(&self) -> Result<Option<Claim>> {
            let mut url = self.origin.clone();
            url.set_path(&format!("{API}/claim"));
            let request = self.builder(Method::POST, &url);
            let response: ClaimResponse = self.json(request, Vec::new(), "claim").await?;
            if let Some(claim) = &response.claim {
                claim.validate()?;
            }
            Ok(response.claim)
        }

        fn builder(&self, method: Method, url: &Url) -> Builder {
            Request::builder().method(method).uri(url.as_str()).header(header::AUTHORIZATION, self.bearer.clone())
        }

        fn request(&self, method: Method, claim: &Claim, route: &str, path: Option<&str>) -> Result<Builder> {
            let mut url = self.origin.clone();
            url.set_path(&format!("{API}/{}/{route}", claim.id));
            if let Some(path) = path {
                url.query_pairs_mut().append_pair("path", path);
            }
            let mut lease = header::HeaderValue::from_str(&claim.claim_id).map_err(|_| String::from("Invalid claim header"))?;
            lease.set_sensitive(true);
            Ok(self.builder(method, &url).header("x-artcraft-claim", lease))
        }

        async fn renew(&self, claim: &Claim) -> Result<i64> {
            let request = self.request(Method::POST, claim, "renew", None)?;
            let response: Renewal = self.json(request, Vec::new(), "renew").await?;
            Ok(response.expires_at)
        }

        async fn manifest(&self, claim: &Claim, manifest: &Manifest<'_>) -> Result<()> {
            let body = serde_json::to_vec(manifest).map_err(|_| String::from("Cannot encode the artifact manifest"))?;
            if body.len() > MAX_MANIFEST {
                return Err("The artifact manifest exceeds its size limit".into());
            }
            let request = self.request(Method::POST, claim, "manifest", None)?.header(header::CONTENT_TYPE, "application/json");
            self.empty(request, body, "manifest").await
        }

        async fn upload(&self, claim: &Claim, path: &str, bytes: Vec<u8>) -> Result<()> {
            let request = self.request(Method::PUT, claim, "file", Some(path))?.header(header::CONTENT_TYPE, "application/octet-stream");
            self.empty(request, bytes, "file").await
        }

        async fn terminal(&self, claim: &Claim, route: &str, expected: &str) -> Result<()> {
            let request = self.request(Method::POST, claim, route, None)?;
            let response: TerminalResponse = self.json(request, Vec::new(), route).await?;
            if response.id != claim.id || response.state != expected {
                return Err("Unexpected server terminal response".into());
            }
            Ok(())
        }

        async fn json<T: DeserializeOwned>(&self, request: Builder, body: Vec<u8>, operation: &str) -> Result<T> {
            let response = self.send(request, body, operation).await?;
            serde_json::from_slice(response.body()).map_err(|_| String::from("Invalid worker response JSON"))
        }

        async fn empty(&self, request: Builder, body: Vec<u8>, operation: &str) -> Result<()> {
            let response = self.send(request, body, operation).await?;
            if response.status() != StatusCode::NO_CONTENT {
                return Err(String::from("Unexpected worker response"));
            }
            Ok(())
        }

        async fn send(&self, request: Builder, body: Vec<u8>, operation: &str) -> Result<Response<Vec<u8>>> {
            let request = request.body(body).map_err(|_| String::from("Invalid worker HTTP request"))?;
            let response = self.client.send(request, MAX_RESPONSE).await.map_err(|_| format!("Worker {operation} request failed"))?;
            if !response.status().is_success() {
                return Err(format!("Worker {operation} returned HTTP {}", response.status().as_u16()));
            }
            Ok(response)
        }
    }

    fn read_token(path: &Path) -> Result<String> {
        let metadata = regular_file(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err("The token file must be private to its owner".into());
            }
        }
        #[cfg(not(unix))]
        let _ = metadata;
        let bytes = read_bounded(path, MAX_TOKEN)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| String::from("Malformed worker token"))?;
        let token = text.trim_end_matches(['\r', '\n']);
        if token.is_empty() || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("Malformed worker token".into());
        }
        Ok(token.to_owned())
    }

    struct Lease<'a> {
        http: &'a WorkerHttp,
        claim: &'a Claim,
        next_renewal: Instant,
    }

    impl<'a> Lease<'a> {
        async fn begin(http: &'a WorkerHttp, claim: &'a Claim) -> Result<Self> {
            let mut lease = Self { http, claim, next_renewal: renewal_at(claim.expires_at)? };
            // A returned claim can be an interrupted older attempt. Reset the lease before work.
            lease.renew().await?;
            Ok(lease)
        }

        async fn renew(&mut self) -> Result<()> {
            self.next_renewal = renewal_at(self.http.renew(self.claim).await?)?;
            Ok(())
        }

        async fn renew_if_due(&mut self) -> Result<()> {
            if Instant::now() >= self.next_renewal {
                self.renew().await?;
            }
            Ok(())
        }
    }

    fn renewal_at(expires_at: i64) -> Result<Instant> {
        let now = Timestamp::try_from(SystemTime::now()).map_err(|_| String::from("Invalid PC clock"))?.as_second();
        let remaining = expires_at.saturating_sub(now);
        if remaining <= 0 {
            return Err("The worker lease has expired or the PC clock is incorrect".into());
        }
        let delay = Duration::from_secs((remaining as u64 / 3).max(1)).min(RENEW_INTERVAL);
        Ok(Instant::now() + delay)
    }

    pub(super) async fn run() -> Result<()> {
        let mut args = std::env::args_os().skip(1);
        let flag = args.next();
        let path = args.next().map(PathBuf::from);
        if flag.as_deref() != Some(OsStr::new("--config")) || args.next().is_some() {
            return Err("Usage: artcraft-launcher-pc-worker --config <file>".into());
        }
        let config = Config::load(&path.ok_or("A worker configuration path is required")?)?;
        let _lock = ScratchLock::acquire(&config.scratch_root)?;
        let http = WorkerHttp::new(&config)?;
        let Some(claim) = http.claim().await? else {
            println!("No editor update is queued.");
            return Ok(());
        };
        let result = build_and_publish(&config, &http, &claim).await;
        match result {
            Ok(()) => {
                println!("The editor build is ready for activation: {}", claim.id);
                Ok(())
            }
            Err(error) => {
                let _ = http.terminal(&claim, "fail", "failed").await;
                Err(error)
            }
        }
    }

    struct ScratchLock {
        path: PathBuf,
        file: Option<File>,
    }

    impl ScratchLock {
        fn acquire(root: &Path) -> Result<Self> {
            let path = root.join("worker.lock");
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|_| String::from("Cannot reserve the worker scratch directory; check for another worker"))?;
            if writeln!(file, "{}", std::process::id()).is_err() {
                drop(file);
                let _ = fs::remove_file(&path);
                return Err("Cannot record the worker PID".into());
            }
            Ok(Self { path, file: Some(file) })
        }
    }

    impl Drop for ScratchLock {
        fn drop(&mut self) {
            self.file.take();
            let _ = fs::remove_file(&self.path);
        }
    }

    async fn build_and_publish(config: &Config, http: &WorkerHttp, claim: &Claim) -> Result<()> {
        let app = config.apps.get(&claim.app).ok_or("This editor has no local build recipe")?;
        verify_approved_source(config, app, claim)?;
        let mut lease = Lease::begin(http, claim).await?;
        let job = config.scratch_root.join(format!("{}-{}", claim.id, Uuid::new_v4()));
        fs::create_dir(&job).map_err(|_| String::from("Cannot create the private build directory"))?;
        let job = directory(&job, "private build directory")?;
        if !job.starts_with(&config.scratch_root) {
            return Err("The build directory escaped the configured scratch root".into());
        }
        let worktree = job.join("source");
        add_worktree(config, app, &job, &worktree, &claim.source_commit)?;
        let result = prepare_artifacts(config, app, claim, &job, &worktree, &mut lease).await;
        let cleanup = remove_worktree(config, app, &job, &worktree);
        match (result, cleanup) {
            (Ok(()), Ok(())) => http.terminal(claim, "finalize", "ready").await,
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        }
    }

    fn verify_approved_source(config: &Config, app: &App, claim: &Claim) -> Result<()> {
        let actual = git_text(config, &app.repo, ["rev-parse", "--verify", &format!("{}^{{tree}}", claim.source_commit)])?;
        if actual != claim.source_tree {
            return Err("The configured repository does not contain the requested source tree".into());
        }
        let status = git(config, &app.repo)
            .args(["merge-base", "--is-ancestor", &claim.source_commit, &app.allowed_ref])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| String::from("Cannot check the configured approved source"))?;
        if !status.success() {
            return Err("The requested source commit is outside the configured approved ref".into());
        }
        Ok(())
    }

    fn verify_checkout(config: &Config, checkout: &Path, claim: &Claim) -> Result<()> {
        if git_text(config, checkout, ["rev-parse", "--verify", "HEAD"])? != claim.source_commit
            || git_text(config, checkout, ["rev-parse", "--verify", "HEAD^{tree}"])? != claim.source_tree
        {
            return Err("The build checkout no longer matches the requested source".into());
        }
        let status = git(config, checkout)
            .args(["diff", "--quiet", "HEAD", "--"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| String::from("Cannot check the build checkout"))?;
        if !status.success() {
            return Err("The build recipe changed tracked source files".into());
        }
        Ok(())
    }

    fn add_worktree(config: &Config, app: &App, job: &Path, checkout: &Path, commit: &str) -> Result<()> {
        let empty_hooks = job.join("hooks");
        fs::create_dir(&empty_hooks).map_err(|_| String::from("Cannot create the isolated hooks directory"))?;
        let mut options = OsString::from("core.hooksPath=");
        options.push(&empty_hooks);
        let status = git(config, &app.repo)
            .arg("-c")
            .arg(options)
            .args(["worktree", "add", "--detach"])
            .arg(checkout)
            .arg(commit)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| String::from("Cannot start the isolated Git checkout"))?;
        if !status.success() {
            return Err("Git could not create the isolated build checkout".into());
        }
        Ok(())
    }

    fn remove_worktree(config: &Config, app: &App, job: &Path, checkout: &Path) -> Result<()> {
        let checkout = directory(checkout, "isolated worktree")?;
        if checkout.parent() != Some(job)
            || checkout.file_name() != Some(OsStr::new("source"))
            || !job.starts_with(&config.scratch_root)
            || !checkout.join(".git").is_file()
        {
            return Err("The isolated worktree could not be verified for cleanup".into());
        }
        let status = git(config, &app.repo)
            .args(["worktree", "remove", "--force"])
            .arg(&checkout)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| String::from("Cannot remove the isolated build checkout"))?;
        if !status.success() {
            return Err("Git could not remove the isolated build checkout".into());
        }
        Ok(())
    }

    fn git(config: &Config, path: &Path) -> Command {
        let mut command = Command::new(&config.git_program);
        command.arg("-C").arg(path).env("GIT_TERMINAL_PROMPT", "0");
        hide_window(&mut command);
        command
    }

    fn git_text<const N: usize>(config: &Config, path: &Path, args: [&str; N]) -> Result<String> {
        let output = git(config, path).args(args).output().map_err(|_| String::from("Cannot read the editor Git checkout"))?;
        if !output.status.success() || output.stdout.len() > 256 {
            return Err("Git could not resolve the requested object".into());
        }
        let output = std::str::from_utf8(&output.stdout).map_err(|_| "Invalid Git object response")?;
        Ok(output.trim().to_owned())
    }

    async fn prepare_artifacts(config: &Config, app: &App, claim: &Claim, job: &Path, checkout: &Path, lease: &mut Lease<'_>) -> Result<()> {
        verify_checkout(config, checkout, claim)?;
        run_recipe(&app.build, job, checkout, lease).await?;
        verify_checkout(config, checkout, claim)?;
        let site = output_directory(checkout, &app.build.output)?;
        let files = inventory(&site)?;
        let manifest = Manifest { app: &claim.app, source_commit: &claim.source_commit, source_tree: &claim.source_tree, files };
        lease.renew().await?;
        lease.http.manifest(claim, &manifest).await?;
        for entry in manifest.files {
            lease.renew_if_due().await?;
            let bytes = read_bounded(&site.join(&entry.path), MAX_FILE)?;
            if bytes.len() as u64 != entry.size || format!("{:x}", Sha256::digest(&bytes)) != entry.sha256 {
                return Err("A local artifact changed after the manifest was generated".into());
            }
            lease.http.upload(claim, &entry.path, bytes).await?;
        }
        lease.renew_if_due().await
    }

    async fn run_recipe(recipe: &Recipe, job: &Path, checkout: &Path, lease: &mut Lease<'_>) -> Result<()> {
        let temp = job.join("temp");
        fs::create_dir(&temp).map_err(|_| String::from("Cannot create the job's temporary directory"))?;
        let log = File::create(job.join("build.log")).map_err(|_| String::from("Cannot open the local build log"))?;
        let mut command = Command::new(&recipe.program);
        command
            .args(&recipe.args)
            .current_dir(checkout)
            .env("TEMP", &temp)
            .env("TMP", &temp)
            .env("TMPDIR", &temp)
            .env("CARGO_BUILD_JOBS", "1")
            .env("CARGO_INCREMENTAL", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let Spawned { mut process, stdout, stderr } =
            recipe_process::spawn(command).map_err(|error| format!("Cannot start the configured build recipe: {error}"))?;
        let mut output = [stdout.map(drain), stderr.map(drain)];
        let result = wait_for_recipe(&mut process, lease).await;
        finish_recipe(process, result, log, &mut output, RECIPE_FINISH_TIMEOUT).await
    }

    async fn wait_for_recipe(process: &mut Process, lease: &mut Lease<'_>) -> Result<()> {
        let deadline = tokio::time::Instant::now() + MAX_RECIPE_DURATION;
        tokio::time::timeout_at(deadline, poll_recipe(process, lease))
            .await
            .unwrap_or_else(|_| Err(String::from("The configured build recipe exceeded its time limit")))
    }

    async fn poll_recipe(process: &mut Process, lease: &mut Lease<'_>) -> Result<()> {
        loop {
            match process.leader_success().map_err(|_| String::from("Cannot verify completion of the build recipe"))? {
                Some(true) => return Ok(()),
                Some(false) => return Err("The configured build recipe failed; inspect build.log".into()),
                None => {}
            }
            lease.renew_if_due().await?;
            tokio::time::sleep(RECIPE_POLL).await;
        }
    }

    async fn finish_recipe(process: Process, result: Result<()>, log: impl Write, output: &mut [Option<Reader>; 2], timeout: Duration) -> Result<()> {
        let process = match result {
            Ok(()) => Ok(process),
            Err(reason) => Err(stop_recipe(process, reason)),
        };
        let deadline = Instant::now() + timeout;
        let logs = write_log(log, output, deadline).await;
        let process = process?;
        if let Err(reason) = logs {
            return Err(stop_recipe(process, reason));
        }
        if let Err(reason) = wait_for_recipe_tree(&process, deadline).await {
            return Err(stop_recipe(process, reason));
        }
        process.reap().map(|_| ()).map_err(|error| format!("Cannot reap the build recipe: {error}"))
    }

    async fn wait_for_recipe_tree(process: &Process, deadline: Instant) -> Result<()> {
        while Instant::now() < deadline {
            let empty = process.is_empty().map_err(|error| format!("Cannot verify that the build recipe tree finished: {error}"))?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if empty {
                return Ok(());
            }
            tokio::time::sleep(RECIPE_FINISH_POLL.min(remaining)).await;
        }
        Err("The build recipe did not finish its process tree before the deadline; inspect build.log".into())
    }

    fn stop_recipe(process: Process, reason: String) -> String {
        match process.stop() {
            Ok(_) => reason,
            Err(error) => format!("{reason}. Cannot stop the recipe: {error}"),
        }
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    #[test]
    fn cancelling_a_recipe_reaps_the_owned_child() -> io::Result<()> {
        let mut command = Command::new("sleep");
        command.arg("30").stdout(Stdio::null()).stderr(Stdio::null());
        let Spawned { process, .. } = recipe_process::spawn(command)?;
        assert!(!process.stop()?.success());
        Ok(())
    }

    type Reader = std::thread::JoinHandle<io::Result<Vec<u8>>>;

    fn drain(mut reader: impl Read + Send + 'static) -> Reader {
        const CAPACITY: usize = 128 * 1024;
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            let mut tail = Vec::new();
            loop {
                let read = reader.read(&mut buffer)?;
                let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else { return Ok(tail) };
                let overflow = (tail.len() + read).saturating_sub(CAPACITY);
                tail.drain(..overflow);
                tail.extend_from_slice(chunk);
            }
        })
    }

    async fn write_log(mut file: impl Write, output: &mut [Option<Reader>; 2], deadline: Instant) -> Result<()> {
        let mut complete = true;
        for (name, reader) in ["stdout", "stderr"].into_iter().zip(output) {
            let Some(handle) = reader.as_ref() else { continue };
            while !handle.is_finished() && Instant::now() < deadline {
                tokio::time::sleep(RECIPE_FINISH_POLL).await;
            }
            // A descendant can inherit the pipe. Keep the wait bounded and let the caller stop the owned tree.
            let tail = if handle.is_finished() {
                let reader = reader.take().ok_or("Cannot collect the build log")?;
                reader.join().map_err(|_| "Cannot collect the build log")?.map_err(|_| "Cannot read the build log")?
            } else {
                complete = false;
                b"Log collection timed out while the output pipe was still open.\n".to_vec()
            };
            writeln!(file, "Latest build {name}")
                .and_then(|()| file.write_all(&tail))
                .map_err(|_| String::from("Cannot write the local build log"))?;
        }
        if !complete {
            return Err("The build recipe did not close its output pipes; inspect build.log".into());
        }
        Ok(())
    }

    #[cfg(test)]
    #[tokio::test]
    async fn an_open_log_pipe_makes_the_recipe_incomplete() {
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let reader = std::thread::spawn(move || {
            let _ = blocked.recv();
            Ok(Vec::new())
        });
        let mut output = [Some(reader), None];
        let mut log = Vec::new();
        let result = write_log(&mut log, &mut output, Instant::now()).await;
        drop(release);
        assert!(result.is_err());
        assert!(String::from_utf8_lossy(&log).contains("output pipe was still open"));
    }

    #[cfg(all(test, any(windows, target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    mod recipe_tests {
        use std::fs::{File, TryLockError};
        use std::io::Write as _;
        use std::path::{Path, PathBuf};
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        use std::{fs, io};

        use uuid::Uuid;

        use super::{Process, Reader, Spawned, drain, finish_recipe, hide_window, recipe_process};

        const FIXTURE_ROLE: &str = "ARTCRAFT_TEST_RECIPE_ROLE";
        const FIXTURE_ROOT: &str = "ARTCRAFT_TEST_RECIPE_ROOT";
        const EXIT_WITNESS: &str = "exit-witness";
        const POLL: Duration = Duration::from_millis(10);
        const LIMIT: Duration = Duration::from_secs(5);

        struct FixtureFiles(PathBuf);

        impl FixtureFiles {
            fn new() -> io::Result<Self> {
                let root = std::env::temp_dir().join(format!("artcraft-recipe-tree-{}", Uuid::new_v4()));
                fs::create_dir(&root)?;
                Ok(Self(root))
            }
        }

        impl Drop for FixtureFiles {
            fn drop(&mut self) {
                for name in ["ready", "release", "complete", EXIT_WITNESS] {
                    let _ = fs::remove_file(self.0.join(name));
                }
                let _ = fs::remove_dir(&self.0);
            }
        }

        fn fixture_command(role: &str, root: Option<&Path>) -> io::Result<Command> {
            let mut command = Command::new(std::env::current_exe()?);
            command.args(["--exact", "native::recipe_tests::owned_recipe_fixture", "--nocapture"]).env(FIXTURE_ROLE, role);
            if let Some(root) = root {
                command.env(FIXTURE_ROOT, root);
            }
            hide_window(&mut command);
            Ok(command)
        }

        #[allow(clippy::zombie_processes, reason = "the fixture intentionally leaves an owned descendant alive after its recipe leader exits")]
        #[test]
        fn owned_recipe_fixture() -> io::Result<()> {
            match std::env::var(FIXTURE_ROLE).as_deref() {
                Ok("pipe-leader") => {
                    let descendant = fixture_command("pipe-descendant", None)?.stdout(Stdio::inherit()).spawn()?;
                    drop(descendant);
                }
                Ok("pipe-descendant") => std::thread::sleep(Duration::from_secs(15)),
                Ok("redirected-leader") => {
                    let descendant = fixture_command("redirected-descendant", None)?.stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
                    drop(descendant);
                }
                Ok("redirected-descendant") => finish_redirected_fixture()?,
                _ => {}
            }
            Ok(())
        }

        fn finish_redirected_fixture() -> io::Result<()> {
            let root = std::env::var_os(FIXTURE_ROOT).map(PathBuf::from).ok_or_else(|| io::Error::other("Missing recipe fixture root"))?;
            let witness = File::options().read(true).write(true).open(root.join(EXIT_WITNESS));
            let mut witness = match witness {
                Ok(file) => {
                    file.lock()?;
                    Some(file)
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            let result = wait_for_fixture_release(&root);
            // The descendant holds the lock from before readiness until process teardown or
            // this marked return, including if it times out waiting for the fixture gate.
            if let Some(file) = &mut witness {
                file.write_all(b"returned")?;
            }
            result
        }

        fn wait_for_fixture_release(root: &Path) -> io::Result<()> {
            fs::write(root.join("ready"), b"ready")?;
            let deadline = Instant::now() + Duration::from_secs(15);
            let release = root.join("release");
            while !release.try_exists()? {
                if Instant::now() >= deadline {
                    return Err(io::Error::other("Recipe fixture was not released"));
                }
                std::thread::sleep(POLL);
            }
            fs::write(root.join("complete"), b"complete")
        }

        async fn start_fixture(role: &str, root: Option<&Path>) -> io::Result<(Process, [Option<Reader>; 2])> {
            let mut command = fixture_command(role, root)?;
            command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
            let Spawned { mut process, stdout, stderr } = recipe_process::spawn(command)?;
            let output = [stdout.map(drain), stderr.map(drain)];
            let deadline = Instant::now() + LIMIT;
            let result = wait_for_fixture_leader(&mut process, deadline).await;
            if let Err(error) = result {
                let _ = process.stop();
                return Err(error);
            }
            if let Some(root) = root {
                let result = wait_for_fixture_file(&root.join("ready"), deadline).await;
                if let Err(error) = result {
                    let _ = process.stop();
                    return Err(error);
                }
            }
            Ok((process, output))
        }

        async fn wait_for_fixture_leader(process: &mut Process, deadline: Instant) -> io::Result<()> {
            loop {
                match process.leader_success()? {
                    Some(true) => return Ok(()),
                    None if Instant::now() < deadline => tokio::time::sleep(POLL).await,
                    _ => return Err(io::Error::other("Synthetic recipe leader did not exit successfully")),
                }
            }
        }

        async fn wait_for_fixture_file(path: &Path, deadline: Instant) -> io::Result<()> {
            while !path.try_exists()? {
                if Instant::now() >= deadline {
                    return Err(io::Error::other("Synthetic recipe descendant did not become ready"));
                }
                tokio::time::sleep(POLL).await;
            }
            Ok(())
        }

        #[tokio::test]
        async fn log_timeout_stops_the_owned_descendant_after_its_leader_exits() -> io::Result<()> {
            let (process, mut output) = start_fixture("pipe-leader", None).await?;
            let mut log = Vec::new();
            let result = finish_recipe(process, Ok(()), &mut log, &mut output, Duration::ZERO).await;
            let reader = output.first().and_then(Option::as_ref).ok_or_else(|| io::Error::other("The inherited pipe did not time out"))?;
            let deadline = Instant::now() + LIMIT;
            while !reader.is_finished() && Instant::now() < deadline {
                tokio::time::sleep(POLL).await;
            }
            assert!(reader.is_finished(), "the owned descendant should no longer hold the pipe open");
            assert_eq!(result.as_ref().map_err(String::as_str), Err("The build recipe did not close its output pipes; inspect build.log"));
            assert!(String::from_utf8_lossy(&log).contains("output pipe was still open"));
            Ok(())
        }

        #[tokio::test]
        async fn redirected_descendant_finishes_within_recipe_grace() -> io::Result<()> {
            let files = FixtureFiles::new()?;
            let (process, mut output) = start_fixture("redirected-leader", Some(&files.0)).await?;
            let release = files.0.join("release");
            let release = async {
                tokio::time::sleep(Duration::from_millis(150)).await;
                fs::write(release, b"release")
            };
            let mut log = Vec::new();
            let finish = async {
                let result = finish_recipe(process, Ok(()), &mut log, &mut output, LIMIT).await;
                result.map_err(io::Error::other)?;
                // Observe the marker at the inventory boundary, before joining the gate writer.
                assert_eq!(fs::read(files.0.join("complete"))?.as_slice(), b"complete");
                Ok::<(), io::Error>(())
            };
            let (result, gate) = tokio::join!(finish, release);
            gate?;
            result
        }

        async fn wait_for_exit_witness(witness: &File) -> io::Result<()> {
            let deadline = Instant::now() + LIMIT;
            while Instant::now() < deadline {
                match witness.try_lock() {
                    Ok(()) => return Ok(()),
                    Err(TryLockError::WouldBlock) => tokio::time::sleep(POLL).await,
                    Err(TryLockError::Error(error)) => return Err(error),
                }
            }
            Err(io::Error::new(io::ErrorKind::TimedOut, "The redirected descendant still holds its exit witness"))
        }

        #[tokio::test]
        async fn redirected_descendant_past_recipe_grace_fails_and_is_stopped() -> io::Result<()> {
            let files = FixtureFiles::new()?;
            let witness = File::create_new(files.0.join(EXIT_WITNESS))?;
            let (process, mut output) = start_fixture("redirected-leader", Some(&files.0)).await?;
            let held = witness.try_lock();
            if !matches!(&held, Err(TryLockError::WouldBlock)) {
                let _ = process.stop();
                return Err(io::Error::other(format!("The descendant did not hold its exit witness: {held:?}")));
            }
            let mut log = Vec::new();
            let result = finish_recipe(process, Ok(()), &mut log, &mut output, Duration::from_millis(400)).await;
            if let Err(error) = wait_for_exit_witness(&witness).await {
                // Let this fixture exit if cancellation regresses. The test still fails.
                let _ = fs::write(files.0.join("release"), b"release");
                let _ = wait_for_exit_witness(&witness).await;
                return Err(error);
            }
            assert_eq!(
                result.as_ref().map_err(String::as_str),
                Err("The build recipe did not finish its process tree before the deadline; inspect build.log")
            );
            assert_eq!(witness.metadata()?.len(), 0, "the descendant should not have returned through the fixture gate");
            assert!(!files.0.join("complete").try_exists()?);
            Ok(())
        }
    }

    fn output_directory(checkout: &Path, path: &str) -> Result<PathBuf> {
        let mut directory_path = checkout.to_path_buf();
        for part in path.split('/') {
            directory_path.push(part);
            let _ = directory(&directory_path, "editor artifact output")?;
        }
        Ok(directory_path)
    }

    fn inventory(site: &Path) -> Result<Vec<Entry>> {
        let mut directories = vec![(site.to_path_buf(), String::new(), 0)];
        let mut seen = HashSet::new();
        let mut files = Vec::new();
        let mut total = 0_u64;
        while let Some((dir, prefix, depth)) = directories.pop() {
            let entries = fs::read_dir(dir).map_err(|_| String::from("Cannot read the editor artifact directory"))?;
            for entry in entries {
                let entry = entry.map_err(|_| String::from("Cannot inspect an editor artifact"))?;
                let name = entry.file_name().into_string().map_err(|_| "The editor artifact name is not portable")?;
                let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
                if depth >= MAX_ARTIFACT_DEPTH || !valid_path(&path) || seen.len() >= MAX_ENTRIES || !seen.insert(path.to_ascii_lowercase()) {
                    return Err("The editor artifact inventory is invalid or too large".into());
                }
                let metadata = fs::symlink_metadata(entry.path()).map_err(|_| "Cannot inspect editor artifact metadata")?;
                if is_link(&metadata) {
                    return Err("Editor artifacts may not contain links".into());
                }
                if metadata.is_dir() {
                    directories.push((entry.path(), path, depth + 1));
                    continue;
                }
                if !metadata.is_file() || files.len() >= MAX_FILES || metadata.len() > MAX_FILE || metadata.len() > MAX_SITE.saturating_sub(total) {
                    return Err("The editor artifact exceeds a size or file-count limit".into());
                }
                let bytes = read_bounded(&entry.path(), MAX_FILE)?;
                if bytes.len() as u64 != metadata.len() {
                    return Err("An editor artifact changed during inventory".into());
                }
                total += metadata.len();
                files.push(Entry { path, size: metadata.len(), sha256: format!("{:x}", Sha256::digest(&bytes)) });
            }
        }
        if !files.iter().any(|entry| entry.path == "index.html" && entry.size > 0) {
            return Err("The editor site must contain a nonempty index.html".into());
        }
        files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok(files)
    }

    fn executable(path: &Path) -> Result<PathBuf> {
        if !path.is_absolute() {
            return Err("Build and Git programs must be configured as absolute paths".into());
        }
        file_path(path, "configured executable")
    }

    fn directory(path: &Path, label: &str) -> Result<PathBuf> {
        let metadata = fs::symlink_metadata(path).map_err(|_| format!("Cannot inspect {label}"))?;
        if !metadata.is_dir() || is_link(&metadata) {
            return Err(format!("{label} must be a regular directory"));
        }
        path.canonicalize().map_err(|_| format!("Cannot resolve {label}"))
    }

    fn file_path(path: &Path, label: &str) -> Result<PathBuf> {
        regular_file(path)?;
        path.canonicalize().map_err(|_| format!("Cannot resolve {label}"))
    }

    fn regular_file(path: &Path) -> Result<Metadata> {
        let metadata = fs::symlink_metadata(path).map_err(|_| String::from("Cannot inspect a configured file"))?;
        if !metadata.is_file() || is_link(&metadata) {
            return Err("The configured file must be regular".into());
        }
        Ok(metadata)
    }

    fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
        if regular_file(path)?.len() > max {
            return Err("The input file exceeds its size limit".into());
        }
        let file = File::open(path).map_err(|_| "Cannot open the configured file")?;
        let mut bytes = Vec::new();
        file.take(max + 1).read_to_end(&mut bytes).map_err(|_| "Cannot read the configured file")?;
        if bytes.len() as u64 > max {
            return Err("The input file exceeds its size limit".into());
        }
        Ok(bytes)
    }

    fn hide_window(command: &mut Command) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(crate::recipe_process::CREATE_NO_WINDOW);
        }
        #[cfg(not(windows))]
        let _ = command;
    }
}
