//! The architecture in a release asset's name. craft-update's `platform` reads the name; this is
//! the one question about it only the launcher asks (which Program Files a fresh MSI goes in).

use crate::platform::{Arch, AssetName};

/// The architecture the asset is built for: the last part of its token (`windows-x86` → x86).
/// `None` for a token that names none (`macos-universal`).
pub fn asset_arch(name: &AssetName) -> Option<Arch> {
    match name.token.rsplit('-').next()? {
        "x64" | "x86_64" => Some(Arch::X64),
        "x86" => Some(Arch::X86),
        "arm64" | "aarch64" => Some(Arch::Arm64),
        "riscv64" => Some(Arch::Riscv64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::parse_asset_name;

    #[test]
    fn asset_names_carry_their_architecture() {
        let arch = |name: &str| asset_arch(&parse_asset_name(name, &["photocraft"]).unwrap());
        assert_eq!(arch("photocraft-0.5.0-windows-x64.msi"), Some(Arch::X64));
        assert_eq!(arch("photocraft-0.5.0-windows-x86.msi"), Some(Arch::X86));
        assert_eq!(arch("photocraft-0.5.0-linux-aarch64.tar.gz"), Some(Arch::Arm64));
        assert_eq!(arch("photocraft-0.5.0-macos-universal.dmg"), None);
    }
}
