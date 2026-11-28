//! Windows Installer automation on the job's worker thread. The system installer owns
//! elevation. The Launcher owns the status UI and waits for the transaction to finish.

use std::io::{BufRead, BufReader};
use std::path::Path;

use winsafe::prelude::oleaut_IDispatch;
use winsafe::{CLSIDFromString, CoCreateInstance, CoInitializeEx, IDispatch, IUnknown, Variant, co};

use super::{MsiScope, explain, msi_batch, msi_folder_argument, msi_path, msi_scope_arguments};

// WindowsInstaller.Installer. Resolve the registered system class by its documented CLSID.
const INSTALLER: &str = "{000C1090-0000-0000-C000-000000000046}";
const NO_DIALOGS_WITH_UAC: i32 = 2 | 512; // INSTALLUILEVEL_NONE | INSTALLUILEVEL_UACONLY

pub(super) fn install(file: &Path, sha256: &str, destination: Option<&Path>, scope: Option<MsiScope>) -> Result<(), String> {
    let msi = msi_path(file)?;
    let _held = msi_batch::hold(&msi, sha256)?;
    let mut properties = vec!["REBOOT=ReallySuppress".to_owned(), "MSIRESTARTMANAGERCONTROL=DisableShutdown".into(), "MSIDISABLERMRESTART=1".into()];
    if let Some(folder) = destination {
        properties.push(msi_folder_argument(folder)?);
    }
    properties.extend(msi_scope_arguments(scope).into_iter().map(str::to_owned));

    let setup_error = |err: &dyn std::fmt::Display| format!("couldn't configure background Windows Installer ({err})");
    let _com = CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE).map_err(|e| setup_error(&e))?;
    let class = CLSIDFromString(INSTALLER).map_err(|e| setup_error(&e))?;
    let installer = CoCreateInstance::<IDispatch>(&class, None::<&IUnknown>, co::CLSCTX::INPROC_SERVER).map_err(|e| setup_error(&e))?;
    installer.invoke_put("UILevel", &Variant::I4(NO_DIALOGS_WITH_UAC)).map_err(|e| setup_error(&e))?;

    // A new log prevents a failed call from interpreting a previous install's result.
    let parent = msi.parent().ok_or("the MSI has no parent folder")?;
    let log = tempfile::Builder::new().prefix("msi-install-").suffix(".log").tempfile_in(parent).map_err(|e| setup_error(&e))?;
    let log = log.into_temp_path();
    let log_value = Variant::Bstr(msi_batch::text(&log)?.into());
    let modes = Variant::Bstr("iwefarucmvop".into());
    installer.invoke_method("EnableLog", &[&modes, &log_value]).map_err(|e| setup_error(&e))?;
    let result = installer.invoke_method("InstallProduct", &[&Variant::Bstr(msi_batch::text(&msi)?.into()), &Variant::Bstr(properties.join(" "))]);
    let _ = installer.invoke_method("EnableLog", &[&modes, &Variant::Bstr(String::new())]);
    if result.is_ok() {
        return Ok(());
    }

    // Automation reports a generic COM failure for MSI errors. Read the engine's terminal
    // status so a rejected UAC prompt, a busy installer and a reboot-required success retain
    // their original meanings. A missing or incomplete log stays an error.
    let code = engine_result(&log);
    let message = match code {
        Some(3010 | 1641) => return Ok(()),
        Some(1602 | 1223) => return Err(artcraft_launcher_core::protocol::CANCELLED.into()),
        Some(code) if code != 0 => explain(Some(code)).err().unwrap_or_else(|| format!("Windows Installer returned {code}")),
        _ => format!("Windows Installer reported an error ({})", result.err().map_or_else(String::new, |error| error.to_string())),
    };
    let path = log.display().to_string();
    if log.keep().is_ok() { Err(format!("{message}. Its log is at {path}")) } else { Err(message) }
}

fn engine_result(log: &Path) -> Option<i32> {
    let log = std::fs::File::open(log).ok()?;
    BufReader::new(log)
        .split(b'\n')
        .map_while(Result::ok)
        .filter_map(|line| {
            let line = String::from_utf8_lossy(&line);
            let (_, suffix) = line.rsplit_once("MainEngineThread is returning ")?;
            suffix.trim().parse().ok()
        })
        .last()
}

#[cfg(test)]
mod tests {
    use super::engine_result;

    #[test]
    fn engine_result_uses_the_final_code_with_non_utf8_messages() {
        let log = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            log.path(),
            b"MSI (s) [time]: MainEngineThread is returning 1603\r\n\xff\r\nMSI (c) [time]: MainEngineThread is returning 1602\r\n",
        )
        .unwrap();
        assert_eq!(engine_result(log.path()), Some(1602));
    }
}
