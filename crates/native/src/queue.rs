//! One system installer at a time.
//!
//! apt, dnf, rpm and the Windows Installer each hold a system-wide lock: an install started
//! while another runs fails. Jobs wait their turn here instead. Where the installer takes
//! several packages, the jobs waiting for the same command share one call, so the password is
//! asked for once.
//!
//! Jobs started together by one "update all" also *arrive* together: each waits for the others'
//! downloads ([`Gather`]) before the queue makes its call, so one call covers the whole batch.
//! Without that, a call goes out as soon as the first download finishes and every app gets its
//! own password prompt.

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::time::Instant;

use artcraft_launcher_core::protocol::CANCELLED;

type Outcome = Result<(), String>;

/// What one installer call does with its command and the targets sharing it: one outcome per
/// target, in their order.
type Exec<'a> = dyn Fn(&[OsString], &[OsString]) -> Vec<Outcome> + 'a;

/// What a job of a call that never reported hears (the call's job died mid-call).
const INTERRUPTED: &str = "the system installer was interrupted";

/// How long a gathered job waits for the rest of its batch. The gate opens then even if a
/// member is still nowhere near the installer, so a stuck download can't hold the others back
/// indefinitely.
const GATHER_TIMEOUT: Duration = Duration::from_secs(600);

/// The members of one batch (an "update all") that share a single permission prompt.
///
/// Every member counts down exactly once: [`Queue::batched_when`] does it when the member
/// reaches the installer queue, and [`Gather::leave`] when a member ends before that (its
/// download failed, it was cancelled, or its install needs no prompt at all). While anyone is
/// still on the way, the members already waiting hold back from starting the installer call;
/// once the count reaches zero — or [`GATHER_TIMEOUT`] passes — the call goes out with every
/// waiting member of the batch in it.
#[derive(Clone)]
pub struct Gather {
    remaining: Arc<AtomicUsize>,
    until: Instant,
}

/// The same batch, not two batches that happen to count the same: the handle a job carries is
/// its batch's identity (see [`Queue::gather`], whose members all share one).
impl PartialEq for Gather {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.remaining, &other.remaining)
    }
}

impl Eq for Gather {}

impl Gather {
    /// A member that will not reach the installer queue after all: its download failed, it was
    /// cancelled, or its install needs no prompt. The rest of the batch must not wait for it.
    pub fn leave(&self) {
        self.step();
    }

    /// One member is accounted for: waiting at the queue (arrived) or gone for good (left).
    fn step(&self) {
        let _ = self.remaining.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
    }

    /// Whether the batch may make its call: everyone has arrived or left, or the wait is over.
    pub(crate) fn open(&self) -> bool {
        self.remaining.load(Ordering::SeqCst) == 0 || Instant::now() >= self.until
    }
}

struct Waiting {
    id: u64,
    command: Vec<OsString>,
    target: OsString,
}

struct State {
    busy: bool,
    next_id: u64,
    waiting: Vec<Waiting>,
    /// The jobs of the call in progress.
    running: Vec<u64>,
    finished: Vec<(u64, Outcome)>,
}

pub struct Queue {
    state: Mutex<State>,
    changed: Condvar,
}

/// Frees the installer when a call ends (even by unwinding) and hands each of the other jobs of
/// the call its outcome.
struct Turn<'a> {
    queue: &'a Queue,
    /// The other jobs of the call and the outcome each is to hear: the call's per-job results
    /// once it ended, or the failure each was given here if it never reported.
    others: Vec<(u64, Outcome)>,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut state = self.queue.lock();
        state.busy = false;
        state.running.clear();
        for (id, outcome) in self.others.drain(..) {
            state.finished.push((id, outcome));
        }
        self.queue.changed.notify_all();
    }
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    pub const fn new() -> Self {
        let state = State { busy: false, next_id: 0, waiting: Vec::new(), running: Vec::new(), finished: Vec::new() };
        Self { state: Mutex::new(state), changed: Condvar::new() }
    }

    /// A batch of `members` jobs that will install behind one permission prompt (see
    /// [`Gather`]). The count is the jobs actually started: members already running their own
    /// job are not part of the batch.
    pub fn gather(&self, members: usize) -> Gather {
        self.gather_within(members, GATHER_TIMEOUT)
    }

    /// [`gather`](Self::gather) with the wait cap spelled out (the tests take a short one).
    fn gather_within(&self, members: usize, within: Duration) -> Gather {
        Gather { remaining: Arc::new(AtomicUsize::new(members)), until: Instant::now() + within }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Until something changes, or long enough to ask again whether the job was cancelled.
    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.changed.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(PoisonError::into_inner).0
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
        let _turn = Turn { queue: self, others: Vec::new() };
        Some(work())
    }

    /// Runs `exec(command, targets)` once no other installer is running, with the targets of
    /// every job waiting for the same `command`, and gives each of those jobs its own outcome
    /// from the call. `turn(true)` says the job has to wait, `turn(false)` that its call has
    /// started.
    ///
    /// A job that is waiting leaves with [`CANCELLED`] once `cancelled` (asked a few times a
    /// second, so it must not block) says so; once its call has started it sees it through.
    ///
    /// `exec` returns one outcome per target, in their order.
    pub fn batched_when(
        &self,
        gather: Option<Gather>,
        command: Vec<OsString>,
        target: OsString,
        cancelled: &dyn Fn() -> bool,
        turn: &mut dyn FnMut(bool),
        exec: &Exec<'_>,
    ) -> Outcome {
        let mut state = self.lock();
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.waiting.push(Waiting { id, command: command.clone(), target });
        drop(state);
        // Arrived: the rest of the batch is no longer held back by this one. The waiting entry
        // is in place first, so whoever finds the gate open finds every arrived member waiting.
        if let Some(gather) = &gather {
            gather.step();
        }
        let mut state = self.lock();
        let (mut waited, mut started) = (false, false);
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
            // An unopened gather holds the job back even on an idle queue: the call must cover
            // the whole batch, so it goes out only once everyone has arrived or left.
            let held_back = gather.as_ref().is_some_and(|gather| !gather.open());
            if !running && !state.busy && !held_back {
                break;
            }
            let tell = if running { waited && !started } else { !waited };
            if tell {
                (waited, started) = (true, running);
                drop(state);
                turn(!running);
                state = self.lock();
            } else {
                state = self.wait(state);
            }
        }
        let (batch, rest): (Vec<Waiting>, Vec<Waiting>) = std::mem::take(&mut state.waiting).into_iter().partition(|w| w.command == command);
        state.waiting = rest;
        state.busy = true;
        state.running = batch.iter().map(|w| w.id).collect();
        self.changed.notify_all();
        drop(state);
        if waited {
            turn(false);
        }
        let mine = batch.iter().position(|w| w.id == id);
        let others = batch.iter().filter(|w| w.id != id).map(|w| (w.id, Err(INTERRUPTED.to_string()))).collect();
        let mut call = Turn { queue: self, others };
        let targets: Vec<OsString> = batch.iter().map(|w| w.target.clone()).collect();
        let outcomes = exec(&command, &targets);
        for (at, waiting) in batch.iter().enumerate() {
            if waiting.id != id
                && let Some(outcome) = outcomes.get(at)
                && let Some(heard) = call.others.iter_mut().find(|(other, _)| *other == waiting.id)
            {
                heard.1.clone_from(outcome);
            }
        }
        mine.and_then(|at| outcomes.get(at)).cloned().unwrap_or_else(|| Err(INTERRUPTED.to_string()))
    }

    /// [`batched_when`](Self::batched_when) for a job on its own: the call's one outcome is
    /// what every job of it hears.
    pub fn batched(
        &self,
        command: Vec<OsString>,
        target: OsString,
        cancelled: &dyn Fn() -> bool,
        turn: &mut dyn FnMut(bool),
        exec: &dyn Fn(&[OsString], &[OsString]) -> Outcome,
    ) -> Outcome {
        self.batched_when(None, command, target, cancelled, turn, &|command, targets| vec![exec(command, targets); targets.len()])
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

        /// Up to `secs` for the first call to be made, checked every 10 ms.
        fn soon(&self, secs: u64) -> bool {
            (0..secs * 100).any(|_| {
                if !self.0.lock().unwrap().is_empty() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
                false
            })
        }
    }

    /// Starts `jobs` (command, target) while an install is running, so they all wait; lets that
    /// install finish once every one of them has said it is waiting. Returns their outcomes.
    #[allow(clippy::type_complexity)] // one shared installer call, spelled out for the threads
    fn behind_a_running_install(
        queue: &Queue,
        jobs: &[(Vec<OsString>, &str)],
        exec: &(dyn Fn(&[OsString], &[OsString]) -> Vec<Outcome> + Sync),
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
                        queue.batched_when(None, command.clone(), os(target), &|| false, &mut turn, &|command, targets| exec(command, targets))
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
        let queue = Queue::new();
        let calls = Calls::default();
        let jobs = [(install(), "a.deb"), (install(), "b.deb"), (remove(), "photocraft"), (install(), "c.deb")];
        let results = behind_a_running_install(&queue, &jobs, &|command, targets| {
            calls.record(command, targets);
            vec![Ok(()); targets.len()]
        });
        assert_eq!(results, vec![Some(Ok(())); 4]);
        assert_eq!(calls.taken(), [line(&["apt-get", "install", "-y", "a.deb", "b.deb", "c.deb"]), line(&["apt-get", "remove", "-y", "photocraft"])]);
    }

    #[test]
    fn each_job_of_a_call_hears_its_own_outcome() {
        let queue = Queue::new();
        let calls = Calls::default();
        let jobs = [(install(), "a.deb"), (install(), "b.deb")];
        let results = behind_a_running_install(&queue, &jobs, &|command, targets| {
            calls.record(command, targets);
            targets.iter().map(|t| if t == &os("b.deb") { Err("b.deb failed".to_string()) } else { Ok(()) }).collect()
        });
        // b.deb failed; a.deb, in the same call, did not.
        assert!(results.contains(&Some(Err("b.deb failed".to_string()))), "{results:?}");
        assert!(results.contains(&Some(Ok(()))), "{results:?}");
        assert_eq!(calls.taken().len(), 1);
    }

    #[test]
    fn installers_never_run_at_the_same_time() {
        let queue = Queue::new();
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
        let queue = Queue::new();
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
        let queue = Queue::new();
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
        let queue = Queue::new();
        let calls = Calls::default();
        let jobs = [(install(), "a.deb"), (install(), "b.deb"), (install(), "c.deb")];
        let results = behind_a_running_install(&queue, &jobs, &|command, targets| {
            calls.record(command, targets);
            vec![Err("cancelled".into()); targets.len()]
        });
        assert_eq!(calls.taken().len(), 1);
        assert_eq!(results, vec![Some(Err("cancelled".to_string())); 3]);
    }

    #[test]
    fn a_waiting_job_can_be_cancelled_and_is_left_out_of_the_call() {
        let queue = Queue::new();
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
                vec![Ok(()); targets.len()]
            };
            let turned_too = turned.clone();
            let leaving = scope.spawn(move || {
                queue.batched_when(
                    None,
                    install(),
                    os("a.deb"),
                    &|| cancel.load(Ordering::SeqCst),
                    &mut |waiting| turned.send(waiting).unwrap(),
                    &exec,
                )
            });
            let staying = scope
                .spawn(move || queue.batched_when(None, install(), os("b.deb"), &|| false, &mut |waiting| turned_too.send(waiting).unwrap(), &exec));
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
        let queue = Queue::new();
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
        let queue = Queue::new();
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
        let queue = Queue::new();
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
    fn a_gate_opens_once_every_member_has_arrived_or_left() {
        let queue = Queue::new();
        let gather = queue.gather(3);
        gather.leave();
        assert!(!gather.open(), "two members are still on the way");
        gather.leave();
        assert!(!gather.open(), "one member is still on the way");
        gather.leave();
        assert!(gather.open(), "the last one left before reaching the installer");
        let single = queue.gather(1);
        single.leave();
        // One member, counted twice by mistake, still leaves an open gate.
        single.leave();
        assert!(single.open());
    }

    /// Starts one gathered job per entry, `stagger` milliseconds apart, on an idle queue. The
    /// first would run at once if the gather didn't hold it back.
    #[allow(clippy::type_complexity)] // one shared installer call, spelled out for the threads
    fn gathered(
        queue: &Queue,
        gather: &Gather,
        jobs: &[(Vec<OsString>, &str)],
        stagger: u64,
        exec: &(dyn Fn(&[OsString], &[OsString]) -> Vec<Outcome> + Sync),
    ) -> Vec<Option<Outcome>> {
        std::thread::scope(|scope| {
            let handles: Vec<_> = jobs
                .iter()
                .enumerate()
                .map(|(at, (command, target))| {
                    let gather = gather.clone();
                    scope.spawn(move || {
                        std::thread::sleep(Duration::from_millis(stagger * at as u64));
                        queue.batched_when(Some(gather), command.clone(), os(target), &|| false, &mut |_| {}, &|command, targets| {
                            exec(command, targets)
                        })
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().ok()).collect()
        })
    }

    #[test]
    fn a_gathered_batch_makes_one_call_even_when_arrivals_are_staggered() {
        let queue = Queue::new();
        let calls = Calls::default();
        let gather = queue.gather(3);
        let jobs = [(install(), "a.deb"), (install(), "b.deb"), (install(), "c.deb")];
        let results = gathered(&queue, &gather, &jobs, 30, &|command, targets| {
            calls.record(command, targets);
            vec![Ok(()); targets.len()]
        });
        assert_eq!(results, vec![Some(Ok(())); 3]);
        assert_eq!(calls.taken(), [line(&["apt-get", "install", "-y", "a.deb", "b.deb", "c.deb"])], "all three in one call, not one each");
    }

    #[test]
    fn a_member_that_never_arrives_does_not_hold_the_batch_back() {
        let queue = Queue::new();
        let calls = Calls::default();
        // The wait cap is an hour: only the missing member's `leave` can open the gate.
        let gather = queue.gather_within(2, Duration::from_secs(3600));
        let arrived = std::thread::scope(|scope| {
            let job = scope.spawn(|| {
                queue.batched_when(Some(gather.clone()), install(), os("a.deb"), &|| false, &mut |_| {}, &|command, targets| {
                    calls.record(command, targets);
                    vec![Ok(()); targets.len()]
                })
            });
            // The other member's download failed before it ever reached the queue.
            std::thread::sleep(Duration::from_millis(50));
            gather.leave();
            job.join().unwrap()
        });
        assert_eq!(arrived, Ok(()));
        assert!(calls.soon(5), "the call went out once the missing member left");
        assert_eq!(calls.taken(), [line(&["apt-get", "install", "-y", "a.deb"])]);
    }

    #[test]
    fn a_gather_opens_after_its_wait_is_over() {
        let queue = Queue::new();
        let calls = Calls::default();
        let gather = queue.gather_within(2, Duration::from_millis(100));
        std::thread::scope(|scope| {
            scope.spawn(|| {
                queue.batched_when(Some(gather), install(), os("a.deb"), &|| false, &mut |_| {}, &|command, targets| {
                    calls.record(command, targets);
                    vec![Ok(()); targets.len()]
                })
            });
            // The second member never shows up; the cap lets the first go after 100 ms.
            assert!(calls.soon(5), "the call went out once the wait was over");
        });
        assert_eq!(calls.taken(), [line(&["apt-get", "install", "-y", "a.deb"])]);
    }

    #[test]
    fn a_cancelled_member_of_a_gather_lets_the_rest_go() {
        let queue = Queue::new();
        let calls = Calls::default();
        let gather = queue.gather(2);
        let cancel = AtomicBool::new(false);
        let (started, has_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let (turned, turns) = mpsc::channel();
        std::thread::scope(|scope| {
            let (queue, calls, cancel, gather) = (&queue, &calls, &cancel, &gather);
            scope.spawn(move || {
                queue.alone(&|| false, &mut |_| {}, || {
                    started.send(()).unwrap();
                    released.recv().unwrap();
                });
            });
            has_started.recv().unwrap();
            let exec = |command: &[OsString], targets: &[OsString]| {
                calls.record(command, targets);
                vec![Ok(()); targets.len()]
            };
            let staying_gather = gather.clone();
            let turned_too = turned.clone();
            let staying = scope.spawn(move || {
                queue.batched_when(
                    Some(staying_gather),
                    install(),
                    os("a.deb"),
                    &|| false,
                    &mut |waiting| {
                        if waiting {
                            turned_too.send(()).unwrap();
                        }
                    },
                    &exec,
                )
            });
            let leaving = scope.spawn(move || {
                queue.batched_when(
                    Some(gather.clone()),
                    install(),
                    os("b.deb"),
                    &|| cancel.load(Ordering::SeqCst),
                    &mut |waiting| {
                        if waiting {
                            turned.send(()).unwrap();
                        }
                    },
                    &exec,
                )
            });
            // Both have said they are waiting (behind the running install), then one is called off.
            turns.recv().unwrap();
            turns.recv().unwrap();
            cancel.store(true, Ordering::SeqCst);
            assert_eq!(leaving.join().unwrap(), Err("cancelled".to_string()));
            release.send(()).unwrap();
            assert_eq!(staying.join().unwrap(), Ok(()));
        });
        assert_eq!(calls.taken(), [line(&["apt-get", "install", "-y", "a.deb"])], "the cancelled member is not in the call");
    }
}
