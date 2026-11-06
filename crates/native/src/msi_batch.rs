//! Coordinates Windows MSI downloads so one elevated helper handles one batch.
//!
//! A ticket is joined before its download starts. Ready files wait until every ticket in the
//! batch has either handed in its verified file or left because its download failed or was
//! cancelled. The first ready ticket then runs one installer call and distributes each result
//! back to its own job.

use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use artcraft_launcher_core::protocol::{CANCELLED, JobPhase};

type Outcome = Result<(), String>;

/// A downloaded MSI and the published SHA-256 it must still match when elevated.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Item {
    pub msi: PathBuf,
    pub sha256: String,
    pub destination: Option<PathBuf>,
    pub release: crate::elevated::ReleaseIdentity,
}

/// A phase or result reported for one item in a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    Phase(JobPhase),
    Done(Outcome),
}

/// Installs a batch and reports each item by its position.
pub type Call<'a> = &'a dyn Fn(&[Item], &mut dyn FnMut(usize, Report));

struct State {
    /// Jobs reserved by an explicit Update all command that have not yet chosen a package.
    slots: usize,
    downloading: usize,
    next_id: u64,
    ready: Vec<(u64, Item)>,
    busy: bool,
    running: Vec<(u64, Option<JobPhase>)>,
    finished: Vec<(u64, Outcome)>,
}

/// The one Windows MSI batch for this launcher process.
#[cfg(windows)]
pub static BATCH: Batch = Batch::with_installer(&crate::queue::WINDOWS_INSTALLER);

pub struct Batch {
    state: Mutex<State>,
    changed: Condvar,
    installer: &'static crate::queue::Queue,
}

#[cfg(test)]
impl Default for Batch {
    fn default() -> Self {
        Self::new()
    }
}

impl Batch {
    /// Creates a batch coordinator that serializes installs through `installer` with other MSI work.
    pub const fn with_installer(installer: &'static crate::queue::Queue) -> Self {
        let state = State { slots: 0, downloading: 0, next_id: 0, ready: Vec::new(), busy: false, running: Vec::new(), finished: Vec::new() };
        Self { state: Mutex::new(state), changed: Condvar::new(), installer }
    }

    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_installer(Box::leak(Box::new(crate::queue::Queue::new())))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.changed.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(PoisonError::into_inner).0
    }

    /// Joins before a download starts. Dropping the ticket tells ready files they need not wait
    /// for this download anymore.
    #[cfg(test)]
    pub fn join(&self) -> Ticket<'_> {
        self.lock().downloading += 1;
        Ticket { batch: self, handed_in: false }
    }

    /// Reserves one slot for every app in an explicit Update all command. The slots close the
    /// admission window even when one download is already cached and finishes before the other
    /// worker threads have started.
    pub fn begin(&self, count: usize) -> bool {
        let mut state = self.lock();
        if count == 0 {
            return false;
        }
        state.slots += count;
        true
    }

    /// Reserves one previously admitted Update all item; dropping the slot releases that admission.
    pub fn slot(&self) -> Slot<'_> {
        Slot { batch: self, used: false }
    }
}

/// One reserved app in an explicit batch. Dropping it records that this app will not use MSI,
/// failed before choosing a package, or was already running.
pub struct Slot<'a> {
    batch: &'a Batch,
    used: bool,
}

impl<'a> Slot<'a> {
    /// Converts an admitted item into a download ticket that blocks installation until it finishes.
    pub fn join(mut self) -> Ticket<'a> {
        self.used = true;
        let batch = self.batch;
        let mut state = batch.lock();
        state.slots = state.slots.saturating_sub(1);
        state.downloading += 1;
        batch.changed.notify_all();
        Ticket { batch, handed_in: false }
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if !self.used {
            let mut state = self.batch.lock();
            state.slots = state.slots.saturating_sub(1);
            self.batch.changed.notify_all();
        }
    }
}

pub struct Ticket<'a> {
    batch: &'a Batch,
    handed_in: bool,
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if !self.handed_in {
            let mut state = self.batch.lock();
            state.downloading = state.downloading.saturating_sub(1);
            self.batch.changed.notify_all();
        }
    }
}

struct CallEnd<'a> {
    batch: &'a Batch,
    leader: u64,
}

impl Drop for CallEnd<'_> {
    fn drop(&mut self) {
        let mut state = self.batch.lock();
        state.busy = false;
        for (id, _) in std::mem::take(&mut state.running) {
            if id != self.leader {
                state.finished.push((id, Err("the Windows Installer was interrupted".into())));
            }
        }
        self.batch.changed.notify_all();
    }
}

impl Ticket<'_> {
    /// Hands in a verified file and waits for its result. Cancellation is honored while waiting;
    /// once the elevated call has started, every item sees that call through.
    pub fn install(mut self, item: Item, cancelled: &dyn Fn() -> bool, phase: &mut dyn FnMut(JobPhase), call: Call<'_>) -> Outcome {
        let batch = self.batch;
        let mut state = batch.lock();
        self.handed_in = true;
        state.downloading = state.downloading.saturating_sub(1);
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.ready.push((id, item));
        batch.changed.notify_all();
        let mut told_waiting = false;
        let _installer = loop {
            if let Some(at) = state.finished.iter().position(|(done, _)| *done == id) {
                return state.finished.swap_remove(at).1;
            }
            let running = state.running.iter().position(|(running, _)| *running == id);
            if let Some(at) = running {
                if let Some(p) = state.running.get_mut(at).and_then(|(_, pending)| pending.take()) {
                    drop(state);
                    phase(p);
                    state = batch.lock();
                } else {
                    state = batch.wait(state);
                }
                continue;
            }
            if cancelled() {
                state.ready.retain(|(waiting, _)| *waiting != id);
                batch.changed.notify_all();
                return Err(CANCELLED.into());
            }
            if !state.busy
                && state.slots == 0
                && state.downloading == 0
                && let Some(permit) = batch.installer.try_claim()
            {
                break permit;
            }
            if told_waiting {
                state = batch.wait(state);
            } else {
                told_waiting = true;
                drop(state);
                phase(JobPhase::Waiting);
                state = batch.lock();
            }
        };

        let taken = std::mem::take(&mut state.ready);
        state.busy = true;
        state.running = taken.iter().map(|(job, _)| (*job, None)).collect();
        drop(state);
        let (ids, items): (Vec<u64>, Vec<Item>) = taken.into_iter().unzip();
        let _end = CallEnd { batch, leader: id };
        let mut mine = None;
        call(&items, &mut |index, report| {
            let Some(&job) = ids.get(index) else { return };
            match report {
                Report::Phase(p) if job == id => phase(p),
                Report::Done(outcome) if job == id => mine = Some(outcome),
                Report::Phase(p) => {
                    let mut state = batch.lock();
                    if let Some((_, pending)) = state.running.iter_mut().find(|(running, _)| *running == job) {
                        *pending = Some(p);
                    }
                    batch.changed.notify_all();
                }
                Report::Done(outcome) => {
                    let mut state = batch.lock();
                    if state.running.iter().any(|(running, _)| *running == job) {
                        state.running.retain(|(running, _)| *running != job);
                        state.finished.push((job, outcome));
                    }
                    batch.changed.notify_all();
                }
            }
        });
        mine.unwrap_or_else(|| Err("the Windows Installer stopped before installing it".into()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn item(name: &str) -> Item {
        item_for("photocraft", name)
    }

    fn item_for(app: &str, name: &str) -> Item {
        Item {
            msi: PathBuf::from(name),
            sha256: "0".repeat(64),
            destination: None,
            release: crate::elevated::ReleaseIdentity { app: app.into(), tag: "v0.5.0".into(), asset: name.into() },
        }
    }

    #[test]
    /// Holds installation until all joined downloads finish, then distributes results by item index.
    fn files_wait_for_every_download_and_keep_individual_results() {
        let batch = Batch::new();
        let calls = std::sync::Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let tickets: Vec<Ticket<'_>> = (0..3).map(|_| batch.join()).collect();
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = tickets
                .into_iter()
                .enumerate()
                .map(|(n, ticket)| {
                    let started = started_tx.clone();
                    let calls = calls.clone();
                    scope.spawn(move || {
                        std::thread::sleep(Duration::from_millis(20 * n as u64));
                        ticket.install(item(if n == 1 { "bad.msi" } else { "ok.msi" }), &|| false, &mut |_| {}, &|items, report| {
                            calls.lock().unwrap().push(items.iter().map(|i| i.msi.clone()).collect::<Vec<_>>());
                            started.send(()).unwrap();
                            report(0, Report::Phase(JobPhase::Installing));
                            for (i, item) in items.iter().enumerate() {
                                report(i, Report::Done(if item.msi == *"bad.msi" { Err("failed".into()) } else { Ok(()) }));
                            }
                        })
                    })
                })
                .collect();
            let results = handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>();
            assert_eq!(started_rx.recv().unwrap(), ());
            results
        });
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(results, [Ok(()), Err("failed".into()), Ok(())]);
    }

    #[test]
    /// Proves two reserved Update all items invoke one installer callback and retain separate outcomes.
    fn two_reserved_update_all_items_use_one_installer_call_and_keep_individual_results() {
        let batch = Batch::new();
        assert!(batch.begin(2));
        let slots = [batch.slot(), batch.slot()];
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = slots
                .into_iter()
                .zip([("photocraft", "photocraft.msi"), ("vectorcraft", "vectorcraft.msi")])
                .map(|(slot, (app, name))| {
                    let calls = calls.clone();
                    scope.spawn(move || {
                        let result = slot.join().install(item_for(app, name), &|| false, &mut |_| {}, &|items, report| {
                            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            assert_eq!(items.len(), 2, "one elevated batch should contain both eligible MSIs");
                            for (index, item) in items.iter().enumerate() {
                                let outcome = if item.release.app == "vectorcraft" { Err("VectorCraft failed".into()) } else { Ok(()) };
                                report(index, Report::Done(outcome));
                            }
                        });
                        (app.to_string(), result)
                    })
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap()).collect::<Vec<_>>()
        });

        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "one eligible InstallAll batch makes one installer call");
        let mut results = results;
        results.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            results,
            [("photocraft".into(), Ok(())), ("vectorcraft".into(), Err("VectorCraft failed".into()))],
            "each app receives its own installer result"
        );
    }

    #[test]
    /// Confirms a failed download releases its ticket so a ready package can proceed.
    fn a_failed_download_does_not_hold_the_batch() {
        let batch = Batch::new();
        let failed = batch.join();
        let ready = batch.join();
        drop(failed);
        assert_eq!(ready.install(item("a.msi"), &|| false, &mut |_| {}, &|_, report| report(0, Report::Done(Ok(())))), Ok(()));
    }

    #[test]
    /// Confirms a waiting app can cancel without installation while remaining batch items continue.
    fn waiting_jobs_can_cancel_and_are_not_installed() {
        let batch = Batch::new();
        let first = batch.join();
        let second = batch.join();
        let (waiting, heard) = mpsc::channel();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                second.install(
                    item("a.msi"),
                    &|| cancelled.load(std::sync::atomic::Ordering::SeqCst),
                    &mut |p| {
                        if p == JobPhase::Waiting {
                            waiting.send(()).unwrap();
                        }
                    },
                    &|_, _| panic!("cancelled job was installed"),
                )
            });
            heard.recv().unwrap();
            cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(worker.join().unwrap(), Err(CANCELLED.into()));
        });
        assert_eq!(first.install(item("b.msi"), &|| false, &mut |_| {}, &|_, report| report(0, Report::Done(Ok(())))), Ok(()));
    }

    #[test]
    /// Prevents a cached first package from starting before every synchronous Update all slot joins.
    fn explicit_update_all_slots_close_the_race_before_a_cached_download_runs() {
        let batch = Batch::new();
        assert!(batch.begin(2));
        let first = batch.slot();
        let second = batch.slot();
        let (started, heard) = mpsc::channel();
        std::thread::scope(|scope| {
            let first_worker = scope.spawn(|| {
                first.join().install(item("first.msi"), &|| false, &mut |_| {}, &|items, report| {
                    started.send(items.len()).unwrap();
                    for i in 0..items.len() {
                        report(i, Report::Done(Ok(())));
                    }
                })
            });
            std::thread::sleep(Duration::from_millis(20));
            assert!(heard.try_recv().is_err(), "the first cached download started before Update all's second app joined");
            let second_worker = scope.spawn(|| {
                second.join().install(item("second.msi"), &|| false, &mut |_| {}, &|items, report| {
                    started.send(items.len()).unwrap();
                    for i in 0..items.len() {
                        report(i, Report::Done(Ok(())));
                    }
                })
            });
            assert_eq!(heard.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
            assert_eq!(first_worker.join().unwrap(), Ok(()));
            assert_eq!(second_worker.join().unwrap(), Ok(()));
        });
    }

    #[test]
    /// Releases a reserved slot when an app fails before MSI selection or selects another format.
    fn a_failed_or_non_msi_app_releases_its_reserved_slot() {
        let batch = Batch::new();
        assert!(batch.begin(2));
        let waiting_package = batch.slot();
        drop(batch.slot());
        let (started, heard) = mpsc::channel();
        let result = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                waiting_package.join().install(item("ready.msi"), &|| false, &mut |_| {}, &|items, report| {
                    started.send(items.len()).unwrap();
                    report(0, Report::Done(Ok(())));
                })
            });
            assert_eq!(heard.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
            worker.join().unwrap()
        });
        assert_eq!(result, Ok(()));
    }

    #[test]
    /// Serializes the batch installer callback and MSI uninstall through the same permit.
    fn an_msi_batch_and_an_uninstall_share_the_same_installer_permit() {
        let batch = Batch::new();
        let ticket = batch.join();
        let (started, heard) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (waiting, waited) = mpsc::channel();
        std::thread::scope(|scope| {
            let install = scope.spawn(move || {
                ticket.install(item("a.msi"), &|| false, &mut |_| {}, &|_, report| {
                    started.send(()).unwrap();
                    released.recv().unwrap();
                    report(0, Report::Done(Ok(())));
                })
            });
            heard.recv_timeout(Duration::from_secs(2)).unwrap();
            let uninstall = scope.spawn(|| {
                batch.installer.alone(
                    &|| false,
                    &mut |turn| {
                        if turn {
                            waiting.send(()).unwrap();
                        }
                    },
                    || 42,
                )
            });
            waited.recv_timeout(Duration::from_secs(2)).unwrap();
            release.send(()).unwrap();
            assert_eq!(install.join().unwrap(), Ok(()));
            assert_eq!(uninstall.join().unwrap(), Some(42));
        });
    }

    #[test]
    /// Keeps a batch waiting on an uninstall cancellable, then allows a later MSI ticket to run.
    fn a_batch_waiting_for_uninstall_stays_cancellable() {
        let batch = Batch::new();
        let permit = batch.installer.try_claim().unwrap();
        let ticket = batch.join();
        assert_eq!(ticket.install(item("a.msi"), &|| true, &mut |_| {}, &|_, _| panic!("cancelled batch ran")), Err(CANCELLED.into()));
        drop(permit);
        let ticket = batch.join();
        assert_eq!(ticket.install(item("b.msi"), &|| false, &mut |_| {}, &|_, report| report(0, Report::Done(Ok(())))), Ok(()));
    }
}
