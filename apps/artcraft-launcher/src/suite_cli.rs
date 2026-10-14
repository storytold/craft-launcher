//! Scriptable access to the same verified export as Settings.
use artcraft_launcher_core::Platform;
use artcraft_launcher_native::{
    http::{Http, Stop},
    source::GitHubSource,
    suite,
};
use std::path::Path;
use std::process::ExitCode;

pub fn run(args: &[String]) -> Option<ExitCode> {
    if args.iter().any(|a| a == "--suite-info") {
        let result = (|| {
            let root = suite::root().ok_or("this launcher is not in an offline suite")?;
            let manifest = suite::load(&root, Platform::current().ok_or("unsupported platform")?)?;
            println!("{}", serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?);
            Ok::<_, String>(())
        })();
        return Some(finish(result));
    }
    let index = args.iter().position(|a| a == "--export-suite")?;
    let result = (|| {
        let destination = args
            .get(index + 1)
            .filter(|s| !s.starts_with("--"))
            .ok_or("usage: --export-suite <new absolute folder> [--apps photocraft,wordcraft]")?;
        let ids: Vec<String> = if let Some(index) = args.iter().position(|a| a == "--apps") {
            args.get(index + 1).ok_or("--apps needs a comma-separated list")?.split(',').map(|s| s.trim().to_string()).collect()
        } else {
            artcraft_launcher_core::SUITE.iter().map(|a| a.id.into()).collect()
        };
        let http = Http::new()?;
        let paths = artcraft_launcher_native::paths::Paths::new()?;
        let source = GitHubSource::new(http.clone(), paths.release_cache());
        let mut previous = String::new();
        let folder = suite::export(
            Path::new(destination),
            &ids,
            Platform::current().ok_or("unsupported platform")?,
            &source,
            &http,
            &Stop::default(),
            &mut |app, _| {
                if previous != app {
                    eprintln!("{app}");
                    previous = app.into();
                }
            },
        )?;
        println!("{}", folder.display());
        Ok::<_, String>(())
    })();
    Some(finish(result))
}

fn finish(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ArtCraft Launcher: {e}");
            ExitCode::FAILURE
        }
    }
}
