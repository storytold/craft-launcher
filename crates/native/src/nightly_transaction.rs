//! One recoverable Nightly mutation per app. The fixed preparation slot and append-only
//! ownership log precede payload writes. Program changes begin only after a complete plan is
//! synced. The private manifest, not process lifetime, decides rollback versus committed cleanup.
//! This protects cooperating instances and anchors parents; it is not a same-UID security boundary
//! or a claim that an ordinary process-exit test proves storage-device power-loss behavior.
use crate::manifest::{Manifest, Record};
use crate::nightly_fs::{Dir, Identity, relative};
use crate::nightly_profile::Adapter;
use crate::paths::Paths;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const LIMIT: u64 = 16 * 1024 * 1024;
const NODES: usize = 40_000;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Checkpoint {
    #[cfg(unix)]
    ArchiveExtracted,
    RootReserved,
    BackupsSaved,
    BeforeCommit,
    BeforeProgramCreate,
    BeforeRemoval,
    AfterCommit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    path: PathBuf,
    id: Identity,
    directory: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    version: u32,
    app: String,
    apps: Identity,
    stage: Identity,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    previous: Option<Record>,
    next: Option<Record>,
    root: Identity,
    created_root: bool,
    directories: Vec<Node>,
    new_directories: Vec<Node>,
    old: Vec<Node>,
    new: Vec<Node>,
    directory_moves: Vec<PathBuf>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "value", deny_unknown_fields)]
enum Entry {
    Header(Header),
    Created(Node),
    Metadata(Node),
    Plan(Box<Plan>),
    Clean { committed: Option<bool> },
}

pub(crate) struct Lease {
    data: Dir,
    apps: Dir,
    paths: Paths,
    adapter: Adapter,
    _lock: LockGuard,
    // Only this live lease can reconcile an append it attempted. Restart stays strict.
    unreconciled_append: Mutex<Option<String>>,
}

struct LockGuard(File);
impl Drop for LockGuard {
    fn drop(&mut self) {
        // A concurrent fork can retain our CLOEXEC file description until exec. Retire
        // this operation's lock now, rather than waiting for that last copy to close.
        if let Err(error) = self.0.unlock() {
            log::warn!("couldn't release the private Nightly lock: {error}");
        }
    }
}

fn lock(directory: &Dir, name: &str) -> Result<LockGuard, String> {
    let file = directory.update(Path::new(name), true)?;
    let acquired = file.try_lock();
    #[cfg(all(test, target_os = "linux"))]
    review_tests::diagnose_lock_attempt(&directory.path().join(name), &file, acquired.is_ok());
    acquired.map_err(|error| format!("Another Nightly operation is using this storage. Try again when it finishes: {error}"))?;
    Ok(LockGuard(file))
}

fn read(directory: &Dir, name: &Path) -> Result<Option<Vec<u8>>, String> {
    if directory.info(name)?.is_none() {
        return Ok(None);
    }
    let mut file = directory.file(name, false)?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file).take(LIMIT + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("Nightly metadata exceeds its size limit.".into());
    }
    Ok(Some(bytes))
}

impl Lease {
    pub(crate) fn acquire(paths: &Paths, adapter: Adapter) -> Result<Self, String> {
        adapter.validate_profile(paths)?;
        let data = Dir::ensure(&paths.data)?;
        let apps = Dir::ensure(&paths.apps())?;
        let guard = lock(&data, &format!(".nightly-{}-mutation-v1.lock", adapter.app()))?;
        let lease = Self { data, apps, paths: paths.clone(), adapter, _lock: guard, unreconciled_append: Mutex::new(None) };
        // Loading is strict even without a journal. The lock precedes choosing the old record.
        lease.manifest()?;
        lease.recover()?;
        Ok(lease)
    }

    fn append_ready(&self) -> Result<(), String> {
        match self.unreconciled_append.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            None => Ok(()),
            Some(error) => Err(format!(
                "The live Nightly journal append could not be reconciled; its preparation and journal were preserved at {}: {error}",
                self.paths.data.join(self.journal_name()).display()
            )),
        }
    }
    fn append_entry(&self, file: &mut File, entry: &Entry) -> Result<(), String> {
        self.append_ready()?;
        append(file, entry).map_err(|failure| {
            if failure.unreconciled {
                *self.unreconciled_append.lock().unwrap_or_else(PoisonError::into_inner) = Some(failure.error.clone());
            }
            failure.error
        })
    }

    fn stage_name(&self) -> PathBuf {
        format!(".nightly-{}-preparation-v1", self.adapter.app()).into()
    }
    fn journal_name(&self) -> PathBuf {
        format!(".nightly-{}-journal-v1.jsonl", self.adapter.app()).into()
    }
    fn manifest_name(&self) -> Result<PathBuf, String> {
        self.paths.manifest().file_name().map(PathBuf::from).ok_or_else(|| "Missing Nightly manifest name.".into())
    }
    fn metadata_clear(&self) -> Result<(), String> {
        let name = format!(".nightly-{}-manifest-v1.pending", self.adapter.app());
        let path = self.data.path().join(&name);
        let found = self
            .data
            .info(Path::new(&name))
            .map_err(|error| format!("Nightly metadata at {} could not be inspected and was preserved: {error}", path.display()))?;
        if found.is_some() {
            return Err(format!(
                "An unregistered Nightly metadata object was preserved at {}. Recovery remains pending; inspect and move it aside before retrying. It was not adopted or deleted.",
                path.display()
            ));
        }
        Ok(())
    }
    fn check_namespace(&self) -> Result<(), String> {
        if Dir::open(&self.paths.data)?.id()? != self.data.id()? || Dir::open(&self.paths.apps())?.id()? != self.apps.id()? {
            return Err("Nightly storage changed identity; recovery was retained without adopting its replacement.".into());
        }
        Ok(())
    }
    fn read_manifest(&self) -> Result<Manifest, String> {
        match read(&self.data, &self.manifest_name()?)? {
            None => Ok(Manifest::default()),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("The private Nightly manifest is unreadable; it was not treated as empty: {error}")),
        }
    }
    pub(crate) fn manifest(&self) -> Result<Manifest, String> {
        let _guard = lock(&self.data, ".nightly-manifest-v1.lock")?;
        self.check_namespace()?;
        self.read_manifest()
    }
    pub(crate) fn publish(&self, previous: Option<&Record>, next: Option<&Record>) -> Result<Manifest, String> {
        self.append_ready()?;
        let _guard = lock(&self.data, ".nightly-manifest-v1.lock")?;
        self.check_namespace()?;
        let mut manifest = self.read_manifest()?;
        if manifest.apps.get(self.adapter.app()) != previous {
            return Err("The private Nightly record changed during this operation; recovery was retained.".into());
        }
        if let Some(next) = next {
            manifest.apps.insert(self.adapter.app().into(), next.clone());
        } else {
            manifest.apps.remove(self.adapter.app());
        }
        let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("The private Nightly manifest is too large.".into());
        }
        // The one manifest lock owns this fixed scratch name; unknown leftovers are not adopted.
        let scratch_name = format!(".nightly-{}-manifest-v1.pending", self.adapter.app());
        let scratch = Path::new(&scratch_name);
        let mut file = self.data.file(scratch, true)?;
        #[cfg(test)]
        crate::backend::review_after_manifest_scratch_created();
        let id = self.data.file_id(scratch, &file)?;
        let result = (|| {
            if self.data.info(&self.journal_name())?.is_some() {
                use std::io::{Seek, SeekFrom};
                let mut journal = self.data.update(&self.journal_name(), false)?;
                journal.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
                self.append_entry(&mut journal, &Entry::Metadata(Node { path: scratch.into(), id: id.clone(), directory: false }))?;
            }
            #[cfg(test)]
            crate::backend::review_before_manifest_save();
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            self.data.replace(scratch, &self.manifest_name()?)?;
            #[cfg(test)]
            crate::backend::review_after_manifest_replace()?;
            self.data.sync()
        })();
        if let Err(error) = result {
            let _ = self.data.remove(scratch, &id, false);
            return Err(error);
        }
        Ok(manifest)
    }

    pub(crate) fn prepare(&self) -> Result<Preparation<'_>, String> {
        self.append_ready()?;
        self.check_namespace()?;
        if self.apps.info(&self.stage_name())?.is_some() || self.data.info(&self.journal_name())?.is_some() {
            return Err(format!(
                "An unowned or incomplete Nightly preparation was preserved at {}. No new preparation was created.",
                self.apps.path().join(self.stage_name()).display()
            ));
        }
        // A crash between mkdir and header publication leaves at most this ONE empty slot.
        // Without its identity seal it is deliberately not adopted or recursively deleted.
        let mut file = self.data.file(&self.journal_name(), true)?;
        let journal_id = self.data.file_id(&self.journal_name(), &file)?;
        let stage = match self.apps.mkdir(&self.stage_name()) {
            Ok(stage) => stage,
            Err(error) => {
                drop(file);
                self.data.remove(&self.journal_name(), &journal_id, false)?;
                return Err(error);
            }
        };
        let header = Header { version: 1, app: self.adapter.app().into(), apps: self.apps.id()?, stage: stage.id()? };
        let initialized = (|| {
            self.append_entry(&mut file, &Entry::Header(header.clone()))?;
            self.apps.sync()?;
            self.data.sync()
        })();
        if let Err(error) = initialized {
            drop(file);
            drop(stage);
            self.apps.remove(&self.stage_name(), &header.stage, true)?;
            self.data.remove(&self.journal_name(), &journal_id, false)?;
            return Err(error);
        }
        Ok(Preparation {
            lease: self,
            stage,
            file: Some(file),
            journal_id,
            header,
            entries: BTreeMap::new(),
            metadata: Vec::new(),
            plan: None,
            clean: false,
            finished: None,
        })
    }

    /// `prepare` creates the journal, then the stage, then writes the header. A crash before
    /// the header leaves an empty journal and at most an empty stage: no plan was written and no
    /// program file moved. Only that exact state is cleared; any content is preserved.
    fn recover_headerless_slot(&self) -> Result<(), String> {
        let Some((journal, false)) = self.data.info(&self.journal_name())? else {
            return Err("The empty Nightly journal is not a regular file; it was preserved.".into());
        };
        match self.apps.info(&self.stage_name())? {
            None => {}
            Some((stage, true)) => {
                if !self.apps.prune(&self.stage_name(), &stage)? {
                    return Err(
                        "An unsealed Nightly preparation slot with content was preserved. Its ownership cannot be reconstructed safely.".into()
                    );
                }
            }
            Some(_) => return Err("An unsealed Nightly preparation slot was preserved. Its ownership cannot be reconstructed safely.".into()),
        }
        self.apps.sync()?;
        self.data.remove(&self.journal_name(), &journal, false)?;
        self.data.sync()?;
        self.metadata_clear()
    }

    pub(crate) fn recover(&self) -> Result<(), String> {
        let Some(bytes) = read(&self.data, &self.journal_name())? else {
            if self.apps.info(&self.stage_name())?.is_some() {
                return Err("An unsealed Nightly preparation slot was preserved. Its ownership cannot be reconstructed safely.".into());
            }
            self.metadata_clear()?;
            return Ok(());
        };
        if bytes.is_empty() {
            return self.recover_headerless_slot();
        }
        let mut lines = bytes.split(|byte| *byte == b'\n');
        let header = match serde_json::from_slice(lines.next().ok_or("The Nightly journal is empty.")?).map_err(|e| e.to_string())? {
            Entry::Header(header) => header,
            _ => return Err("The Nightly journal has no ownership header.".into()),
        };
        if header.version != 1 || header.app != self.adapter.app() || header.apps != self.apps.id()? {
            return Err("The Nightly recovery journal belongs to different storage; it was preserved.".into());
        }
        let stage_missing = self.apps.info(&self.stage_name())?.is_none();
        let stage = if stage_missing { self.apps.clone() } else { self.apps.child(&self.stage_name())? };
        if !stage_missing && stage.id()? != header.stage {
            return Err("The Nightly preparation changed identity; its replacement was preserved.".into());
        }
        let journal_id = self.data.info(&self.journal_name())?.ok_or("The Nightly journal disappeared.")?.0;
        let mut preparation = Preparation {
            lease: self,
            stage,
            file: None,
            journal_id,
            header,
            entries: BTreeMap::new(),
            metadata: Vec::new(),
            plan: None,
            clean: false,
            finished: None,
        };
        for line in lines {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice(line).map_err(|e| format!("The Nightly journal is incomplete; recovery was preserved: {e}"))? {
                Entry::Created(node) if preparation.plan.is_none() && !preparation.clean => {
                    validate_node(&node, false)?;
                    if preparation.entries.len() >= NODES || preparation.entries.insert(node.path.clone(), node).is_some() {
                        return Err("The Nightly ownership log has duplicate or excessive entries.".into());
                    }
                }
                Entry::Metadata(node) if preparation.metadata.is_empty() && !preparation.clean => {
                    if node.directory || node.path != format!(".nightly-{}-manifest-v1.pending", self.adapter.app()) {
                        return Err("Invalid Nightly metadata cleanup path.".into());
                    }
                    preparation.metadata.push(node);
                }
                Entry::Plan(plan) if preparation.plan.is_none() && !preparation.clean => {
                    preparation.validate_plan(&plan)?;
                    preparation.plan = Some(*plan);
                }
                Entry::Clean { committed } if !preparation.clean && (preparation.plan.is_some() == committed.is_some()) => {
                    preparation.clean = true;
                    preparation.finished = committed;
                }
                _ => return Err("The Nightly journal has an invalid phase; recovery was preserved.".into()),
            }
        }
        if stage_missing && !preparation.clean && preparation.plan.is_some() {
            return Err("The Nightly preparation disappeared before cleanup completed; recovery was preserved.".into());
        }
        if let Some(plan) = preparation.plan.as_ref() {
            let manifest = self.manifest()?;
            let current = manifest.apps.get(self.adapter.app());
            if preparation.clean {
                let expected = if preparation.finished == Some(true) { plan.next.as_ref() } else { plan.previous.as_ref() };
                if current != expected {
                    return Err("The Nightly manifest changed after cleanup; the journal was preserved.".into());
                }
                preparation.verify_clean()?;
            } else if current == plan.next.as_ref() && current != plan.previous.as_ref() {
                preparation.committed(None)?;
                preparation.finished = Some(true);
            } else if current == plan.previous.as_ref() {
                preparation.finished = Some(false);
                preparation.rollback(None)?;
            } else {
                return Err("The private Nightly record disagrees with its journal; no files were changed.".into());
            }
        }
        if stage_missing {
            for node in &preparation.metadata {
                self.data.remove(&node.path, &node.id, false)?;
            }
            self.metadata_clear()?;
            self.data.remove(&self.journal_name(), &preparation.journal_id, false)?;
            return self.data.sync();
        }
        preparation.cleanup()
    }
}

#[derive(Debug)]
struct AppendFailure {
    error: String,
    unreconciled: bool,
}
impl AppendFailure {
    fn unchanged(error: impl Into<String>) -> Self {
        Self { error: error.into(), unreconciled: false }
    }
}
fn append(file: &mut File, entry: &Entry) -> Result<(), AppendFailure> {
    let mut bytes = serde_json::to_vec(entry).map_err(|e| AppendFailure::unchanged(e.to_string()))?;
    bytes.push(b'\n');
    let limit = {
        #[cfg(test)]
        {
            review_tests::journal_limit().unwrap_or(LIMIT)
        }
        #[cfg(not(test))]
        {
            LIMIT
        }
    };
    // The longest terminal phase must fit in the SAME total bound, for every
    // accepted nonterminal entry, not just Plan/Metadata or an extra cleanup budget.
    let reserve = if matches!(entry, Entry::Clean { .. }) {
        0
    } else {
        serde_json::to_vec(&Entry::Clean { committed: Some(false) }).map_err(|e| AppendFailure::unchanged(e.to_string()))?.len() as u64 + 1
    };
    let previous = file.seek(SeekFrom::End(0)).map_err(|e| AppendFailure::unchanged(e.to_string()))?;
    if previous.saturating_add(bytes.len() as u64).saturating_add(reserve) > limit {
        return Err(AppendFailure::unchanged("The bounded Nightly ownership log is full; terminal cleanup space was retained."));
    }
    let written = (|| {
        #[cfg(test)]
        if let Some(result) = review_tests::injected_append(file, entry, &bytes) {
            return result;
        }
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())
    })();
    if let Err(error) = written {
        // We own this live append and its known preceding complete prefix. This is
        // NOT restart repair: never truncate an arbitrary malformed recovered log.
        let repaired = (|| {
            #[cfg(test)]
            if review_tests::refuse_repair() {
                return Err("injected owned-prefix repair refusal".to_owned());
            }
            file.set_len(previous).map_err(|e| e.to_string())?;
            file.seek(SeekFrom::Start(previous)).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())
        })();
        return Err(match repaired {
            Ok(()) => AppendFailure::unchanged(error),
            Err(repair) => AppendFailure { error: format!("{error}; owned append prefix could not be restored: {repair}"), unreconciled: true },
        });
    }
    Ok(())
}
fn validate_node(node: &Node, root: bool) -> Result<(), String> {
    if !(root && node.path.as_os_str().is_empty() && node.directory) {
        relative(&node.path)?;
    }
    if node.path.as_os_str().len() > 4096 {
        return Err("A Nightly journal path is too long.".into());
    }
    Ok(())
}

pub(crate) struct Preparation<'a> {
    lease: &'a Lease,
    stage: Dir,
    file: Option<File>,
    journal_id: Identity,
    header: Header,
    entries: BTreeMap<PathBuf, Node>,
    metadata: Vec<Node>,
    plan: Option<Plan>,
    clean: bool,
    finished: Option<bool>,
}

impl Preparation<'_> {
    pub(crate) fn path(&self) -> PathBuf {
        self.lease.apps.path().join(self.lease.stage_name())
    }
    fn admit_node(&self, path: &Path) -> Result<(), String> {
        self.lease.append_ready()?;
        self.file.as_ref().ok_or("The preparation is read-only.")?;
        relative(path)?;
        if path.as_os_str().len() > 4096 {
            return Err("A Nightly journal path is too long.".into());
        }
        let limit = {
            #[cfg(test)]
            {
                review_tests::node_limit().unwrap_or(NODES)
            }
            #[cfg(not(test))]
            {
                NODES
            }
        };
        if self.entries.len() >= limit {
            return Err("The Nightly preparation has too many nodes.".into());
        }
        Ok(())
    }
    fn register(&mut self, node: Node) -> Result<(), String> {
        self.admit_node(&node.path)?;
        validate_node(&node, false)?;
        self.lease.append_entry(self.file.as_mut().ok_or("The preparation is read-only.")?, &Entry::Created(node.clone()))?;
        self.entries.insert(node.path.clone(), node);
        Ok(())
    }
    fn registration_failed(directory: &Dir, name: &Path, node: &Node, error: String) -> String {
        // Only the exact captured object created by this live call. Unknown arrivals
        // and recovered unregistered nodes are never adopted or recursively swept.
        match directory.remove(name, &node.id, node.directory).and_then(|()| directory.sync()) {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; the live-created object was preserved at {}: {cleanup}", directory.path().join(name).display()),
        }
    }
    pub(crate) fn directories(&mut self, path: &Path) -> Result<Dir, String> {
        relative(path)?;
        let mut directory = self.stage.clone();
        let mut current = PathBuf::new();
        for part in path.components() {
            let name = Path::new(part.as_os_str());
            current.push(name);
            directory = if let Some(node) = self.entries.get(&current) {
                let child = directory.child(name)?;
                if !node.directory || child.id()? != node.id {
                    return Err("A preparation parent changed identity.".into());
                }
                child
            } else {
                self.admit_node(&current)?;
                let child = directory.mkdir(name)?;
                let node = Node { path: current.clone(), id: child.id()?, directory: true };
                if let Err(error) = self.register(node.clone()) {
                    // Windows holds a no-delete handle: release our own child before removal.
                    drop(child);
                    return Err(Self::registration_failed(&directory, name, &node, error));
                }
                child
            };
        }
        Ok(directory)
    }
    #[cfg(any(unix, test))]
    pub(crate) fn create(&mut self, path: &Path) -> Result<File, String> {
        relative(path)?;
        let parent = path.parent().ok_or("Missing preparation parent.")?;
        let directory = if parent.as_os_str().is_empty() { self.stage.clone() } else { self.directories(parent)? };
        let name = path.file_name().map(Path::new).ok_or("Missing preparation filename.")?;
        self.admit_node(path)?;
        let file = directory.file(name, true)?;
        let node = Node { path: path.into(), id: directory.file_id(name, &file)?, directory: false };
        if let Err(error) = self.register(node.clone()) {
            drop(file);
            return Err(Self::registration_failed(&directory, name, &node, error));
        }
        Ok(file)
    }

    fn record_nodes(&self, record: &Record, root: &Dir) -> Result<(Vec<Node>, Vec<Node>), String> {
        let mut files = Vec::new();
        let mut directories = BTreeMap::new();
        directories.insert(PathBuf::new(), Node { path: PathBuf::new(), id: root.id()?, directory: true });
        let owned = record_root(record, &self.lease.paths.apps().join(self.lease.adapter.app()), self.lease.adapter.executable())?;
        for path in &record.files {
            let path = path.strip_prefix(owned).map_err(|e| e.to_string())?;
            relative(path)?;
            let mut parent = path.parent();
            while let Some(path) = parent {
                let directory = root.walk(path)?;
                directories.insert(path.to_path_buf(), Node { path: path.into(), id: directory.id()?, directory: true });
                parent = path.parent();
            }
            let (directory, name) = location(root, path)?.ok_or("A recorded Nightly parent disappeared.")?;
            let (id, is_dir) = directory.info(&name)?.ok_or("A recorded Nightly file disappeared.")?;
            if is_dir {
                return Err("A recorded Nightly file became a directory.".into());
            }
            files.push(Node { path: path.into(), id, directory: false });
        }
        Ok((files, directories.into_values().collect()))
    }

    pub(crate) fn seal(&mut self, previous: Option<&Record>, next: Option<&Record>) -> Result<(), String> {
        if self.lease.manifest()?.apps.get(self.lease.adapter.app()) != previous {
            return Err("The persisted Nightly record changed before preparing this mutation; no program files were moved.".into());
        }
        let (old, directories, root, created_root) = if let Some(previous) = previous {
            let root = self.lease.apps.child(Path::new(self.lease.adapter.app()))?;
            let (files, directories) = self.record_nodes(previous, &root)?;
            (files, directories, root.id()?, false)
        } else {
            if self.lease.apps.info(Path::new(self.lease.adapter.app()))?.is_some() {
                return Err("An unowned Nightly program folder was preserved.".into());
            }
            let root = self.directories(Path::new("root"))?.id()?;
            (Vec::new(), Vec::new(), root, true)
        };
        for node in &old {
            if let Some(parent) = node.path.parent().filter(|path| !path.as_os_str().is_empty()) {
                self.directories(&Path::new("backup").join(parent))?;
            }
        }
        self.directories(Path::new("backup"))?;
        let mut new = Vec::new();
        let mut new_directories = BTreeMap::new();
        let mut directory_moves = BTreeSet::new();
        if let Some(next) = next {
            let original: BTreeMap<_, _> = directories.iter().map(|node| (node.path.clone(), node)).collect();
            let next_root = record_root(next, &self.lease.paths.apps().join(self.lease.adapter.app()), self.lease.adapter.executable())?;
            for absolute in &next.files {
                let path = absolute.strip_prefix(next_root).map_err(|e| e.to_string())?;
                let staged = self.entries.get(&Path::new("unpacked").join(path)).ok_or("A prepared payload has no owned file identity.")?;
                new.push(Node { path: path.into(), id: staged.id.clone(), directory: false });
                let mut relative_parent = PathBuf::new();
                let mut missing = false;
                for component in path.parent().ok_or("Missing payload parent.")?.components() {
                    relative_parent.push(component);
                    if let Some(expected) = original.get(&relative_parent) {
                        let root = self.lease.apps.child(Path::new(self.lease.adapter.app()))?;
                        if root.walk(&relative_parent)?.id()? != expected.id {
                            return Err("A program parent changed identity.".into());
                        }
                    } else {
                        if !missing {
                            if !created_root
                                && location(&self.lease.apps.child(Path::new(self.lease.adapter.app()))?, &relative_parent)?
                                    .map(|(dir, name)| dir.info(&name))
                                    .transpose()?
                                    .flatten()
                                    .is_some()
                            {
                                return Err("An unowned program parent was preserved.".into());
                            }
                            directory_moves.insert(relative_parent.clone());
                            missing = true;
                        }
                        let id = self.directories(&Path::new("directories").join(&relative_parent))?.id()?;
                        new_directories.insert(relative_parent.clone(), Node { path: relative_parent.clone(), id, directory: true });
                    }
                }
                if !created_root {
                    let root = self.lease.apps.child(Path::new(self.lease.adapter.app()))?;
                    if let Some((directory, name)) = location(&root, path)?
                        && let Some((id, _)) = directory.info(&name)?
                        && !old.iter().any(|old| old.path == path && old.id == id)
                    {
                        return Err("The Nightly update would overwrite an unrecorded object.".into());
                    }
                }
            }
        }
        let plan = Plan {
            previous: previous.cloned(),
            next: next.cloned(),
            root,
            created_root,
            directories,
            new_directories: new_directories.into_values().collect(),
            old,
            new,
            directory_moves: directory_moves.into_iter().collect(),
        };
        self.validate_plan(&plan)?;
        self.stage.sync()?;
        self.lease.append_entry(self.file.as_mut().ok_or("Missing preparation writer.")?, &Entry::Plan(Box::new(plan.clone())))?;
        self.plan = Some(plan);
        Ok(())
    }

    fn validate_plan(&self, plan: &Plan) -> Result<(), String> {
        if plan.old.len() + plan.new.len() + plan.directories.len() + plan.new_directories.len() > NODES
            || plan.created_root != plan.previous.is_none()
        {
            return Err("The Nightly plan is inconsistent or excessive.".into());
        }
        for node in plan.old.iter().chain(&plan.new).chain(&plan.directories).chain(&plan.new_directories) {
            validate_node(node, true)?;
        }
        for path in &plan.directory_moves {
            relative(path)?;
        }
        for (record, nodes) in [(plan.previous.as_ref(), &plan.old), (plan.next.as_ref(), &plan.new)] {
            if let Some(record) = record {
                let root = record_root(record, &self.lease.paths.apps().join(self.lease.adapter.app()), self.lease.adapter.executable())?;
                if record.kind != self.lease.adapter.kind()
                    || record.needs_root
                    || record.path != root.join(self.lease.adapter.executable())
                    || !record.files.contains(&record.path)
                    || record.files.len() != nodes.len()
                    || nodes.iter().any(|node| node.directory || !record.files.contains(&root.join(&node.path)))
                {
                    return Err("The Nightly recovery record does not own the planned files.".into());
                }
                let files: Vec<_> = nodes.iter().map(|node| node.path.clone()).collect();
                #[cfg(unix)]
                crate::nightly_unix::validate_layout(self.lease.adapter, &files)?;
                #[cfg(windows)]
                self.lease.adapter.validate_layout(&files, &files)?;
            } else if !nodes.is_empty() {
                return Err("A Nightly plan has files without a record.".into());
            }
        }
        Ok(())
    }
    fn root(&self, held: Option<&Dir>) -> Result<Option<Dir>, String> {
        let plan = self.plan.as_ref().ok_or("Missing Nightly transaction plan.")?;
        if let Some(held) = held {
            if held.id()? != plan.root {
                return Err("The held Nightly root changed identity.".into());
            }
            return Ok(Some(held.clone()));
        }
        match self.lease.apps.info(Path::new(self.lease.adapter.app()))? {
            None if plan.created_root => Ok(None),
            Some((id, true)) if id == plan.root => self.lease.apps.child(Path::new(self.lease.adapter.app())).map(Some),
            _ => Err("The Nightly program root changed identity; recovery was preserved without touching its replacement.".into()),
        }
    }
    fn check_root(&self) -> Result<(), String> {
        self.lease.check_namespace()?;
        self.root(None)?.ok_or("The Nightly program root disappeared.")?;
        Ok(())
    }
    fn parent(&self, root: &Dir, path: &Path) -> Result<(Dir, PathBuf), String> {
        let plan = self.plan.as_ref().ok_or("Missing Nightly plan.")?;
        let parent = path.parent().ok_or("Missing Nightly parent.")?;
        let mut directory = root.clone();
        let mut current = PathBuf::new();
        for component in parent.components() {
            current.push(component);
            let expected = plan
                .directories
                .iter()
                .chain(&plan.new_directories)
                .find(|node| node.path == current)
                .ok_or("An unowned Nightly parent was preserved.")?;
            directory = directory.child(Path::new(component.as_os_str()))?;
            if directory.id()? != expected.id {
                return Err("A Nightly parent changed identity; it was not adopted.".into());
            }
        }
        Ok((directory, path.file_name().map(PathBuf::from).ok_or("Missing Nightly filename.")?))
    }
    // Only planned-new parents may be absent while undoing new payload. Original parents
    // still use the strict path below, and every present parent must retain its identity.
    fn rollback_parent(&self, root: &Dir, path: &Path) -> Result<Option<(Dir, PathBuf)>, String> {
        let plan = self.plan.as_ref().ok_or("Missing Nightly plan.")?;
        let mut directory = root.clone();
        let mut current = PathBuf::new();
        for component in path.parent().ok_or("Missing Nightly parent.")?.components() {
            current.push(component);
            let original = plan.directories.iter().find(|node| node.path == current);
            let expected = original
                .or_else(|| plan.new_directories.iter().find(|node| node.path == current))
                .ok_or("An unowned Nightly parent was preserved.")?;
            let name = Path::new(component.as_os_str());
            match directory.info(name)? {
                None if original.is_none() => return Ok(None),
                Some((id, true)) if id == expected.id => {
                    directory = directory.child(name)?;
                    if directory.id()? != expected.id {
                        return Err("A Nightly parent changed identity; it was not adopted.".into());
                    }
                }
                _ => return Err("An original or arriving Nightly parent prevented rollback; recovery was preserved.".into()),
            }
        }
        Ok(Some((directory, path.file_name().map(PathBuf::from).ok_or("Missing Nightly filename.")?)))
    }
    fn stage_location(&self, path: &Path) -> Result<Option<(Dir, PathBuf)>, String> {
        relative(path)?;
        let mut directory = self.stage.clone();
        let mut current = PathBuf::new();
        for component in path.parent().ok_or("Missing staging parent.")?.components() {
            let name = Path::new(component.as_os_str());
            current.push(name);
            let expected = self.entries.get(&current).ok_or("An unlogged staging parent was preserved.")?;
            match directory.info(name)? {
                None => return Ok(None),
                Some((id, true)) if id == expected.id => directory = directory.child(name)?,
                _ => return Err("A staging parent changed identity; its replacement was preserved.".into()),
            }
        }
        Ok(Some((directory, path.file_name().map(PathBuf::from).ok_or("Missing staging filename.")?)))
    }
    fn prune_root(&self, committed: bool) -> Result<(), String> {
        let plan = self.plan.as_ref().ok_or("Missing Nightly plan.")?;
        if (committed && plan.next.is_none()) || (!committed && plan.created_root) {
            self.lease.apps.prune(Path::new(self.lease.adapter.app()), &plan.root)?;
        }
        Ok(())
    }
    fn move_file(&self, from: &Dir, source: &Path, to: &Dir, target: &Path, id: &Identity) -> Result<(), String> {
        from.move_to(source, to, target, id)?;
        from.sync()?;
        to.sync()
    }

    pub(crate) fn apply(&self, held: &mut Option<Dir>, checkpoint: &mut dyn FnMut(Checkpoint) -> Result<(), String>) -> Result<(), String> {
        let plan = self.plan.as_ref().ok_or("Missing Nightly plan.")?;
        if plan.created_root {
            self.move_file(&self.stage, Path::new("root"), &self.lease.apps, Path::new(self.lease.adapter.app()), &plan.root)?;
        }
        let root = self.root(None)?.ok_or("Missing reserved Nightly root.")?;
        *held = Some(root.clone());
        checkpoint(Checkpoint::RootReserved)?;
        for node in &plan.old {
            self.check_root()?;
            let (parent, name) = self.parent(&root, &node.path)?;
            let (backup, backup_name) = self.stage_location(&Path::new("backup").join(&node.path))?.ok_or("Missing backup parent.")?;
            checkpoint(Checkpoint::BeforeRemoval)?;
            self.move_file(&parent, &name, &backup, &backup_name, &node.id)?;
        }
        checkpoint(Checkpoint::BackupsSaved)?;
        for path in &plan.directory_moves {
            self.check_root()?;
            let (source, name) = self.stage_location(&Path::new("directories").join(path))?.ok_or("Missing prepared directory parent.")?;
            let (target, target_name) = self.parent(&root, path)?;
            let id = &plan.new_directories.iter().find(|node| &node.path == path).ok_or("Missing prepared directory identity.")?.id;
            self.move_file(&source, &name, &target, &target_name, id)?;
        }
        for node in &plan.new {
            self.check_root()?;
            let (source, name) = self.stage_location(&Path::new("unpacked").join(&node.path))?.ok_or("Missing prepared file parent.")?;
            let (target, target_name) = self.parent(&root, &node.path)?;
            // Both parents are already held before the final interleaving point.
            checkpoint(Checkpoint::BeforeProgramCreate)?;
            self.move_file(&source, &name, &target, &target_name, &node.id)?;
        }
        self.check_root()?;
        Ok(())
    }

    fn rollback(&self, held: Option<&Dir>) -> Result<(), String> {
        let plan = self.plan.as_ref().ok_or("Missing Nightly plan.")?;
        let root = self.root(held)?;
        if let Some(root) = root.as_ref() {
            for node in plan.new.iter().rev() {
                let Some((parent, name)) = self.rollback_parent(root, &node.path)? else { continue };
                match parent.info(&name)? {
                    Some((id, false)) if id == node.id => {
                        let (target, name_to) =
                            self.stage_location(&Path::new("unpacked").join(&node.path))?.ok_or("Missing payload recovery parent.")?;
                        self.move_file(&parent, &name, &target, &name_to, &node.id)?;
                    }
                    None => {}
                    Some((id, false)) if plan.old.iter().any(|old| old.path == node.path && old.id == id) => {}
                    _ => return Err("An arriving program object was preserved; rollback files remain in the preparation slot.".into()),
                }
            }
            for node in &plan.old {
                let (parent, name) = self.parent(root, &node.path)?;
                match parent.info(&name)? {
                    Some((id, false)) if id == node.id => {}
                    None => {
                        let (backup, name_from) =
                            self.stage_location(&Path::new("backup").join(&node.path))?.ok_or("Missing Nightly backup parent.")?;
                        self.move_file(&backup, &name_from, &parent, &name, &node.id)?;
                    }
                    _ => return Err("An arriving file prevented restoration; the original backup was preserved.".into()),
                }
            }
            self.prune(root, &plan.new_directories, None)?;
        }
        self.lease.check_namespace()?;
        // A live held root can restore into its original inode after its name moved, but cannot
        // silently claim that the persisted pathname now denotes that inode again.
        if self.lease.apps.info(Path::new(self.lease.adapter.app()))?.is_some() {
            self.root(None)?;
        }
        Ok(())
    }
    pub(crate) fn verify_installed(&self) -> Result<(), String> {
        self.check_root()?;
        let plan = self.plan.as_ref().ok_or("Missing Nightly plan.")?;
        let root = self.root(None)?.ok_or("Missing installed Nightly root.")?;
        for node in &plan.new {
            let (directory, name) = self.parent(&root, &node.path)?;
            if directory.info(&name)? != Some((node.id.clone(), false)) {
                return Err("An installed Nightly file changed before commit.".into());
            }
        }
        Ok(())
    }
    fn committed(&self, held: Option<&Dir>) -> Result<(), String> {
        let plan = self.plan.as_ref().ok_or("Missing Nightly plan.")?;
        let root = self.root(held)?.ok_or("The committed Nightly program root is missing.")?;
        for node in &plan.new {
            let (directory, name) = self.parent(&root, &node.path)?;
            if directory.info(&name)? != Some((node.id.clone(), false)) {
                return Err("A committed Nightly file changed identity; cleanup was preserved.".into());
            }
        }
        // A visible manifest rename may have returned a directory-sync error. Both live
        // finalization and fresh recovery must establish that barrier before losing backups.
        self.lease.check_namespace()?;
        self.lease.data.sync()?;
        for node in &plan.old {
            if let Some((directory, name)) = self.stage_location(&Path::new("backup").join(&node.path))? {
                directory.remove(&name, &node.id, false)?;
            }
        }
        self.prune(&root, &plan.directories, plan.next.as_ref())?;

        self.lease.check_namespace()
    }
    fn prune(&self, root: &Dir, nodes: &[Node], retained: Option<&Record>) -> Result<(), String> {
        let mut nodes: Vec<_> = nodes.iter().filter(|node| !node.path.as_os_str().is_empty()).collect();
        nodes.sort_by_key(|node| std::cmp::Reverse(node.path.components().count()));
        for node in nodes {
            // A retained record whose root cannot be proven keeps every directory (nothing pruned).
            if retained.is_some_and(|record| {
                record_root(record, &self.lease.paths.apps().join(self.lease.adapter.app()), self.lease.adapter.executable())
                    .map_or(true, |root| record.files.iter().any(|file| file.starts_with(root.join(&node.path))))
            }) {
                continue;
            }
            if let Some((parent, name)) = location(root, &node.path)? {
                parent.prune(&name, &node.id)?;
            }
        }
        Ok(())
    }
    pub(crate) fn finish(&mut self, outcome: Result<(), String>, held: Option<Dir>, commit_succeeded: bool) -> Result<(), String> {
        self.file.take(); // Windows journal cleanup must not fight our own no-delete handle.
        let committed = if commit_succeeded {
            true
        } else {
            match &outcome {
                Ok(()) => true,
                Err(_) => {
                    let manifest = self.lease.manifest()?;
                    let plan = self.plan.as_ref().ok_or("Missing Nightly plan.")?;
                    if manifest.apps.get(self.lease.adapter.app()) == plan.next.as_ref() && plan.next != plan.previous {
                        true
                    } else if manifest.apps.get(self.lease.adapter.app()) == plan.previous.as_ref() {
                        false
                    } else {
                        return Err("The Nightly manifest changed unexpectedly; recovery was preserved.".into());
                    }
                }
            }
        };
        let recovery = if committed { self.committed(held.as_ref()) } else { self.rollback(held.as_ref()) };
        drop(held);
        if let Err(error) = recovery.and_then(|()| {
            self.finished = Some(committed);
            self.cleanup()
        }) {
            let prefix = if committed {
                "The new Nightly state is committed; cleanup is pending"
            } else {
                "The old Nightly record is retained; recovery is pending"
            };
            return Err(format!("{prefix}: {error}. Recovery files: {}", self.path().display()));
        }
        if committed { outcome.map_err(|error| format!("The new Nightly state is committed: {error}")) } else { outcome }
    }
    pub(crate) fn abort(&mut self, error: String) -> String {
        self.file.take();
        match self.cleanup() {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; preparation was preserved at {}: {cleanup}", self.path().display()),
        }
    }
    fn verify_clean(&self) -> Result<(), String> {
        let plan = self.plan.as_ref().ok_or("Missing completed Nightly plan.")?;
        let nodes = if self.finished == Some(true) { &plan.new } else { &plan.old };
        match self.lease.apps.info(Path::new(self.lease.adapter.app()))? {
            None if nodes.is_empty() => Ok(()),
            Some((id, true)) if id == plan.root => {
                let root = self.lease.apps.child(Path::new(self.lease.adapter.app()))?;
                for node in nodes {
                    let (directory, name) = self.parent(&root, &node.path)?;
                    if directory.info(&name)? != Some((node.id.clone(), false)) {
                        return Err("A finalized Nightly program changed identity; the journal was preserved.".into());
                    }
                }
                Ok(())
            }
            _ => Err("The finalized Nightly root changed identity; its replacement was preserved.".into()),
        }
    }
    fn cleanup(&mut self) -> Result<(), String> {
        self.lease.append_ready()?;
        if self.stage.id()? != self.header.stage {
            return Err("The Nightly preparation identity changed.".into());
        }
        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_by_key(|node| (std::cmp::Reverse(node.path.components().count()), node.directory));
        for node in entries {
            if let Some((directory, name)) = self.stage_location(&node.path)? {
                directory.remove(&name, &node.id, node.directory)?;
            }
        }
        // Re-read append-only metadata registrations made by the manifest callback.
        if let Some(bytes) = read(&self.lease.data, &self.lease.journal_name())? {
            for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
                if let Entry::Metadata(node) = serde_json::from_slice(line).map_err(|e| e.to_string())? {
                    if node.directory || node.path != format!(".nightly-{}-manifest-v1.pending", self.lease.adapter.app()) {
                        return Err("Invalid Nightly metadata cleanup path.".into());
                    }
                    self.lease.data.remove(&node.path, &node.id, false)?;
                }
            }
        }
        // A create-to-registration crash leaves an unknown fixed scratch. Do not retire
        // its recovery evidence or delete it by basename, even after restoring old files.
        self.lease.metadata_clear()?;
        if !self.clean {
            use std::io::{Seek, SeekFrom};
            let mut journal = self.lease.data.update(&self.lease.journal_name(), false)?;
            journal.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
            self.lease.append_entry(&mut journal, &Entry::Clean { committed: self.finished })?;
            self.clean = true;
            #[cfg(test)]
            review_tests::after_clean();
        }
        if let Some(committed) = self.finished {
            self.prune_root(committed)?;
        }
        // Release our own no-delete directory handle before empty-only removal on Windows.
        drop(std::mem::replace(&mut self.stage, self.lease.apps.clone()));
        // Empty-only: an unlogged/arriving object prevents cleanup instead of being swept up.
        self.lease.apps.remove(&self.lease.stage_name(), &self.header.stage, true)?;
        self.lease.apps.sync()?;
        #[cfg(test)]
        crate::backend::review_after_stage_removed();
        self.lease.data.remove(&self.lease.journal_name(), &self.journal_id, false)?;
        self.lease.data.sync()
    }
}

fn location(root: &Dir, path: &Path) -> Result<Option<(Dir, PathBuf)>, String> {
    relative(path)?;
    let mut directory = root.clone();
    for component in path.parent().ok_or("Missing Nightly parent.")?.components() {
        let name = Path::new(component.as_os_str());
        match directory.info(name)? {
            None => return Ok(None),
            Some((_, true)) => directory = directory.child(name)?,
            _ => return Err("A Nightly parent is no longer a directory.".into()),
        }
    }
    Ok(Some((directory, path.file_name().map(PathBuf::from).ok_or("Missing Nightly filename.")?)))
}

pub(crate) fn remove(
    lease: &Lease,
    record: &Record,
    commit: &mut dyn FnMut() -> Result<(), String>,
    checkpoint: &mut dyn FnMut(Checkpoint) -> Result<(), String>,
) -> Result<(), String> {
    let mut preparation = lease.prepare()?;
    if let Err(error) = preparation.seal(Some(record), None) {
        return Err(preparation.abort(error));
    }
    let mut held = None;
    let mut committed = false;
    let transaction = (|| {
        preparation.apply(&mut held, checkpoint)?;
        checkpoint(Checkpoint::BeforeCommit)?;
        preparation.verify_installed()?;
        commit()?;
        committed = true;
        checkpoint(Checkpoint::AfterCommit)
    })();
    preparation.finish(transaction, held, committed)
}

/// The program folder exactly as `record` spells it, after proving it is `owned`. Windows
/// installers record the canonical `\\?\C:\…` form while `Paths` uses the ordinary one, so
/// relative paths must be taken from the record's own root, never by stripping `owned`.
pub(crate) fn record_root<'r>(record: &'r Record, owned: &Path, executable: &Path) -> Result<&'r Path, String> {
    record_root_with(record, owned, executable, same_program_folder)
}

fn record_root_with<'r>(
    record: &'r Record,
    owned: &Path,
    executable: &Path,
    same: impl Fn(&Path, &Path) -> Result<bool, String>,
) -> Result<&'r Path, String> {
    // The executable can be nested (a macOS bundle), so the root is what remains after removing it.
    let root = record.path.ancestors().nth(executable.components().count()).ok_or("The Nightly record has no program folder.")?;
    if root.join(executable) != record.path || !same(root, owned)? {
        return Err("The Nightly record does not own this program folder.".into());
    }
    Ok(root)
}

#[cfg(windows)]
fn same_program_folder(root: &Path, owned: &Path) -> Result<bool, String> {
    Ok(crate::portable::same_windows_path(root, owned) || crate::portable::same_windows_path(root, &crate::portable::resolve_missing(owned)?))
}

#[cfg(not(windows))]
fn same_program_folder(root: &Path, owned: &Path) -> Result<bool, String> {
    Ok(root == owned)
}

pub(crate) fn stored_manifest(paths: &Paths) -> Result<Manifest, String> {
    match std::fs::symlink_metadata(paths.manifest()) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(error) => Err(error.to_string()),
        Ok(_) => {
            let directory = Dir::open(&paths.data)?;
            let path = paths.manifest();
            let name = path.file_name().map(Path::new).ok_or("Missing private manifest name.")?;
            let bytes = read(&directory, name)?.ok_or("The Nightly manifest disappeared.")?;
            serde_json::from_slice(&bytes)
                .map_err(|error| format!("The private Nightly manifest is unreadable; it was not treated as empty: {error}"))
        }
    }
}
pub(crate) fn refresh(paths: &Paths, adapter: Adapter) -> Result<Manifest, String> {
    adapter.validate_profile(paths)?;
    let journal = paths.data.join(format!(".nightly-{}-journal-v1.jsonl", adapter.app()));
    let stage = paths.apps().join(format!(".nightly-{}-preparation-v1", adapter.app()));
    if std::fs::symlink_metadata(journal).is_ok() || std::fs::symlink_metadata(stage).is_ok() {
        return Lease::acquire(paths, adapter)?.manifest();
    }
    stored_manifest(paths)
}

#[cfg(test)]
pub(crate) fn review_directory_sync(path: &Path) -> Result<(), String> {
    review_tests::directory_sync(path)
}

#[cfg(test)]
#[path = "nightly_transaction_review_tests.rs"]
mod review_tests;
