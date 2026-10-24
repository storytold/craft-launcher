//! One system installer at a time.
//!
//! apt, dnf, rpm and the Windows Installer each hold a system-wide lock: an install started
//! while another runs fails. Jobs wait their turn here instead. Where the installer takes
//! several packages, the jobs waiting for the same command share one call, so the password is
//! asked for once.
//!
//! A job that will join such a call takes a [`Ticket`] before its download starts. A call waits
//! while any ticket for its command is still out, so jobs started together (**Update all**) end
//! up in one call however long each download takes. A job whose download fails, is paused or is
//! cancelled drops its ticket and leaves the batch.

use std::ffi::OsString;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use artcraft_launcher_core::protocol::CANCELLED;

type Outcome = Result<(), String>;

/// What a job that called [`Queue::batch`] hears about its turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Another install is running, or other jobs of the same batch are still downloading.
    Waiting,
    /// The job's call is under way, at the stage its leader last reported (0 when it starts).
    Running(u32),
}

/// The outcome a job gets when its call ended without saying (the installer died).
const INTERRUPTED: &str = "the system installer was interrupted";

/// A job waiting to join a call.
struct Waiting<T> {
    id: u64,
    command: Vec<OsString>,
    target: T,
}

/// What every job of the queue sees, under its lock.
struct State<T> {
    busy: bool,
    next_id: u64,
    waiting: Vec<Waiting<T>>,
    /// The jobs of the call in progress.
    running: Vec<u64>,
    /// How far the call in progress has got (see [`Step::Running`]).
    stage: u32,
    finished: Vec<(u64, Outcome)>,
    /// The command of every [`Ticket`] still out: jobs that will join a call once downloaded.
    expected: Vec<Vec<OsString>>,
}

impl<T> State<T> {
    /// Takes one [`Ticket`] for `command` off the list of expected jobs.
    fn forget(&mut self, command: &[OsString]) {
        if let Some(at) = self.expected.iter().position(|c| c == command) {
            self.expected.swap_remove(at);
        }
    }
}

/// Jobs waiting for the system's installer, with the target each brings to its call (a package
/// file for Linux package managers, an MSI job on Windows).
pub struct Queue<T = OsString> {
    state: Mutex<State<T>>,
    changed: Condvar,
}

/// A job that is still downloading and will then join a call to `command` (see the module docs).
/// Dropping it takes the job out of the batch; [`Queue::batch`] redeems it.
pub struct Ticket<'a, T> {
    queue: &'a Queue<T>,
    command: Option<Vec<OsString>>,
}

impl<T> Drop for Ticket<'_, T> {
    fn drop(&mut self) {
        if let Some(command) = self.command.take() {
            self.queue.lock().forget(&command);
            self.queue.changed.notify_all();
        }
    }
}

/// Frees the installer when a call ends (even by unwinding) and hands each of the other jobs of
/// the call its outcome.
struct Turn<'a, T> {
    queue: &'a Queue<T>,
    /// Every job of the call, in the order of the targets handed to the installer.
    jobs: Vec<u64>,
    /// The job that made the call: it returns its own outcome.
    leader: u64,
    /// One per job, once the call has returned.
    outcomes: Vec<Outcome>,
}

impl<T> Drop for Turn<'_, T> {
    fn drop(&mut self) {
        let mut state = self.queue.lock();
        state.busy = false;
        state.running.clear();
        state.stage = 0;
        for (at, id) in self.jobs.iter().enumerate() {
            if *id != self.leader {
                let outcome = self.outcomes.get(at).cloned().unwrap_or_else(|| Err(INTERRUPTED.into()));
                state.finished.push((*id, outcome));
            }
        }
        self.queue.changed.notify_all();
    }
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Queue<T> {
    /// An empty queue, with the installer free.
    pub const fn new() -> Self {
        let state = State { busy: false, next_id: 0, waiting: Vec::new(), running: Vec::new(), stage: 0, finished: Vec::new(), expected: Vec::new() };
        Self { state: Mutex::new(state), changed: Condvar::new() }
    }

    /// The queue's state, even after a job panicked while holding it.
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Until something changes, or long enough to ask again whether the job was cancelled.
    fn wait<'a>(&self, state: MutexGuard<'a, State<T>>) -> MutexGuard<'a, State<T>> {
        self.changed.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(PoisonError::into_inner).0
    }

    /// Says that a job will join a call to `command` once its download is ready. Calls to
    /// `command` wait until the ticket is redeemed by [`Queue::batch`] or dropped.
    pub fn expect(&self, command: Vec<OsString>) -> Ticket<'_, T> {
        self.lock().expected.push(command.clone());
        Ticket { queue: self, command: Some(command) }
    }

    /// Runs `work` once no other installer is running. `turn(true)` says it has to wait,
    /// `turn(false)` that the wait is over. `None` when `cancelled` (asked a few times a second,
    /// so it must not block) said so during the wait; `work` then never ran.
    pub fn alone<R>(&self, cancelled: &dyn Fn() -> bool, turn: &mut dyn FnMut(bool), work: impl FnOnce() -> R) -> Option<R> {
        let mut state = self.lock();
        let mut waited = false;
        loop {
            if waited && cancelled() {
                return None;
            }
            if !state.busy {
                break;
            }
            if waited {
                state = self.wait(state);
            } else {
                waited = true;
                drop(state);
                turn(true);
                state = self.lock();
            }
        }
        state.busy = true;
        drop(state);
        if waited {
            turn(false);
        }
        let _turn = Turn { queue: self, jobs: Vec::new(), leader: 0, outcomes: Vec::new() };
        Some(work())
    }

    /// Runs `exec(command, targets, advance)` once no other installer is running and no
    /// [`Ticket`] for `command` is still out, with the targets of every job waiting for the same
    /// `command`, and gives each of those jobs its own outcome: `exec` returns one per target, in
    /// order (a missing one counts as an interrupted installer). `ticket`, this job's own, is
    /// redeemed as it joins the queue.
    ///
    /// `step` tells the job it has to wait, then that its call is running and each stage the call
    /// reaches: the job that makes the call reports its stages to the others with `advance` (and
    /// to itself however it likes), and never hears `step` for its own call unless it waited.
    ///
    /// A job that is waiting leaves with [`CANCELLED`] once `cancelled` (asked a few times a
    /// second, so it must not block) says so; once its call has started it sees it through.
    #[allow(clippy::type_complexity)]
    pub fn batch(
        &self,
        ticket: Option<Ticket<'_, T>>,
        command: Vec<OsString>,
        target: T,
        cancelled: &dyn Fn() -> bool,
        step: &mut dyn FnMut(Step),
        exec: &dyn Fn(&[OsString], &[T], &dyn Fn(u32)) -> Vec<Outcome>,
    ) -> Outcome {
        let mut state = self.lock();
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.waiting.push(Waiting { id, command: command.clone(), target });
        // Joining the queue and redeeming the ticket happen together, so no call can start in
        // between and miss this job.
        if let Some(mut ticket) = ticket
            && let Some(expected) = ticket.command.take()
        {
            state.forget(&expected);
        }
        let (mut waited, mut told) = (false, None);
        loop {
            if let Some(at) = state.finished.iter().position(|(done, _)| *done == id) {
                return state.finished.swap_remove(at).1;
            }
            // Another job took this one into its call.
            let running = state.running.contains(&id);
            if !running && waited && cancelled() {
                state.waiting.retain(|w| w.id != id);
                return Err(CANCELLED.into());
            }
            if !running && !state.busy && !state.expected.contains(&command) {
                break;
            }
            let tell = if running {
                let now = Step::Running(state.stage);
                (waited && told != Some(now)).then_some(now)
            } else {
                (!waited).then_some(Step::Waiting)
            };
            match tell {
                Some(s) => {
                    waited = true;
                    told = Some(s);
                    drop(state);
                    step(s);
                    state = self.lock();
                }
                None => state = self.wait(state),
            }
        }
        let (batch, rest): (Vec<Waiting<T>>, Vec<Waiting<T>>) = std::mem::take(&mut state.waiting).into_iter().partition(|w| w.command == command);
        state.waiting = rest;
        state.busy = true;
        state.stage = 0;
        state.running = batch.iter().map(|w| w.id).collect();
        self.changed.notify_all();
        drop(state);
        if waited {
            step(Step::Running(0));
        }
        let jobs: Vec<u64> = batch.iter().map(|w| w.id).collect();
        let targets: Vec<T> = batch.into_iter().map(|w| w.target).collect();
        let mut call = Turn { queue: self, jobs, leader: id, outcomes: Vec::new() };
        let advance = |stage: u32| {
            self.lock().stage = stage;
            self.changed.notify_all();
        };
        call.outcomes = exec(&command, &targets, &advance);
        let own = call.jobs.iter().position(|j| *j == id).and_then(|at| call.outcomes.get(at).cloned());
        own.unwrap_or_else(|| Err(INTERRUPTED.into()))
    }
}

impl Queue<OsString> {
    /// [`Queue::batch`] for an installer whose one call succeeds or fails as a whole: every job
    /// of the call gets its outcome. `turn(true)` says the job has to wait, `turn(false)` that its
    /// call has started.
    pub fn batched(
        &self,
        command: Vec<OsString>,
        target: OsString,
        cancelled: &dyn Fn() -> bool,
        turn: &mut dyn FnMut(bool),
        exec: &dyn Fn(&[OsString], &[OsString]) -> Outcome,
    ) -> Outcome {
        // Such a call has no stages past its start.
        let mut step = |s: Step| {
            if matches!(s, Step::Waiting | Step::Running(0)) {
                turn(s == Step::Waiting);
            }
        };
        self.batch(None, command, target, cancelled, &mut step, &|command, targets, _| {
            let outcome = exec(command, targets);
            vec![outcome; targets.len()]
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    /// [`Queue::batched`] for a job that took a [`Ticket`] before its download.
    fn batched_with(
        queue: &Queue,
        ticket: Option<Ticket<'_, OsString>>,
        command: Vec<OsString>,
        target: OsString,
        cancelled: &dyn Fn() -> bool,
        turn: &mut dyn FnMut(bool),
        exec: &dyn Fn(&[OsString], &[OsString]) -> Outcome,
    ) -> Outcome {
        let mut step = |s: Step| {
            if matches!(s, Step::Waiting | Step::Running(0)) {
                turn(s == Step::Waiting);
            }
        };
        queue.batch(ticket, command, target, cancelled, &mut step, &|command, targets, _| vec![exec(command, targets); targets.len()])
    }

    fn line(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(|p| os(p)).collect()
    }

    fn install() -> Vec<OsString> {
        line(&["apt-get", "install", "-y"])
    }

    fn remove() -> Vec<OsString> {
        line(&["apt-get", "remove", "-y"])
    }

    /// Every call made, as `command + targets` with the targets sorted.
    #[derive(Default)]
    struct Calls(Mutex<Vec<Vec<OsString>>>);

    impl Calls {
        fn record(&self, command: &[OsString], targets: &[OsString]) {
            let mut targets = targets.to_vec();
            targets.sort();
            self.0.lock().unwrap().push(command.iter().cloned().chain(targets).collect());
        }

        fn taken(&self) -> Vec<Vec<OsString>> {
            let mut calls = std::mem::take(&mut *self.0.lock().unwrap());
            calls.sort();
            calls
        }
    }

    /// Starts `jobs` (command, target) while an install is running, so they all wait; lets that
    /// install finish once every one of them has said it is waiting. Returns their outcomes.
    fn behind_a_running_install(
        queue: &Queue,
        jobs: &[(Vec<OsString>, &str)],
        exec: &(dyn Fn(&[OsString], &[OsString]) -> Outcome + Sync),
    ) -> Vec<Option<Outcome>> {
        let (started, has_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let (turned, turns) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                queue.alone(&|| false, &mut |_| {}, || {
                    started.send(()).unwrap();
                    released.recv().unwrap();
                });
            });
            has_started.recv().unwrap();
            let handles: Vec<_> = jobs
                .iter()
                .map(|(command, target)| {
                    let turned = turned.clone();
                    scope.spawn(move || {
                        let mut turn = |waiting: bool| {
                            if waiting {
                                turned.send(()).unwrap();
                            }
                        };
                        queue.batched(command.clone(), os(target), &|| false, &mut turn, exec)
                    })
                })
                .collect();
            for _ in jobs {
                turns.recv().unwrap();
            }
            release.send(()).unwrap();
            handles.into_iter().map(|h| h.join().ok()).collect()
        })
    }

    #[test]
    fn jobs_that_wait_together_share_one_call_per_command() {
        let queue: Queue = Queue::new();
        let calls = Calls::default();
        let jobs = [(install(), "a.deb"), (install(), "b.deb"), (remove(), "photocraft"), (install(), "c.deb")];
        let results = behind_a_running_install(&queue, &jobs, &|command, targets| {
            calls.record(command, targets);
            Ok(())
        });
        assert_eq!(results, vec![Some(Ok(())); 4]);
        assert_eq!(calls.taken(), [line(&["apt-get", "install", "-y", "a.deb", "b.deb", "c.deb"]), line(&["apt-get", "remove", "-y", "photocraft"])]);
    }

    #[test]
    fn installers_never_run_at_the_same_time() {
        let queue: Queue = Queue::new();
        let (inside, most, handled) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
        let enter = || {
            most.fetch_max(inside.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(3));
            inside.fetch_sub(1, Ordering::SeqCst);
        };
        std::thread::scope(|scope| {
            for i in 0..24 {
                let (queue, enter, handled) = (&queue, &enter, &handled);
                scope.spawn(move || {
                    std::thread::sleep(Duration::from_millis(i % 5));
                    let exec = |_: &[OsString], targets: &[OsString]| {
                        enter();
                        handled.fetch_add(targets.len(), Ordering::SeqCst);
                        Ok(())
                    };
                    match i % 3 {
                        0 => queue.batched(install(), os(&format!("{i}.deb")), &|| false, &mut |_| {}, &exec).unwrap(),
                        1 => queue.batched(remove(), os(&format!("app{i}")), &|| false, &mut |_| {}, &exec).unwrap(),
                        _ => queue
                            .alone(&|| false, &mut |_| {}, || {
                                enter();
                                handled.fetch_add(1, Ordering::SeqCst);
                            })
                            .unwrap(),
                    }
                });
            }
        });
        assert_eq!(most.load(Ordering::SeqCst), 1);
        assert_eq!(handled.load(Ordering::SeqCst), 24);
    }

    #[test]
    fn a_job_arriving_mid_install_waits_then_runs() {
        let queue: Queue = Queue::new();
        let calls = Calls::default();
        let (started, has_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let (turned, turns) = mpsc::channel();
        std::thread::scope(|scope| {
            let (queue, calls) = (&queue, &calls);
            let first = scope.spawn(move || {
                queue.batched(install(), os("a.deb"), &|| false, &mut |_| panic!("the first job never waits"), &|command, targets| {
                    calls.record(command, targets);
                    started.send(()).unwrap();
                    released.recv().unwrap();
                    Ok(())
                })
            });
            has_started.recv().unwrap();
            let second = scope.spawn(move || {
                queue.batched(install(), os("b.deb"), &|| false, &mut |waiting| turned.send(waiting).unwrap(), &|command, targets| {
                    calls.record(command, targets);
                    Ok(())
                })
            });
            assert!(turns.recv().unwrap(), "the second job says it is waiting");
            assert_eq!(calls.0.lock().unwrap().len(), 1, "and does not start while the first runs");
            release.send(()).unwrap();
            assert_eq!(first.join().unwrap(), Ok(()));
            assert_eq!(second.join().unwrap(), Ok(()));
            assert!(!turns.recv().unwrap(), "then that its turn came");
        });
        assert_eq!(calls.taken(), [line(&["apt-get", "install", "-y", "a.deb"]), line(&["apt-get", "install", "-y", "b.deb"])]);
    }

    #[test]
    fn a_job_taken_into_another_jobs_call_hears_it_started() {
        let queue: Queue = Queue::new();
        let (started, has_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let (turned, turns) = mpsc::channel();
        let installing = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let (queue, installing) = (&queue, &installing);
            scope.spawn(move || {
                queue.alone(&|| false, &mut |_| {}, || {
                    started.send(()).unwrap();
                    released.recv().unwrap();
                });
            });
            has_started.recv().unwrap();
            for file in ["a.deb", "b.deb"] {
                let turned = turned.clone();
                scope.spawn(move || {
                    let mut turn = |waiting: bool| {
                        if !waiting {
                            installing.fetch_add(1, Ordering::SeqCst);
                        }
                        turned.send(waiting).unwrap();
                    };
                    // The call lasts until both jobs know it is under way.
                    let exec = |_: &[OsString], targets: &[OsString]| {
                        assert_eq!(targets.len(), 2);
                        while installing.load(Ordering::SeqCst) < 2 {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Ok(())
                    };
                    assert_eq!(queue.batched(install(), os(file), &|| false, &mut turn, &exec), Ok(()));
                });
            }
            assert!(turns.recv().unwrap() && turns.recv().unwrap());
            release.send(()).unwrap();
        });
        assert_eq!(turns.try_iter().collect::<Vec<_>>(), [false, false]);
    }

    #[test]
    fn a_dismissed_prompt_cancels_every_job_of_the_call() {
        let queue: Queue = Queue::new();
        let calls = Calls::default();
        let jobs = [(install(), "a.deb"), (install(), "b.deb"), (install(), "c.deb")];
        let results = behind_a_running_install(&queue, &jobs, &|command, targets| {
            calls.record(command, targets);
            Err("cancelled".into())
        });
        assert_eq!(calls.taken().len(), 1);
        assert_eq!(results, vec![Some(Err("cancelled".to_string())); 3]);
    }

    #[test]
    fn a_waiting_job_can_be_cancelled_and_is_left_out_of_the_call() {
        let queue: Queue = Queue::new();
        let calls = Calls::default();
        let cancel = AtomicBool::new(false);
        let (started, has_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let (turned, turns) = mpsc::channel();
        std::thread::scope(|scope| {
            let (queue, calls, cancel) = (&queue, &calls, &cancel);
            scope.spawn(move || {
                queue.alone(&|| false, &mut |_| {}, || {
                    started.send(()).unwrap();
                    released.recv().unwrap();
                });
            });
            has_started.recv().unwrap();
            let exec = |command: &[OsString], targets: &[OsString]| {
                calls.record(command, targets);
                Ok(())
            };
            let turned_too = turned.clone();
            let leaving = scope.spawn(move || {
                queue.batched(install(), os("a.deb"), &|| cancel.load(Ordering::SeqCst), &mut |waiting| turned.send(waiting).unwrap(), &exec)
            });
            let staying =
                scope.spawn(move || queue.batched(install(), os("b.deb"), &|| false, &mut |waiting| turned_too.send(waiting).unwrap(), &exec));
            assert!(turns.recv().unwrap() && turns.recv().unwrap());
            cancel.store(true, Ordering::SeqCst);
            // It leaves while the other install is still running, without waiting for its turn.
            assert_eq!(leaving.join().unwrap(), Err("cancelled".to_string()));
            assert!(calls.0.lock().unwrap().is_empty());
            release.send(()).unwrap();
            assert_eq!(staying.join().unwrap(), Ok(()));
        });
        assert_eq!(calls.taken(), [line(&["apt-get", "install", "-y", "b.deb"])]);
    }

    #[test]
    fn waiting_alone_for_the_installer_can_be_cancelled() {
        let queue: Queue = Queue::new();
        let (started, has_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        std::thread::scope(|scope| {
            let queue = &queue;
            scope.spawn(move || {
                queue.alone(&|| false, &mut |_| {}, || {
                    started.send(()).unwrap();
                    released.recv().unwrap();
                });
            });
            has_started.recv().unwrap();
            let ran = queue.alone(&|| true, &mut |_| {}, || panic!("a cancelled job never runs"));
            assert_eq!(ran, None);
            release.send(()).unwrap();
        });
    }

    #[test]
    fn a_job_whose_call_has_started_sees_it_through() {
        let queue: Queue = Queue::new();
        let cancel = AtomicBool::new(false);
        let (in_call, is_in_call) = mpsc::channel();
        let (finish, finished) = mpsc::channel::<()>();
        let finished = Mutex::new(finished);
        let jobs = [(install(), "a.deb"), (install(), "b.deb")];
        std::thread::scope(|scope| {
            let (queue, cancel, finished) = (&queue, &cancel, &finished);
            let (started, has_started) = mpsc::channel();
            let (release, released) = mpsc::channel::<()>();
            let (turned, turns) = mpsc::channel();
            scope.spawn(move || {
                queue.alone(&|| false, &mut |_| {}, || {
                    started.send(()).unwrap();
                    released.recv().unwrap();
                });
            });
            has_started.recv().unwrap();
            let handles: Vec<_> = jobs
                .iter()
                .map(|(command, target)| {
                    let (turned, in_call) = (turned.clone(), in_call.clone());
                    scope.spawn(move || {
                        let mut turn = |waiting: bool| {
                            if waiting {
                                turned.send(()).unwrap();
                            }
                        };
                        queue.batched(command.clone(), os(target), &|| cancel.load(Ordering::SeqCst), &mut turn, &|_, targets| {
                            assert_eq!(targets.len(), 2);
                            in_call.send(()).unwrap();
                            finished.lock().unwrap().recv().unwrap();
                            Ok(())
                        })
                    })
                })
                .collect();
            turns.recv().unwrap();
            turns.recv().unwrap();
            release.send(()).unwrap();
            is_in_call.recv().unwrap();
            // Cancelled mid-call: the job taken into the other's call has time to notice, and must not leave.
            cancel.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(300));
            finish.send(()).unwrap();
            for h in handles {
                assert_eq!(h.join().unwrap(), Ok(()));
            }
        });
    }

    #[test]
    fn an_installer_that_dies_frees_the_queue_and_fails_its_jobs() {
        let queue: Queue = Queue::new();
        let jobs = [(install(), "a.deb"), (install(), "b.deb")];
        let results = behind_a_running_install(&queue, &jobs, &|_, targets| {
            assert_eq!(targets.len(), 2);
            panic!("the installer died")
        });
        // The job that made the call unwound; the other was told.
        assert!(results.contains(&None), "{results:?}");
        assert!(results.contains(&Some(Err("the system installer was interrupted".to_string()))), "{results:?}");
        assert_eq!(queue.batched(install(), os("c.deb"), &|| false, &mut |_| {}, &|_, _| Ok(())), Ok(()));
    }

    #[test]
    fn a_batch_waits_for_jobs_still_downloading_then_takes_them_all() {
        let queue: Queue = Queue::new();
        let calls = Calls::default();
        let exec = |command: &[OsString], targets: &[OsString]| {
            calls.record(command, targets);
            Ok(())
        };
        let (turned, turns) = mpsc::channel();
        std::thread::scope(|scope| {
            let (queue, exec) = (&queue, &exec);
            let slow = queue.expect(install());
            let fast = queue.expect(install());
            // The first download is ready: nothing is running, but the other is still coming.
            let first =
                scope.spawn(move || batched_with(queue, Some(fast), install(), os("a.deb"), &|| false, &mut |w| turned.send(w).unwrap(), exec));
            assert!(turns.recv().unwrap(), "it waits for the other download");
            assert!(calls.0.lock().unwrap().is_empty());
            let second = scope.spawn(move || batched_with(queue, Some(slow), install(), os("b.deb"), &|| false, &mut |_| {}, exec));
            assert_eq!(first.join().unwrap(), Ok(()));
            assert_eq!(second.join().unwrap(), Ok(()));
        });
        assert_eq!(calls.taken(), [line(&["apt-get", "install", "-y", "a.deb", "b.deb"])]);
    }

    #[test]
    fn a_job_whose_download_ends_otherwise_leaves_the_batch() {
        let queue: Queue = Queue::new();
        let calls = Calls::default();
        std::thread::scope(|scope| {
            let (queue, calls) = (&queue, &calls);
            let failing = queue.expect(install());
            let (turned, turns) = mpsc::channel();
            let ready = scope.spawn(move || {
                batched_with(queue, None, install(), os("a.deb"), &|| false, &mut |w| turned.send(w).unwrap(), &|command, targets| {
                    calls.record(command, targets);
                    Ok(())
                })
            });
            assert!(turns.recv().unwrap());
            // Its download failed, was paused or cancelled.
            drop(failing);
            assert_eq!(ready.join().unwrap(), Ok(()));
        });
        assert_eq!(calls.taken(), [line(&["apt-get", "install", "-y", "a.deb"])]);
    }

    #[test]
    fn a_ticket_for_another_command_holds_nothing_back() {
        let queue: Queue = Queue::new();
        let _other = queue.expect(remove());
        assert_eq!(queue.batched(install(), os("a.deb"), &|| false, &mut |_| panic!("nothing to wait for"), &|_, _| Ok(())), Ok(()));
    }

    #[test]
    fn each_job_of_a_batch_gets_its_own_outcome_and_hears_its_stages() {
        let queue: Queue<u32> = Queue::new();
        let (turned, turns) = mpsc::channel();
        let results = std::thread::scope(|scope| {
            let queue = &queue;
            let (started, has_started) = mpsc::channel();
            let (release, released) = mpsc::channel::<()>();
            scope.spawn(move || {
                queue.alone(&|| false, &mut |_| {}, || {
                    started.send(()).unwrap();
                    released.recv().unwrap();
                });
            });
            has_started.recv().unwrap();
            let (waiting, is_waiting) = mpsc::channel();
            let handles: Vec<_> = (0..3)
                .map(|n| {
                    let (turned, waiting) = (turned.clone(), waiting.clone());
                    scope.spawn(move || {
                        let mut step = |s: Step| {
                            if s == Step::Waiting {
                                waiting.send(()).unwrap();
                            }
                            turned.send((n, s)).unwrap();
                        };
                        let exec = |_: &[OsString], targets: &[u32], advance: &dyn Fn(u32)| {
                            advance(1);
                            // The others hear the new stage before the call ends.
                            std::thread::sleep(Duration::from_millis(300));
                            targets.iter().map(|t| if t % 2 == 0 { Ok(()) } else { Err(format!("{t} failed")) }).collect()
                        };
                        queue.batch(None, install(), n, &|| false, &mut step, &exec)
                    })
                })
                .collect();
            for _ in 0..3 {
                is_waiting.recv().unwrap();
            }
            release.send(()).unwrap();
            handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
        });
        assert_eq!(results, [Ok(()), Err("1 failed".to_string()), Ok(())]);
        let heard: Vec<(u32, Step)> = turns.try_iter().collect();
        for n in 0..3 {
            let own: Vec<Step> = heard.iter().filter(|(job, _)| *job == n).map(|(_, s)| *s).collect();
            assert_eq!(own.first(), Some(&Step::Waiting), "{heard:?}");
            assert!(matches!(own.get(1), Some(Step::Running(_))), "{heard:?}");
        }
        // The one that made the call heard it start; the two that followed it heard where it
        // had got to (a job that wakes late hears the current stage only).
        let last = |n: u32| heard.iter().rev().find(|(job, _)| *job == n).map(|(_, s)| *s);
        let mut ends: Vec<Option<Step>> = (0..3).map(last).collect();
        ends.sort_by_key(|s| format!("{s:?}"));
        assert_eq!(ends, [Some(Step::Running(0)), Some(Step::Running(1)), Some(Step::Running(1))], "{heard:?}");
    }
}
