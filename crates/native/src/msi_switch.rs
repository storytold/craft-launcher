//! A confirmed MSI downgrade prepares both verified installers before removing anything.
//! Windows Installer owns each individual operation; recovery is a separate best-effort install.

use std::path::{Path, PathBuf};
use std::time::Duration;

use artcraft_launcher_core::protocol::JobPhase;
use artcraft_launcher_core::{Asset, CraftApp, Installed, Release, version};
use serde::Deserialize;

use crate::http::Verified;
use crate::install::{self, Job, windows};
use crate::process;

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    publisher: String,
    version: String,
    product: String,
    upgrade: String,
    scope: String,
    architecture: String,
    directory: String,
}

const READ_PACKAGE: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$installer = New-Object -ComObject WindowsInstaller.Installer
$db = $installer.OpenDatabase($env:ARTCRAFT_MSI_FILE, 0)
function Read-Property($name) {
    $view = $db.OpenView("SELECT ``Value`` FROM ``Property`` WHERE ``Property``='$name'")
    $null = $view.Execute()
    $record = $view.Fetch()
    if ($null -eq $record) { return '' }
    return $record.StringData(1)
}
$directory = ''
foreach ($name in @('INSTALLFOLDER', 'INSTALLDIR')) {
    $view = $db.OpenView("SELECT ``Directory`` FROM ``Directory`` WHERE ``Directory``='$name'")
    $null = $view.Execute()
    if ($null -ne $view.Fetch()) { $directory = $name; break }
}
[ordered]@{
    name = Read-Property 'ProductName'
    publisher = Read-Property 'Manufacturer'
    version = Read-Property 'ProductVersion'
    product = Read-Property 'ProductCode'
    upgrade = Read-Property 'UpgradeCode'
    scope = Read-Property 'ALLUSERS'
    architecture = $db.SummaryInformation(0).Property(7)
    directory = $directory
} | ConvertTo-Json -Compress
"#;

fn read_package(file: &Path) -> Result<Package, String> {
    let mut command = process::command("powershell.exe");
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", READ_PACKAGE]).env("ARTCRAFT_MSI_FILE", file);
    let output = process::capture_command(command, Duration::from_secs(20)).ok_or("Could not inspect the MSI package. Nothing was removed.")?;
    serde_json::from_str(&output).map_err(|_| "Could not inspect the MSI package. Nothing was removed.".into())
}

fn guid(value: &str) -> bool {
    value.len() == 38
        && value.bytes().enumerate().all(|(i, b)| match i {
            0 => b == b'{',
            37 => b == b'}',
            9 | 14 | 19 | 24 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

fn validate_packages(
    app: &CraftApp,
    target: &Package,
    original: &Package,
    release: &Release,
    recovery: &Release,
    existing: &Installed,
) -> Result<(), String> {
    let valid = |package: &Package, release: &Release| {
        package.name == app.name
            && package.publisher == "Learning Machines LLC"
            && version::parse(&package.version) == release.version
            && release.version.is_some()
            && guid(&package.product)
            && guid(&package.upgrade)
            && matches!(package.directory.as_str(), "INSTALLFOLDER" | "INSTALLDIR")
    };
    if !valid(target, release)
        || !valid(original, recovery)
        || !target.upgrade.eq_ignore_ascii_case(&original.upgrade)
        || target.scope != original.scope
        || target.architecture != original.architecture
        || target.directory != original.directory
        || target.product.eq_ignore_ascii_case(&original.product)
        || existing.package.as_ref().is_none_or(|code| !code.eq_ignore_ascii_case(&original.product))
        || existing.version != recovery.version
    {
        return Err("The MSI packages do not match the installed app. Nothing was removed.".into());
    }
    Ok(())
}

fn verified_download(release: &Release, asset: &Asset, job: &mut Job<'_>) -> Result<PathBuf, String> {
    if asset.sha256.is_none() && release.sums_asset().is_none() {
        return Err("Both MSI releases need published checksums before downgrading.".into());
    }
    let (file, verified) = install::download(release, asset, job)?;
    if verified != Verified::Checksum {
        return Err("Both MSI releases need published checksums before downgrading.".into());
    }
    Ok(file)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install(
    app: &CraftApp,
    release: &Release,
    asset: &Asset,
    recovery: &Release,
    recovery_asset: &Asset,
    existing: &Installed,
    job: &mut Job<'_>,
) -> Result<(), String> {
    let exe = Path::new(&existing.location);
    if process::is_running(exe) {
        return Err(format!("{} is running. Quit it, then try again.", app.name));
    }
    let target_download = verified_download(release, asset, job)?;
    let original_download = verified_download(recovery, recovery_asset, job)?;
    let target = read_package(&target_download)?;
    let original = read_package(&original_download)?;
    validate_packages(app, &target, &original, release, recovery, existing)?;
    let folder =
        exe.parent().filter(|p| p.is_absolute() && p.is_dir()).ok_or("The installed MSI folder could not be verified. Nothing was removed.")?;
    // Validate the raw MSI property before any uninstall can begin.
    directory_argument(&original.directory, folder)?;
    let recovery_root = job.paths.data.join("msi-recovery");
    std::fs::create_dir_all(&recovery_root).map_err(|e| format!("couldn't create the MSI recovery folder ({e})"))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let recovery_dir = recovery_root.join(format!("{}-{stamp}", app.id));
    std::fs::create_dir(&recovery_dir).map_err(|e| format!("couldn't create the MSI recovery folder ({e})"))?;
    let target_file = recovery_dir.join("target.msi");
    let original_file = recovery_dir.join("original.msi");
    recovery_copy(&target_download, &target_file)?;
    recovery_copy(&original_download, &original_file)?;
    let journal = recovery_dir.join("recovery.json");
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "app": app.id, "original": original_file, "target": target_file,
        "original_version": recovery.tag, "target_version": release.tag,
        "original_product_code": original.product, "target_product_code": target.product,
        "folder": folder, "directory_property": original.directory,
    }))
    .map_err(|e| e.to_string())?;
    crate::paths::write_atomic(&journal, &bytes)?;
    let mut driver = WindowsSwitch {
        app,
        target: &target,
        original: &original,
        target_file: &target_file,
        original_file: &original_file,
        exe,
        folder,
        log: recovery_dir.join(format!("{}.log", app.id)),
        started: false,
        original_safe: false,
    };
    // Recheck immediately before removal. The installer lock covers removal, installation
    // and recovery together; cancellation only applies before the first destructive step.
    let result = windows::in_turn(job.stop, &mut *job.phase, JobPhase::Installing, || {
        driver.verify_original()?;
        if process::is_running(exe) {
            return Err(format!("{} is running. Quit it, then try again.", app.name));
        }
        job.stop.check()?;
        replace(&mut driver)
    });
    if result.is_ok() {
        for file in [&target_file, &original_file, &target_download, &original_download, &journal] {
            let _ = std::fs::remove_file(file);
        }
    }
    if result.is_err() && (!driver.started || driver.original_safe) {
        // No removal happened, or recovery was independently confirmed. Keep downloads
        // for a retry and the installer log; redundant recovery copies can go.
        for file in [&target_file, &original_file, &journal] {
            let _ = std::fs::remove_file(file);
        }
        if !driver.started {
            let _ = std::fs::remove_dir(&recovery_dir);
        }
    }
    result.map_err(|error| {
        if !driver.started || error == artcraft_launcher_core::protocol::CANCELLED || error == artcraft_launcher_core::protocol::PAUSED {
            error
        } else if driver.original_safe {
            if driver.log.exists() { format!("{error}\nMSI log: {}", driver.log.display()) } else { error }
        } else {
            format!("{error}\nMSI recovery files: {}", recovery_dir.display())
        }
    })
}

fn recovery_copy(source: &Path, destination: &Path) -> Result<(), String> {
    let mut created = false;
    let result = (|| {
        let mut input = std::fs::File::open(source)?;
        let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(destination)?;
        created = true;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()
    })();
    if let Err(error) = result {
        if created {
            let _ = std::fs::remove_file(destination);
        }
        return Err(format!("couldn't save a recovery MSI; nothing was removed ({error})"));
    }
    if crate::http::sha256_file(source)? != crate::http::sha256_file(destination)? {
        let _ = std::fs::remove_file(destination);
        return Err("The recovery MSI copy could not be verified. Nothing was removed.".into());
    }
    Ok(())
}

trait Switch {
    fn remove_original(&mut self) -> Result<(), String>;
    fn install_target(&mut self) -> Result<(), String>;
    fn verify_target(&mut self) -> Result<(), String>;
    fn restore_original(&mut self) -> Result<(), String>;
    fn verify_original(&mut self) -> Result<(), String>;
}

fn replace(driver: &mut impl Switch) -> Result<(), String> {
    let result = driver.remove_original().and_then(|()| driver.install_target()).and_then(|()| driver.verify_target());
    match result {
        Ok(()) => Ok(()),
        Err(error) => match driver.restore_original().and_then(|()| driver.verify_original()) {
            Ok(()) => Err(format!("MSI downgrade failed; the original version was restored. {error}")),
            Err(recovery) => Err(format!("MSI downgrade failed; recovery also failed. {error}\nRecovery: {recovery}")),
        },
    }
}

struct WindowsSwitch<'a> {
    app: &'a CraftApp,
    target: &'a Package,
    original: &'a Package,
    target_file: &'a Path,
    original_file: &'a Path,
    exe: &'a Path,
    folder: &'a Path,
    log: PathBuf,
    started: bool,
    original_safe: bool,
}

fn directory_argument(property: &str, folder: &Path) -> Result<std::ffi::OsString, String> {
    use std::os::windows::ffi::OsStrExt;

    if !matches!(property, "INSTALLFOLDER" | "INSTALLDIR")
        || !folder.is_absolute()
        || folder.as_os_str().encode_wide().any(|c| matches!(c, 0 | 10 | 13 | 34))
    {
        return Err("The installed MSI folder could not be verified. Nothing was removed.".into());
    }
    // MSI parses PROPERTY="value" itself. Command::arg would quote the entire
    // PROPERTY=value token for spaces, which MSI rejects as an invalid command line.
    let mut argument = std::ffi::OsString::from(format!("{property}=\""));
    argument.push(folder);
    argument.push("\"");
    Ok(argument)
}

impl WindowsSwitch<'_> {
    fn run(&self, operation: &str, file: &std::ffi::OsStr, directory: Option<&str>) -> Result<(), String> {
        let directory = directory.map(|directory| directory_argument(directory, self.folder)).transpose()?;
        crate::windows_msi::run(operation, file, &self.log, directory.as_deref())
    }
    fn verify(&self, package: &Package) -> Result<(), String> {
        let installs = crate::detect::windows::msi_installs(self.app);
        if installs.len() != 1
            || !installs.first().is_some_and(|i| {
                i.product_code.eq_ignore_ascii_case(&package.product)
                    && i.version == version::parse(&package.version)
                    && i.exe.as_deref().is_some_and(|exe| exe == self.exe)
            })
            || crate::detect::windows::executable_version(self.exe) != version::parse(&package.version)
        {
            return Err("The installed MSI version or folder does not match the expected app.".into());
        }
        Ok(())
    }
}

impl Switch for WindowsSwitch<'_> {
    fn remove_original(&mut self) -> Result<(), String> {
        self.started = true;
        self.original_safe = false;
        self.run("/x", std::ffi::OsStr::new(&self.original.product), None)
    }
    fn install_target(&mut self) -> Result<(), String> {
        self.run("/i", self.target_file.as_os_str(), Some(&self.target.directory))
    }
    fn verify_target(&mut self) -> Result<(), String> {
        self.verify(self.target)
    }
    fn restore_original(&mut self) -> Result<(), String> {
        // A cancelled removal may already have rolled back inside Windows Installer.
        if self.verify_original().is_ok() {
            return Ok(());
        }
        self.run("/i", self.original_file.as_os_str(), Some(&self.original.directory))
    }
    fn verify_original(&mut self) -> Result<(), String> {
        let result = self.verify(self.original);
        self.original_safe = result.is_ok();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msi_directory_property_quotes_only_the_value_and_rejects_injection() {
        for property in ["INSTALLFOLDER", "INSTALLDIR"] {
            for folder in [r"C:\Program Files\PhotoCraft", "D:\\Apps\\Übung 日本語\\", r"D:\Apps\PhotoCraft"] {
                assert_eq!(directory_argument(property, Path::new(folder)).unwrap(), std::ffi::OsString::from(format!("{property}=\"{folder}\"")));
            }
        }
        for folder in ["relative", "C:\\Apps\\bad\" /x product", "C:\\Apps\\bad\n", "C:\\Apps\\bad\r", "C:\\Apps\\bad\0"] {
            assert!(directory_argument("INSTALLDIR", Path::new(folder)).is_err());
        }
        assert!(directory_argument("INSTALLDIR=bad /x", Path::new(r"C:\Apps")).is_err());
    }

    #[test]
    fn windows_installer_accepts_directory_property_with_spaces() {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        use std::time::Instant;

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nonexistent.msi");
        assert!(!missing.exists());
        let log = dir.path().join("probe.log");
        let mut command = process::command(windows::msiexec());
        command.arg("/i").arg(&missing).args(["/qn", "/norestart", "/l*v+"]).arg(&log);
        command.raw_arg(directory_argument("INSTALLFOLDER", Path::new(r"C:\Program Files\PhotoCraft")).unwrap());
        let mut child = command.stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("MSI command parsing hung before opening the nonexistent package");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // 1619 means the parser accepted the property and tried to open the missing
        // package. No product exists to install, uninstall, or prompt for elevation.
        assert_eq!(status.code(), Some(1619));
        assert!(log.is_file());
    }

    struct Fake {
        fail: &'static str,
        recovery_fails: bool,
        steps: Vec<&'static str>,
    }
    impl Fake {
        fn step(&mut self, name: &'static str) -> Result<(), String> {
            self.steps.push(name);
            if name == self.fail || self.recovery_fails && name == "restore" { Err(name.into()) } else { Ok(()) }
        }
    }
    impl Switch for Fake {
        fn remove_original(&mut self) -> Result<(), String> {
            self.step("remove")
        }
        fn install_target(&mut self) -> Result<(), String> {
            self.step("install")
        }
        fn verify_target(&mut self) -> Result<(), String> {
            self.step("verify-target")
        }
        fn restore_original(&mut self) -> Result<(), String> {
            self.step("restore")
        }
        fn verify_original(&mut self) -> Result<(), String> {
            self.step("verify-original")
        }
    }

    #[test]
    fn recovery_copies_are_complete_and_never_overwrite_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("download.msi");
        let destination = dir.path().join("original.msi");
        std::fs::write(&source, b"verified installer bytes").unwrap();
        recovery_copy(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"verified installer bytes");
        std::fs::write(&source, b"different download").unwrap();
        assert!(recovery_copy(&source, &destination).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"verified installer bytes");
    }

    #[test]
    fn failed_or_cancelled_msi_steps_restore_and_verify_the_original() {
        for fail in ["remove", "install", "verify-target"] {
            let mut fake = Fake { fail, recovery_fails: false, steps: vec![] };
            assert!(replace(&mut fake).unwrap_err().contains("the original version was restored"));
            assert!(fake.steps.ends_with(&["restore", "verify-original"]));
            if fail == "remove" {
                assert!(!fake.steps.contains(&"install"));
            }
        }
        let mut fake = Fake { fail: "", recovery_fails: false, steps: vec![] };
        assert!(replace(&mut fake).is_ok());
        assert_eq!(fake.steps, ["remove", "install", "verify-target"]);
    }

    #[test]
    fn failed_recovery_never_claims_the_original_was_restored() {
        let mut fake = Fake { fail: "install", recovery_fails: true, steps: vec![] };
        let error = replace(&mut fake).unwrap_err();
        assert!(error.contains("recovery also failed"));
        assert!(!error.contains("was restored"));
        assert!(!fake.steps.contains(&"verify-original"));
    }

    fn package(v: &str, product: &str) -> Package {
        Package {
            name: "PhotoCraft".into(),
            publisher: "Learning Machines LLC".into(),
            version: v.into(),
            product: product.into(),
            upgrade: "{D9660F32-2D0A-4ABE-B73D-17BE25F1542C}".into(),
            scope: "1".into(),
            architecture: "x64;1033".into(),
            directory: "INSTALLFOLDER".into(),
        }
    }

    #[test]
    fn mismatched_recovery_or_target_is_rejected_before_any_removal() {
        let old = package("0.7.0", "{10000000-0000-0000-0000-000000000000}");
        let mut target = package("0.6.0", "{20000000-0000-0000-0000-000000000000}");
        let app = CraftApp::by_id("photocraft").unwrap();
        let existing = Installed {
            version: version::parse("0.7.0"),
            source: artcraft_launcher_core::InstallSource::Msi,
            location: String::new(),
            package: Some(old.product.clone()),
        };
        let release = Release { version: version::parse("0.6.0"), ..Release::default() };
        let recovery = Release { version: version::parse("0.7.0"), ..Release::default() };
        assert!(validate_packages(app, &target, &old, &release, &recovery, &existing).is_ok());
        target.upgrade = "{30000000-0000-0000-0000-000000000000}".into();
        assert!(validate_packages(app, &target, &old, &release, &recovery, &existing).is_err());
        target.upgrade = old.upgrade.clone();
        target.directory.clear();
        assert!(validate_packages(app, &target, &old, &release, &recovery, &existing).is_err());
        target.directory = old.directory.clone();
        target.architecture = "Intel;1033".into();
        assert!(validate_packages(app, &target, &old, &release, &recovery, &existing).is_err());
        assert!(!guid("not-a-product-code"));
    }
}
