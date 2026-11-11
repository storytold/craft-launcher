use std::fs::Metadata;

pub(crate) const MAX_ARTIFACT_DEPTH: usize = 12;
const MAX_PATH: usize = 240;

pub(crate) fn valid_path(path: &str) -> bool {
    if path.len() > MAX_PATH || path.split('/').count() > MAX_ARTIFACT_DEPTH {
        return false;
    }
    path.split('/').all(|part| {
        !part.is_empty()
            && !part.starts_with('.')
            && !part.ends_with('.')
            && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            && !windows_device(part)
    })
}

fn windows_device(part: &str) -> bool {
    let base = part.split('.').next().unwrap_or_default().to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"]
            .iter()
            .any(|prefix| base.strip_prefix(prefix).is_some_and(|suffix| suffix.len() == 1 && suffix.bytes().all(|byte| byte.is_ascii_digit())))
}

pub(crate) fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
