//! One system installer at a time.
//!
//! apt, dnf, rpm and the Windows Installer each hold a system-wide lock: an install started
//! while another runs fails. Jobs wait their turn here instead. Where the installer takes
//! several packages, the jobs waiting for the same command share one call, so the password is
//! asked for once.

use std::ffi::OsString;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use artcraft_launcher_core::protocol::CANCELLED;

type Outcome = Result<(), String>;

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

/// Frees the installer when a call ends (even by unwinding) and hands its outcome to the other
/// jobs of the call.
struct Turn<'a> {
    queue: &'a Queue,
    others: Vec<u64>,
    outcome: Outcome,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut state = self.queue.lock();
        state.busy = false;
        state.running.clear();
        for id in self.others.drain(..) {
            state.finished.push((id, self.outcome.clone()));
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
        let _turn = Turn { queue: self, others: Vec::new(), outcome: Ok(()) };
        Some(work())
    }

    /// Runs `exec(command, targets)` once no other installer is running, with the targets of
    /// every job waiting for the same `command`, and gives each of those jobs its outcome.
    /// `turn(true)` says the job has to wait, `turn(false)` that its call has started.
    ///
    /// A job that is waiting leaves with [`CANCELLED`] once `cancelled` (asked a few times a
    /// second, so it must not block) says so; once its call has started it sees it through.
    pub fn batched(
        &self,
        command: Vec<OsString>,
        target: OsString,
        cancelled: &dyn Fn() -> bool,
        turn: &mut dyn FnMut(bool),
        exec: &dyn Fn(&[OsString], &[OsString]) -> Outcome,
    ) -> Outcome {
        self.batched_when(command, target, cancelled, turn, exec, &|| {}, &|_| true)
    }

    /// [`batched`](Self::batched) for a call that should not start until more jobs have joined it.
    ///
    /// `queued` is called once, as this job joins the waiting list. `ready` is asked, a few times
    /// a second, with how many jobs wait for the same `command`; a job takes the call only when
    /// it says yes, so the jobs of one call can be held until the last of them has arrived. Both
    /// run with the queue locked: they must not block.
    #[allow(clippy::too_many_arguments)]
    pub fn batched_when(
        &self,
        command: Vec<OsString>,
        target: OsString,
        cancelled: &dyn Fn() -> bool,
        turn: &mut dyn FnMut(bool),
        exec: &dyn Fn(&[OsString], &[OsString]) -> Outcome,
        queued: &dyn Fn(),
        ready: &dyn Fn(usize) -> bool,
    ) -> Outcome {
        let mut state = self.lock();
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.waiting.push(Waiting { id, command: command.clone(), target });
        queued();
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
            if !running && !state.busy && ready(state.waiting.iter().filter(|w| w.command == command).count()) {
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
        let others = batch.iter().map(|w| w.id).filter(|other| *other != id).collect();
        let targets: Vec<OsString> = batch.into_iter().map(|w| w.target).collect();
        let mut call = Turn { queue: self, others, outcome: Err("the system installer was interrupted".into()) };
        let outcome = exec(&command, &targets);
        call.outcome.clone_from(&outcome);
        outcome
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
        let queue = Queue::new();
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
            Err("cancelled".into())
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
}

/// A call that is held until every job it is waiting for has joined it.
#[cfg(test)]
mod gathering {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn a_call_is_held_until_the_last_job_has_joined_and_then_takes_them_all() {
        let queue = Queue::new();
        let calls = Mutex::new(Vec::<Vec<OsString>>::new());
        let joined = AtomicUsize::new(0);
        let results: Vec<Outcome> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..3u64)
                .map(|i| {
                    let (queue, calls, joined) = (&queue, &calls, &joined);
                    scope.spawn(move || {
                        // They arrive a good while apart: the first would run alone without the hold.
                        std::thread::sleep(Duration::from_millis(250 * i));
                        queue.batched_when(
                            vec![os("all-users")],
                            os(&format!("app{i}")),
                            &|| false,
                            &mut |_| {},
                            &|_, targets| {
                                calls.lock().unwrap().push(targets.to_vec());
                                Ok(())
                            },
                            &|| {
                                joined.fetch_add(1, Ordering::SeqCst);
                            },
                            &|waiting| waiting >= 3,
                        )
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(results, vec![Ok(()); 3]);
        assert_eq!(joined.load(Ordering::SeqCst), 3, "every job says once that it has joined");
        let mut calls = calls.into_inner().unwrap();
        assert_eq!(calls.len(), 1, "one call for all three: {calls:?}");
        let mut targets = calls.remove(0);
        targets.sort();
        assert_eq!(targets, [os("app0"), os("app1"), os("app2")]);
    }

    #[test]
    fn a_job_that_drops_out_does_not_hold_the_others_forever() {
        let queue = Queue::new();
        // `ready` is the caller's own count of who is still coming; here the third never comes.
        let still_coming = AtomicUsize::new(1);
        let calls = AtomicUsize::new(0);
        let targets_seen = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for name in ["a", "b"] {
                let (queue, still_coming, calls, targets_seen) = (&queue, &still_coming, &calls, &targets_seen);
                scope.spawn(move || {
                    queue
                        .batched_when(
                            vec![os("all-users")],
                            os(name),
                            &|| false,
                            &mut |_| {},
                            &|_, targets| {
                                calls.fetch_add(1, Ordering::SeqCst);
                                targets_seen.fetch_add(targets.len(), Ordering::SeqCst);
                                Ok(())
                            },
                            &|| {},
                            &|_| still_coming.load(Ordering::SeqCst) == 0,
                        )
                        .unwrap();
                });
            }
            std::thread::sleep(Duration::from_millis(400));
            assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing runs while a job is still coming");
            still_coming.store(0, Ordering::SeqCst); // it failed or was cancelled
        });
        assert_eq!((calls.load(Ordering::SeqCst), targets_seen.load(Ordering::SeqCst)), (1, 2));
    }

    #[test]
    fn a_cancel_ends_the_wait_without_running_anything() {
        let queue = Queue::new();
        let cancel = AtomicBool::new(false);
        let ran = AtomicBool::new(false);
        let outcome = std::thread::scope(|scope| {
            let job = scope.spawn(|| {
                queue.batched_when(
                    vec![os("all-users")],
                    os("a"),
                    &|| cancel.load(Ordering::SeqCst),
                    &mut |_| {},
                    &|_, _| {
                        ran.store(true, Ordering::SeqCst);
                        Ok(())
                    },
                    &|| {},
                    &|_| false,
                )
            });
            std::thread::sleep(Duration::from_millis(300));
            cancel.store(true, Ordering::SeqCst);
            job.join().unwrap()
        });
        assert_eq!(outcome, Err(CANCELLED.into()));
        assert!(!ran.load(Ordering::SeqCst));
        // The queue is free again.
        assert_eq!(queue.batched(vec![os("x")], os("t"), &|| false, &mut |_| {}, &|_, _| Ok(())), Ok(()));
    }

    #[test]
    fn a_job_that_is_told_to_wait_hears_it_once() {
        let queue = Queue::new();
        let waits = AtomicUsize::new(0);
        let go = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let job = scope.spawn(|| {
                queue.batched_when(
                    vec![os("all-users")],
                    os("a"),
                    &|| false,
                    &mut |waiting| {
                        if waiting {
                            waits.fetch_add(1, Ordering::SeqCst);
                        }
                    },
                    &|_, _| Ok(()),
                    &|| {},
                    &|_| go.load(Ordering::SeqCst),
                )
            });
            std::thread::sleep(Duration::from_millis(350));
            go.store(true, Ordering::SeqCst);
            assert_eq!(job.join().unwrap(), Ok(()));
        });
        assert_eq!(waits.load(Ordering::SeqCst), 1);
    }
}
