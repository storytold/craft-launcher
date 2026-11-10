//! What the system says about which GPU an app should draw with.

/// Windows' per-app choice under Settings → System → Display → Graphics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuPreference {
    /// "Power saving": the integrated GPU.
    PowerSaving,
    /// "High performance": the discrete GPU.
    HighPerformance,
}

/// The choice Windows recorded for this executable, if any. Settings keeps it under
/// `HKCU\Software\Microsoft\DirectX\UserGpuPreferences`, one value per exe path, as
/// `GpuPreference=1;` (power saving) or `GpuPreference=2;` (high performance). `None` for
/// "Let Windows decide" or an exe nothing was recorded for (#399).
#[cfg(windows)]
pub fn windows_gpu_preference() -> Option<GpuPreference> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let exe = std::env::current_exe().ok()?;
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Microsoft\DirectX\UserGpuPreferences").ok()?;
    let value: String = key.get_value(exe.as_os_str()).ok()?;
    parse_gpu_preference(&value)
}

/// Reads one `UserGpuPreferences` value: `key=value;` pairs, of which `GpuPreference` counts.
pub fn parse_gpu_preference(value: &str) -> Option<GpuPreference> {
    value.split(';').map(str::trim).find_map(|pair| pair.strip_prefix("GpuPreference=")).and_then(|n| match n.trim() {
        "1" => Some(GpuPreference::PowerSaving),
        "2" => Some(GpuPreference::HighPerformance),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_windows_gpu_preference_values() {
        assert_eq!(parse_gpu_preference("GpuPreference=1;"), Some(GpuPreference::PowerSaving));
        assert_eq!(parse_gpu_preference("GpuPreference=2;"), Some(GpuPreference::HighPerformance));
        assert_eq!(parse_gpu_preference("SwapEffectUpgradeEnable=1;GpuPreference=1;"), Some(GpuPreference::PowerSaving));
        // "Let Windows decide", an unknown number, and nonsense.
        for value in ["GpuPreference=0;", "GpuPreference=7;", "", "SwapEffectUpgradeEnable=1;", "GpuPreference=", "garbage"] {
            assert_eq!(parse_gpu_preference(value), None, "{value:?}");
        }
    }
}
