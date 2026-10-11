//! Quiet Windows Installer operations, with explicit elevation for per-machine packages.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use serde::Deserialize;

use crate::{install::windows, process};

const PREPARE: &str = r#"
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $source = $env:ARTCRAFT_MSI_TARGET
    if ($env:ARTCRAFT_MSI_OPERATION -eq '/x') {
        $source = $installer.ProductInfo($source, 'LocalPackage')
    }
    $database = $installer.OpenDatabase($source, 0)
    $view = $database.OpenView('SELECT `Value` FROM `Property` WHERE `Property`=''ALLUSERS''')
    $null = $view.Execute()
    $record = $view.Fetch()
    $scope = if ($null -eq $record) { '' } else { $record.StringData(1) }
    $null = $view.Close()
    foreach ($object in @($record, $view, $database, $installer)) {
        if ($null -ne $object) { $null = [System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($object) }
    }
    if ($scope -ne '' -and $scope -ne '1') {
        throw 'This MSI uses a mixed installation scope. Use its interactive installer or a portable package.'
    }
    $options = @{
        FilePath = $env:ARTCRAFT_MSI_EXECUTABLE
        ArgumentList = $env:ARTCRAFT_MSI_ARGUMENTS
        WindowStyle = 'Hidden'
        Wait = $true
        PassThru = $true
    }
    if ($scope -eq '1') { $options.Verb = 'RunAs' }
"#;

const FAILURE: &str = r#"
} catch {
    $exception = $_.Exception
    $cancelled = $false
    while ($null -ne $exception) {
        if ($exception -is [System.ComponentModel.Win32Exception] -and $exception.NativeErrorCode -eq 1223) { $cancelled = $true }
        $exception = $exception.InnerException
    }
    if ($cancelled) {
        [ordered]@{ code = 1602; error = $null } | ConvertTo-Json -Compress
    } else {
        [ordered]@{ code = $null; error = $_.Exception.Message } | ConvertTo-Json -Compress
    }
}
"#;

#[derive(Deserialize)]
struct Outcome {
    code: Option<i32>,
    error: Option<String>,
}

fn quoted(value: &OsStr) -> Result<OsString, String> {
    use std::os::windows::ffi::OsStrExt;

    if value.is_empty() || value.encode_wide().any(|c| matches!(c, 0 | 10 | 13 | 34)) {
        return Err("The MSI command contains an invalid path. Nothing was changed.".into());
    }
    let mut argument = OsString::from("\"");
    argument.push(value);
    argument.push("\"");
    Ok(argument)
}

fn product_code(value: &OsStr) -> bool {
    value.to_str().is_some_and(|value| {
        value.len() == 38
            && value.bytes().enumerate().all(|(i, c)| match i {
                0 => c == b'{',
                37 => c == b'}',
                9 | 14 | 19 | 24 => c == b'-',
                _ => c.is_ascii_hexdigit(),
            })
    })
}

pub(crate) fn command_line(operation: &str, target: &OsStr, log: &Path, directory: Option<&OsStr>) -> Result<OsString, String> {
    if !log.is_absolute()
        || match operation {
            "/i" => !Path::new(target).is_absolute(),
            "/x" => !product_code(target),
            _ => true,
        }
    {
        return Err("The MSI command could not be verified. Nothing was changed.".into());
    }
    let mut arguments = OsString::from(operation);
    arguments.push(" ");
    arguments.push(quoted(target)?);
    arguments.push(" /qn /norestart /l*v+ ");
    arguments.push(quoted(log.as_os_str())?);
    if let Some(directory) = directory {
        // This is already a validated PROPERTY="value" argument from msi_switch.
        arguments.push(" ");
        arguments.push(directory);
    }
    Ok(arguments)
}

pub(crate) fn run(operation: &str, target: &OsStr, log: &Path, directory: Option<&OsStr>) -> Result<(), String> {
    let arguments = command_line(operation, target, log, directory)?;
    let script = format!(
        "$ErrorActionPreference = 'Stop'; [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; try {{\n{PREPARE}\n$child = Start-Process @options; [ordered]@{{ code = $child.ExitCode; error = $null }} | ConvertTo-Json -Compress\n{FAILURE}"
    );
    let output = process::command("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script])
        .env("ARTCRAFT_MSI_OPERATION", operation)
        .env("ARTCRAFT_MSI_TARGET", target)
        .env("ARTCRAFT_MSI_ARGUMENTS", arguments)
        .env("ARTCRAFT_MSI_EXECUTABLE", windows::msiexec())
        .output()
        .map_err(|e| format!("couldn't start the Windows Installer ({e})"))?;
    let outcome: Outcome = serde_json::from_slice(&output.stdout).map_err(|_| "The Windows Installer helper returned an unreadable result.")?;
    result(outcome, log)
}

fn result(outcome: Outcome, log: &Path) -> Result<(), String> {
    if let Some(error) = outcome.error {
        return Err(format!("Windows Installer could not start: {error}"));
    }
    windows::explain(outcome.code)
        .map_err(|error| if error == artcraft_launcher_core::protocol::CANCELLED { error } else { format!("{error}\nMSI log: {}", log.display()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::time::{Duration, Instant};

    #[test]
    fn installer_scope_preparation_elevates_only_machine_packages_and_never_launches_them() {
        // A metadata-only database has no files or install sequence. Inspect the real helper's
        // options without starting msiexec, changing installed software, or triggering UAC.
        const FIXTURE: &str = r#"
$ErrorActionPreference = 'Stop'
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase($env:ARTCRAFT_MSI_TARGET, 3)
$view = $database.OpenView('CREATE TABLE `Property` (`Property` CHAR(72) NOT NULL, `Value` CHAR(0) LOCALIZABLE PRIMARY KEY `Property`)')
$null = $view.Execute()
$null = $view.Close()
if ($env:ARTCRAFT_MSI_SCOPE -ne '') {
    $view = $database.OpenView('INSERT INTO `Property` (`Property`, `Value`) VALUES (''ALLUSERS'', ?)')
    $record = $installer.CreateRecord(1)
    $record.StringData(1) = $env:ARTCRAFT_MSI_SCOPE
    $null = $view.Execute($record)
    $null = $view.Close()
}
$null = $database.Commit()
"#;
        let dir = tempfile::tempdir().unwrap();
        for (scope, verb) in [("", ""), ("1", "RunAs"), ("2", "rejected")] {
            let file = dir.path().join(format!("scope-{scope}.msi"));
            let mut fixture = process::command("powershell.exe");
            fixture.args(["-NoProfile", "-NonInteractive", "-Command", FIXTURE]).env("ARTCRAFT_MSI_TARGET", &file).env("ARTCRAFT_MSI_SCOPE", scope);
            let fixture = process::capture_command(fixture, Duration::from_secs(10));
            assert!(fixture.is_some(), "could not create scope {scope} fixture");
            let script = format!(
                "$ErrorActionPreference = 'Stop'; try {{\n{PREPARE}\n[ordered]@{{verb=[string]$options.Verb; args=$options.ArgumentList}} | ConvertTo-Json -Compress\n}} catch {{ [ordered]@{{verb='rejected'; error=$_.Exception.Message}} | ConvertTo-Json -Compress }}"
            );
            let mut inspect = process::command("powershell.exe");
            inspect
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .env("ARTCRAFT_MSI_OPERATION", "/i")
                .env("ARTCRAFT_MSI_TARGET", &file)
                .env("ARTCRAFT_MSI_EXECUTABLE", windows::msiexec())
                .env("ARTCRAFT_MSI_ARGUMENTS", "/qn /norestart");
            let result = process::capture_command(inspect, Duration::from_secs(10)).unwrap();
            let result: serde_json::Value = serde_json::from_str(&result).unwrap();
            assert_eq!(result["verb"], verb, "scope {scope}: {result}");
            if scope != "2" {
                assert_eq!(result["args"], "/qn /norestart");
            }
            if scope.is_empty() {
                // It cannot install: required product metadata and every action sequence are absent.
                // Verify the background process returns the real failure code, rather than success.
                let error = run("/i", file.as_os_str(), &dir.path().join("invalid-package.log"), None).unwrap_err();
                assert!(error.contains("MSI log:"), "{error}");
                assert!(!error.contains("was interrupted"), "missing child exit code: {error}");
            }
        }
    }

    #[test]
    fn helper_reports_missing_package_without_installing_or_elevating() {
        let dir = tempfile::tempdir().unwrap();
        let error = run("/i", dir.path().join("missing.msi").as_os_str(), &dir.path().join("log.txt"), None).unwrap_err();
        assert!(error.contains("Windows Installer could not start:"), "{error}");
        assert!(!dir.path().join("log.txt").exists());
    }

    #[test]
    fn declined_administrator_permission_is_reported_as_cancellation() {
        // Exercise the same exception handling as Start-Process, without opening UAC.
        let script = format!("$ErrorActionPreference = 'Stop'; try {{ throw (New-Object System.ComponentModel.Win32Exception 1223)\n{FAILURE}");
        let result = process::capture("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", &script], Duration::from_secs(10)).unwrap();
        let outcome: Outcome = serde_json::from_str(&result).unwrap();
        assert!(outcome.error.is_none());
        assert_eq!(outcome.code, Some(1602));
        assert_eq!(super::result(outcome, Path::new(r"C:\Cache\install.log")), Err(artcraft_launcher_core::protocol::CANCELLED.into()));
    }

    #[test]
    fn quiet_msi_arguments_are_accepted_by_windows_without_installing_a_package() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing package.msi");
        let log = dir.path().join("installer log.txt");
        let arguments = command_line("/i", missing.as_os_str(), &log, Some(OsStr::new(r#"INSTALLFOLDER="C:\Program Files\PhotoCraft""#))).unwrap();
        let mut command = process::command(windows::msiexec());
        command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        command.raw_arg(arguments);
        let mut child = command.spawn().unwrap();
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("quiet MSI argument parsing did not finish");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.code(), Some(1619));
        assert!(log.is_file());
    }

    #[test]
    fn quiet_msi_rejects_unverified_operation_paths_and_product_codes() {
        let log = Path::new(r"C:\Cache\install.log");
        for (operation, target) in
            [("/i", "relative.msi"), ("/i", "C:\\Cache\\bad\".msi"), ("/x", "{bad} /i other.msi"), ("/i /x", "C:\\Cache\\app.msi")]
        {
            assert!(command_line(operation, OsStr::new(target), log, None).is_err());
        }
        assert!(command_line("/x", OsStr::new("{C10DE45D-077A-4443-961C-8217639B66D2}"), log, None).is_ok());
    }
}
