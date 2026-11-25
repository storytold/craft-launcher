//! Nightly's private, held-parent filesystem operations. No ambient descriptor paths or unsafe.
//! Unix mutations are fd-relative/no-follow. Windows keeps every directory ancestor open without
//! delete sharing. These bind the operation to validated directory objects, not a pathname check.
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    device: String,
    node: String,
}

#[derive(Clone)]
pub(crate) struct Dir {
    file: Arc<File>,
    path: PathBuf,
    #[cfg(windows)]
    parents: Vec<Arc<File>>,
}

pub(crate) fn relative(path: &Path) -> Result<(), String> {
    let text = path.to_str().ok_or("A Nightly path is not textual.")?;
    if text.is_empty()
        || (text.contains([':', '\0']) || (cfg!(unix) && text.contains('\\')))
        || path.components().any(|part| !matches!(part, Component::Normal(name) if name.to_str().is_some_and(|name| !name.ends_with(['.', ' ']))))
        || text.split('/').any(|part| part.is_empty() || part == "." || part == ".." || part.ends_with(['.', ' ']))
    {
        return Err("A Nightly transaction path is unsafe.".into());
    }
    Ok(())
}

fn name(path: &Path) -> Result<(), String> {
    relative(path)?;
    if path.components().count() != 1 {
        return Err("A Nightly operation needs one child name.".into());
    }
    Ok(())
}

#[cfg(unix)]
fn unix_identity(stat: &rustix::fs::Stat) -> Identity {
    Identity { device: stat.st_dev.to_string(), node: stat.st_ino.to_string() }
}

#[cfg(windows)]
fn windows_info(path: &Path) -> Result<(Identity, bool), String> {
    use winsafe::{HFILE, co};
    let (file, _) = HFILE::CreateFile(
        path.to_str().ok_or("A Nightly path cannot be represented without loss.")?,
        co::GENERIC::READ,
        Some(co::FILE_SHARE::READ | co::FILE_SHARE::WRITE | co::FILE_SHARE::DELETE),
        None,
        co::DISPOSITION::OPEN_EXISTING,
        co::FILE_ATTRIBUTE::NORMAL,
        Some(co::FILE_FLAG::OPEN_REPARSE_POINT | co::FILE_FLAG::BACKUP_SEMANTICS),
        None,
        None,
    )
    .map_err(|e| e.to_string())?;
    let info = file.GetFileInformationByHandle().map_err(|e| e.to_string())?;
    if info.dwFileAttributes.has(co::FILE_ATTRIBUTE::REPARSE_POINT) {
        return Err("A Nightly transaction encountered a reparse point.".into());
    }
    let directory = info.dwFileAttributes.has(co::FILE_ATTRIBUTE::DIRECTORY);
    if !directory && info.nNumberOfLinks != 1 {
        return Err("A Nightly transaction encountered a shared file identity.".into());
    }
    Ok((Identity { device: info.dwVolumeSerialNumber.to_string(), node: info.nFileIndex().to_string() }, directory))
}

impl Dir {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() || path.components().any(|part| matches!(part, Component::CurDir | Component::ParentDir)) {
            return Err("A Nightly directory is not absolute and unambiguous.".into());
        }
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let file: File = rustix::fs::open("/", OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
                .map_err(|e| e.to_string())?
                .into();
            let mut directory = Self { file: Arc::new(file), path: PathBuf::from("/") };
            for part in path.components() {
                if let Component::Normal(part) = part {
                    directory = directory.child(Path::new(part))?;
                }
            }
            Ok(directory)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let mut parents = Vec::new();
            let mut result = None;
            for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
                if ancestor.as_os_str().is_empty() {
                    continue;
                }
                let file =
                    File::options().read(true).share_mode(3).custom_flags(0x0200_0000 | 0x0020_0000).open(ancestor).map_err(|e| e.to_string())?;
                let (_, directory) = windows_info(ancestor)?;
                if !directory {
                    return Err("A Nightly ancestor is not a directory.".into());
                }
                let file = Arc::new(file);
                result = Some(Self { file: file.clone(), path: ancestor.to_path_buf(), parents: parents.clone() });
                parents.push(file);
            }
            result.ok_or_else(|| "A Nightly directory has no filesystem root.".into())
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("No Nightly filesystem adapter is available.".into())
        }
    }

    pub(crate) fn ensure(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() || path.components().any(|part| matches!(part, Component::CurDir | Component::ParentDir)) {
            return Err("An ambiguous Nightly directory was refused.".into());
        }
        let first = path.ancestors().last().ok_or("Missing filesystem root.")?;
        let mut directory = Self::open(first)?;
        for component in path.components() {
            if let Component::Normal(component) = component {
                let name = Path::new(component);
                directory = match directory.info(name)? {
                    Some((_, true)) => directory.child(name)?,
                    None => directory.mkdir(name)?,
                    _ => return Err("A Nightly namespace contains a non-directory.".into()),
                };
            }
        }
        Ok(directory)
    }

    pub(crate) fn child(&self, child: &Path) -> Result<Self, String> {
        name(child)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let file: File =
                rustix::fs::openat(&*self.file, child, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
                    .map_err(|e| e.to_string())?
                    .into();
            Ok(Self { file: Arc::new(file), path: self.path.join(child) })
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let path = self.path.join(child);
            let file = File::options().read(true).share_mode(3).custom_flags(0x0200_0000 | 0x0020_0000).open(&path).map_err(|e| e.to_string())?;
            if !windows_info(&path)?.1 {
                return Err("A Nightly child is not a directory.".into());
            }
            let mut parents = self.parents.clone();
            parents.push(self.file.clone());
            Ok(Self { file: Arc::new(file), path, parents })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("No Nightly filesystem adapter is available.".into())
        }
    }

    pub(crate) fn walk(&self, path: &Path) -> Result<Self, String> {
        if path.as_os_str().is_empty() {
            return Ok(self.clone());
        }
        relative(path)?;
        let mut directory = self.clone();
        for part in path.components() {
            directory = directory.child(Path::new(part.as_os_str()))?;
        }
        Ok(directory)
    }

    pub(crate) fn id(&self) -> Result<Identity, String> {
        #[cfg(unix)]
        {
            Ok(unix_identity(&rustix::fs::fstat(&*self.file).map_err(|e| e.to_string())?))
        }
        #[cfg(windows)]
        {
            Ok(windows_info(&self.path)?.0)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("No Nightly filesystem adapter is available.".into())
        }
    }

    pub(crate) fn info(&self, child: &Path) -> Result<Option<(Identity, bool)>, String> {
        name(child)?;
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, FileType};
            let stat = match rustix::fs::statat(&*self.file, child, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(error) => return Err(error.to_string()),
            };
            let kind = FileType::from_raw_mode(stat.st_mode);
            if kind != FileType::Directory && (kind != FileType::RegularFile || stat.st_nlink != 1) {
                return Err("A Nightly transaction encountered an alias, shared or non-regular file.".into());
            }
            Ok(Some((unix_identity(&stat), kind == FileType::Directory)))
        }
        #[cfg(windows)]
        {
            let path = self.path.join(child);
            match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.to_string()),
                Ok(_) => windows_info(&path).map(Some),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("No Nightly filesystem adapter is available.".into())
        }
    }

    pub(crate) fn file(&self, child: &Path, create: bool) -> Result<File, String> {
        name(child)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let access = if create { OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL } else { OFlags::RDONLY };
            let file: File =
                rustix::fs::openat(&*self.file, child, access | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK, Mode::from_raw_mode(0o600))
                    .map_err(|e| e.to_string())?
                    .into();
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("A Nightly transaction file is not regular.".into());
            }
            Ok(file)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let path = self.path.join(child);
            let file = File::options()
                .read(!create)
                .write(create)
                .create_new(create)
                .share_mode(3)
                .custom_flags(0x0020_0000)
                .open(&path)
                .map_err(|e| e.to_string())?;
            if windows_info(&path)?.1 {
                return Err("A Nightly transaction file is not regular.".into());
            }
            Ok(file)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("No Nightly filesystem adapter is available.".into())
        }
    }

    pub(crate) fn mkdir(&self, child: &Path) -> Result<Self, String> {
        name(child)?;
        #[cfg(unix)]
        {
            rustix::fs::mkdirat(&*self.file, child, rustix::fs::Mode::from_raw_mode(0o700)).map_err(|e| e.to_string())?;
        }
        #[cfg(windows)]
        {
            std::fs::create_dir(self.path.join(child)).map_err(|e| e.to_string())?;
        }
        self.child(child)
    }

    pub(crate) fn move_to(&self, child: &Path, target: &Self, destination: &Path, expected: &Identity) -> Result<(), String> {
        name(child)?;
        name(destination)?;
        if self.info(child)?.as_ref().map(|(id, _)| id) != Some(expected) {
            return Err("A Nightly source identity changed before moving; it was left unchanged.".into());
        }
        self.rename_exclusive(child, target, destination)?;
        #[cfg(test)]
        review_after_exclusive_move();
        if target.info(destination)?.as_ref().map(|(id, _)| id) != Some(expected) {
            // A rename by name cannot prove which object it moves, so whatever now has the
            // destination name stays there; recovery keeps the failure for inspection.
            return Err("A Nightly file changed while moving; the entry now at its destination was left in place and recovery was retained.".into());
        }
        Ok(())
    }

    fn rename_exclusive(&self, child: &Path, target: &Self, destination: &Path) -> Result<(), String> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            rustix::fs::renameat_with(&*self.file, child, &*target.file, destination, rustix::fs::RenameFlags::NOREPLACE).map_err(|e| e.to_string())
        }
        #[cfg(windows)]
        {
            winsafe::MoveFileEx(
                self.path.join(child).to_str().ok_or("A Nightly source path cannot be represented without loss.")?,
                Some(target.path.join(destination).to_str().ok_or("A Nightly target path cannot be represented without loss.")?),
                winsafe::co::MOVEFILE::WRITE_THROUGH,
            )
            .map_err(|e| e.to_string())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            Err("Exclusive Nightly moves are unavailable on this platform.".into())
        }
    }

    pub(crate) fn remove(&self, child: &Path, expected: &Identity, directory: bool) -> Result<(), String> {
        name(child)?;
        match self.info(child)? {
            None => return Ok(()),
            Some((id, kind)) if &id == expected && kind == directory => {}
            _ => return Err("An arriving Nightly object was left unchanged.".into()),
        }
        #[cfg(unix)]
        {
            let flags = if directory { rustix::fs::AtFlags::REMOVEDIR } else { rustix::fs::AtFlags::empty() };
            rustix::fs::unlinkat(&*self.file, child, flags).map_err(|e| e.to_string())
        }
        #[cfg(windows)]
        {
            if directory { std::fs::remove_dir(self.path.join(child)) } else { std::fs::remove_file(self.path.join(child)) }
                .map_err(|e| e.to_string())
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("No Nightly filesystem adapter is available.".into())
        }
    }

    pub(crate) fn prune(&self, child: &Path, expected: &Identity) -> Result<bool, String> {
        name(child)?;
        match self.info(child)? {
            None => return Ok(true),
            Some((id, true)) if &id == expected => {}
            _ => return Err("An arriving Nightly directory was preserved.".into()),
        }
        #[cfg(unix)]
        {
            match rustix::fs::unlinkat(&*self.file, child, rustix::fs::AtFlags::REMOVEDIR) {
                Ok(()) => Ok(true),
                Err(rustix::io::Errno::NOTEMPTY | rustix::io::Errno::EXIST) => Ok(false),
                Err(error) => Err(error.to_string()),
            }
        }
        #[cfg(windows)]
        {
            match std::fs::remove_dir(self.path.join(child)) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(false),
                Err(error) => Err(error.to_string()),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("No Nightly filesystem adapter is available.".into())
        }
    }

    pub(crate) fn sync(&self) -> Result<(), String> {
        #[cfg(test)]
        crate::nightly_transaction::review_directory_sync(self.path())?;
        #[cfg(unix)]
        {
            self.file.sync_all().map_err(|e| e.to_string())
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn update(&self, child: &Path, create: bool) -> Result<File, String> {
        name(child)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let file: File = rustix::fs::openat(
                &*self.file,
                child,
                OFlags::RDWR | if create { OFlags::CREATE } else { OFlags::empty() } | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|e| e.to_string())?
            .into();
            let stat = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
            if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile || stat.st_nlink != 1 {
                return Err("Nightly metadata is not a private regular file.".into());
            }
            Ok(file)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let file = File::options()
                .read(true)
                .write(true)
                .create(create)
                .truncate(false)
                .share_mode(3)
                .custom_flags(0x0020_0000)
                .open(self.path.join(child))
                .map_err(|e| e.to_string())?;
            if windows_info(&self.path.join(child))?.1 {
                return Err("Nightly metadata is not a file.".into());
            }
            Ok(file)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("No Nightly filesystem adapter is available.".into())
        }
    }

    pub(crate) fn file_id(&self, child: &Path, file: &File) -> Result<Identity, String> {
        #[cfg(unix)]
        {
            let _ = child;
            Ok(unix_identity(&rustix::fs::fstat(file).map_err(|e| e.to_string())?))
        }
        #[cfg(windows)]
        {
            let _ = file;
            Ok(windows_info(&self.path.join(child))?.0)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (child, file);
            Err("No Nightly filesystem adapter is available.".into())
        }
    }

    pub(crate) fn replace(&self, source: &Path, target: &Path) -> Result<(), String> {
        name(source)?;
        name(target)?;
        #[cfg(unix)]
        {
            rustix::fs::renameat(&*self.file, source, &*self.file, target).map_err(|e| e.to_string())
        }
        #[cfg(windows)]
        {
            // The payload was synced above; std uses replace-existing rename on Windows.
            // Both paths have held no-delete ancestors. This is not a power-loss guarantee.
            std::fs::rename(self.path.join(source), self.path.join(target)).map_err(|e| e.to_string())
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("No Nightly filesystem adapter is available.".into())
        }
    }
}

#[cfg(test)]
thread_local! {
    static REVIEW_AFTER_EXCLUSIVE_MOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}
#[cfg(test)]
fn review_after_exclusive_move() {
    REVIEW_AFTER_EXCLUSIVE_MOVE.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

// Exclusive moves exist only where Nightly installs are staged; elsewhere move_to refuses
// before renaming anything, which this success-path test can't observe.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod move_identity_tests {
    use super::*;

    #[test]
    fn a_destination_replaced_after_the_move_is_never_moved_back_by_name() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::write(root.join("owned"), b"owned bytes").unwrap();
        let dir = Dir::open(&root).unwrap();
        let expected = dir.info(Path::new("owned")).unwrap().unwrap().0;
        let swap = root.clone();
        REVIEW_AFTER_EXCLUSIVE_MOVE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                // Another writer takes the destination name after the exclusive move.
                std::fs::rename(swap.join("destination"), swap.join("moved aside")).unwrap();
                std::fs::write(swap.join("destination"), b"foreign arriving bytes").unwrap();
            }))
        });
        assert!(dir.move_to(Path::new("owned"), &dir, Path::new("destination"), &expected).is_err());
        assert!(!root.join("owned").exists(), "the foreign entry must not be renamed into the owned name");
        assert_eq!(std::fs::read(root.join("destination")).unwrap(), b"foreign arriving bytes");
        assert_eq!(std::fs::read(root.join("moved aside")).unwrap(), b"owned bytes");
    }
}

// Where exclusive moves and Nightly staging don't exist (FreeBSD and other Unix systems), both
// refuse cleanly: nothing is renamed, and the isolation check stops before any install.
#[cfg(all(test, unix, not(any(target_os = "linux", target_os = "macos"))))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn move_to_refuses_and_leaves_the_source_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::write(root.join("owned"), b"owned bytes").unwrap();
        let dir = Dir::open(&root).unwrap();
        let expected = dir.info(Path::new("owned")).unwrap().unwrap().0;
        let error = dir.move_to(Path::new("owned"), &dir, Path::new("destination"), &expected).unwrap_err();
        assert_eq!(error, "Exclusive Nightly moves are unavailable on this platform.");
        assert_eq!(std::fs::read(root.join("owned")).unwrap(), b"owned bytes");
        assert!(!root.join("destination").exists());
    }

    #[test]
    fn nightly_staging_refuses_on_this_platform() {
        let temp = tempfile::tempdir().unwrap();
        let paths =
            crate::paths::Paths::under(&temp.path().canonicalize().unwrap()).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
        assert_eq!(crate::nightly_unix::validate_namespaces(&paths).unwrap_err(), "No nightly isolation contract is staged for this Unix platform.");
    }
}
