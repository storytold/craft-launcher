//! One permission prompt for many Windows Installer packages.
//!
//! The Craft Apps' MSIs install for every user of the machine, so each needs administrator
//! rights, and every elevation is a UAC prompt. Instead of one prompt per app, an MSI job joins
//! the [`Batch`] before its download starts and hands its verified file in once it's ready.
//! Files wait while any job of the batch is still downloading; then everything waiting goes to
//! the installer in one call, so **Update all** asks once however long each download takes. A
//! job that fails, pauses or is cancelled before handing its file in simply leaves.
//!
//! The call reports on each file as it goes ([`Report`]), and each job hears its own phase and
//! outcome. Nothing here is Windows-specific, so it's tested on every system; only Windows uses
//! it (`install::windows`).

use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use artcraft_launcher_core::protocol::{CANCELLED, JobPhase};

type Outcome = Result<(), String>;

/// A verified installer file, and the SHA-256 the installing side checks it against again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub msi: PathBuf,
    pub sha256: String,
}

/// What a batch call says about one of its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    Phase(JobPhase),
    Done(Outcome),
}

/// The function that installs a batch: it gets the files, and tells `report` about each one by
/// its position. A file it never reports on has failed.
pub type Call<'a> = &'a dyn Fn(&[Item], &mut dyn FnMut(usize, Report));

struct State {
    /// Jobs that joined and haven't handed their file in (or left) yet.
    downloading: usize,
    next_id: u64,
    ready: Vec<(u64, Item)>,
    busy: bool,
    /// The jobs of the call in progress, each with the phase it hasn't heard yet.
    running: Vec<(u64, Option<JobPhase>)>,
    finished: Vec<(u64, Outcome)>,
}

pub struct Batch {
    state: Mutex<State>,
    changed: Condvar,
}

impl Default for Batch {
    fn default() -> Self {
        Self::new()
    }
}

impl Batch {
    pub const fn new() -> Self {
        let state = State { downloading: 0, next_id: 0, ready: Vec::new(), busy: false, running: Vec::new(), finished: Vec::new() };
        Self { state: Mutex::new(state), changed: Condvar::new() }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Until something changes, or long enough to ask again whether the job was cancelled.
    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.changed.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(PoisonError::into_inner).0
    }

    /// Joins the batch before a download starts: until the ticket is used or dropped, files
    /// that are ready wait for this one.
    pub fn join(&self) -> Ticket<'_> {
        self.lock().downloading += 1;
        Ticket { batch: self, handed_in: false }
    }
}

/// A job's place in the [`Batch`] while it downloads.
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

/// Ends a call (even by unwinding): frees the batch, and fails the jobs the call never reported
/// on, so none waits forever.
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
    /// Hands `item` in and returns once it's installed (or not). `phase` hears
    /// [`JobPhase::Waiting`] while other jobs are still downloading or installing, then whatever
    /// the call reports for this file. While it waits, the job leaves with [`CANCELLED`] once
    /// `cancelled` (asked a few times a second, so it must not block) says so; once its call has
    /// started it sees it through.
    pub fn install(mut self, item: Item, cancelled: &dyn Fn() -> bool, phase: &mut dyn FnMut(JobPhase), call: Call<'_>) -> Outcome {
        let batch = self.batch;
        let mut state = batch.lock();
        self.handed_in = true;
        state.downloading = state.downloading.saturating_sub(1);
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.ready.push((id, item));
        batch.changed.notify_all();
        let mut told = false;
        loop {
            if let Some(at) = state.finished.iter().position(|(done, _)| *done == id) {
                return state.finished.swap_remove(at).1;
            }
            // Another job took this one into its call.
            if let Some((_, unheard)) = state.running.iter_mut().find(|(running, _)| *running == id) {
                if let Some(p) = unheard.take() {
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
            if !state.busy && state.downloading == 0 {
                break;
            }
            if told {
                state = batch.wait(state);
            } else {
                told = true;
                drop(state);
                phase(JobPhase::Waiting);
                state = batch.lock();
            }
        }
        // This job starts the call, for every file that is ready.
        let taken = std::mem::take(&mut state.ready);
        state.busy = true;
        state.running = taken.iter().map(|(job, _)| (*job, None)).collect();
        drop(state);
        let (ids, items): (Vec<u64>, Vec<Item>) = taken.into_iter().unzip();
        let end = CallEnd { batch, leader: id };
        let mut mine = None;
        call(&items, &mut |index, report| {
            let Some(&job) = ids.get(index) else { return };
            match report {
                Report::Phase(p) if job == id => phase(p),
                Report::Done(outcome) if job == id => mine = Some(outcome),
                Report::Phase(p) => {
                    let mut state = batch.lock();
                    if let Some((_, unheard)) = state.running.iter_mut().find(|(running, _)| *running == job) {
                        *unheard = Some(p);
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
        drop(end);
        mine.unwrap_or_else(|| Err("the Windows Installer stopped before installing it".into()))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use super::*;

    fn item(name: &str) -> Item {
        Item { msi: PathBuf::from(name), sha256: "0".repeat(64) }
    }

    /// Every call made, as the file names it was given.
    #[derive(Default)]
    struct Calls(Mutex<Vec<Vec<String>>>);

    impl Calls {
        fn record(&self, items: &[Item]) {
            self.0.lock().unwrap().push(items.iter().map(|i| i.msi.display().to_string()).collect());
        }

        fn taken(&self) -> Vec<Vec<String>> {
            let mut calls = std::mem::take(&mut *self.0.lock().unwrap());
            for c in &mut calls {
                c.sort();
            }
            calls
        }
    }

    /// Installs every file, failing the ones named `bad*.msi`.
    fn installer(calls: &Calls) -> impl Fn(&[Item], &mut dyn FnMut(usize, Report)) + '_ {
        move |items, report| {
            calls.record(items);
            for (i, it) in items.iter().enumerate() {
                report(i, Report::Phase(JobPhase::Installing));
                let failed = it.msi.display().to_string().starts_with("bad");
                report(i, Report::Done(if failed { Err("the Windows Installer stopped with error 1603".into()) } else { Ok(()) }));
            }
        }
    }

    #[test]
    fn files_wait_for_every_download_then_install_in_one_call() {
        let batch = Batch::new();
        let calls = Calls::default();
        let install = installer(&calls);
        let tickets: Vec<Ticket<'_>> = (0..3).map(|_| batch.join()).collect();
        let (waiting, waits) = mpsc::channel();
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = tickets
                .into_iter()
                .zip(["a.msi", "bad.msi", "c.msi"])
                .enumerate()
                .map(|(n, (ticket, name))| {
                    let (waiting, install) = (waiting.clone(), &install);
                    scope.spawn(move || {
                        // The downloads finish one after another.
                        std::thread::sleep(Duration::from_millis(40 * n as u64));
                        let mut phase = |p: JobPhase| {
                            if p == JobPhase::Waiting {
                                waiting.send(name).unwrap();
                            }
                        };
                        ticket.install(item(name), &|| false, &mut phase, install)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
        });
        assert_eq!(calls.taken(), [["a.msi", "bad.msi", "c.msi"]], "one call for the whole batch");
        // Each job gets its own result: one failure doesn't fail the others.
        assert_eq!(results, [Ok(()), Err("the Windows Installer stopped with error 1603".into()), Ok(())]);
        // The first two waited for the last download; the last one found nothing to wait for.
        let mut waited: Vec<_> = waits.try_iter().collect();
        waited.sort_unstable();
        assert_eq!(waited, ["a.msi", "bad.msi"]);
    }

    #[test]
    fn a_job_that_leaves_while_downloading_holds_nobody_up() {
        let batch = Batch::new();
        let calls = Calls::default();
        let failed_download = batch.join();
        let ticket = batch.join();
        drop(failed_download);
        assert_eq!(ticket.install(item("a.msi"), &|| false, &mut |_| {}, &installer(&calls)), Ok(()));
        assert_eq!(calls.taken(), [["a.msi"]]);
    }

    #[test]
    fn a_waiting_job_can_be_cancelled_and_is_left_out_of_the_call() {
        let batch = Batch::new();
        let calls = Calls::default();
        let install = installer(&calls);
        let still_downloading = batch.join();
        let leaving = batch.join();
        let cancel = AtomicBool::new(false);
        let (waiting, waits) = mpsc::channel();
        std::thread::scope(|scope| {
            let (cancel, install) = (&cancel, &install);
            let left =
                scope.spawn(move || leaving.install(item("a.msi"), &|| cancel.load(Ordering::SeqCst), &mut |p| waiting.send(p).unwrap(), install));
            assert_eq!(waits.recv().unwrap(), JobPhase::Waiting);
            cancel.store(true, Ordering::SeqCst);
            assert_eq!(left.join().unwrap(), Err(CANCELLED.to_string()));
            assert!(calls.0.lock().unwrap().is_empty());
            assert_eq!(still_downloading.install(item("b.msi"), &|| false, &mut |_| {}, install), Ok(()));
        });
        assert_eq!(calls.taken(), [["b.msi"]]);
    }

    #[test]
    fn each_job_hears_its_own_phases_and_sees_its_call_through() {
        let batch = Batch::new();
        let (first, second) = (batch.join(), batch.join());
        let cancel = AtomicBool::new(false);
        let (in_call, is_in_call) = mpsc::channel();
        let (finish, finished) = mpsc::channel::<()>();
        let finished = Mutex::new(finished);
        // Asks for permission and, once the test says, installs: the files report a phase, then
        // (after a moment, as a real install takes) their results. A job that is told its result
        // first goes straight to it; a phase it hasn't heard by then no longer matters.
        let install = |items: &[Item], report: &mut dyn FnMut(usize, Report)| {
            for i in 0..items.len() {
                report(i, Report::Phase(JobPhase::Permission));
            }
            in_call.send(()).unwrap();
            finished.lock().unwrap().recv().unwrap();
            for i in 0..items.len() {
                report(i, Report::Phase(JobPhase::Installing));
            }
            std::thread::sleep(Duration::from_millis(300));
            for i in 0..items.len() {
                report(i, Report::Done(Ok(())));
            }
        };
        let (heard_a, heard_b) = (Mutex::new(Vec::new()), Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            let (install, cancel) = (&install, &cancel);
            let (heard_a, heard_b) = (&heard_a, &heard_b);
            let a = scope
                .spawn(move || first.install(item("a.msi"), &|| cancel.load(Ordering::SeqCst), &mut |p| heard_a.lock().unwrap().push(p), install));
            let b = scope
                .spawn(move || second.install(item("b.msi"), &|| cancel.load(Ordering::SeqCst), &mut |p| heard_b.lock().unwrap().push(p), install));
            is_in_call.recv().unwrap();
            // Cancelled mid-call: neither job leaves, the one that didn't start the call included.
            cancel.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(300));
            finish.send(()).unwrap();
            assert_eq!(a.join().unwrap(), Ok(()));
            assert_eq!(b.join().unwrap(), Ok(()));
        });
        for heard in [heard_a, heard_b] {
            let heard: Vec<_> = heard.into_inner().unwrap().into_iter().filter(|p| *p != JobPhase::Waiting).collect();
            assert_eq!(heard, [JobPhase::Permission, JobPhase::Installing]);
        }
    }

    #[test]
    fn a_file_the_call_never_reports_on_has_failed() {
        let batch = Batch::new();
        let (first, second) = (batch.join(), batch.join());
        let silent = |_: &[Item], _: &mut dyn FnMut(usize, Report)| {};
        let results = std::thread::scope(|scope| {
            let a = scope.spawn(|| first.install(item("a.msi"), &|| false, &mut |_| {}, &silent));
            let b = scope.spawn(|| second.install(item("b.msi"), &|| false, &mut |_| {}, &silent));
            [a.join().unwrap(), b.join().unwrap()]
        });
        let mut errors: Vec<String> = results.into_iter().map(Result::unwrap_err).collect();
        errors.sort();
        assert_eq!(errors, ["the Windows Installer stopped before installing it", "the Windows Installer was interrupted"]);
    }

    #[test]
    fn a_call_that_dies_frees_the_batch_and_fails_its_jobs() {
        let batch = Batch::new();
        let (first, second) = (batch.join(), batch.join());
        let dies = |items: &[Item], _: &mut dyn FnMut(usize, Report)| {
            assert_eq!(items.len(), 2);
            panic!("the installer died")
        };
        let results = std::thread::scope(|scope| {
            let a = scope.spawn(|| first.install(item("a.msi"), &|| false, &mut |_| {}, &dies));
            let b = scope.spawn(|| second.install(item("b.msi"), &|| false, &mut |_| {}, &dies));
            [a.join().ok(), b.join().ok()]
        });
        // The job that made the call unwound; the other was told.
        assert!(results.contains(&None), "{results:?}");
        assert!(results.contains(&Some(Err("the Windows Installer was interrupted".to_string()))), "{results:?}");
        let calls = Calls::default();
        assert_eq!(batch.join().install(item("c.msi"), &|| false, &mut |_| {}, &installer(&calls)), Ok(()));
    }

    #[test]
    fn a_file_ready_during_a_call_goes_in_the_next_one() {
        let batch = Batch::new();
        let calls = Calls::default();
        let (started, has_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        let install = |items: &[Item], report: &mut dyn FnMut(usize, Report)| {
            calls.record(items);
            if items.iter().any(|i| i.msi == Path::new("a.msi")) {
                started.send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
            }
            for i in 0..items.len() {
                report(i, Report::Done(Ok(())));
            }
        };
        std::thread::scope(|scope| {
            let (batch, install) = (&batch, &install);
            let first = scope.spawn(move || batch.join().install(item("a.msi"), &|| false, &mut |_| {}, install));
            has_started.recv().unwrap();
            let (waiting, waits) = mpsc::channel();
            let second = scope.spawn(move || batch.join().install(item("b.msi"), &|| false, &mut |p| waiting.send(p).unwrap(), install));
            assert_eq!(waits.recv().unwrap(), JobPhase::Waiting);
            release.send(()).unwrap();
            assert_eq!(first.join().unwrap(), Ok(()));
            assert_eq!(second.join().unwrap(), Ok(()));
        });
        assert_eq!(calls.taken(), [vec!["a.msi"], vec!["b.msi"]]);
    }
}
