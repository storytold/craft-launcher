//! One download at a time.
//!
//! Downloads running side by side split the connection, so each crawls and none is ready soon.
//! Here they take turns: a job that asks while another downloads waits, and the waiting jobs go
//! in the order their apps are on screen ([`Downloads::set_order`]), not the order they were
//! asked for.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use artcraft_launcher_core::SUITE;

struct Waiting {
    id: u64,
    app: String,
}

struct State {
    busy: bool,
    next_id: u64,
    waiting: Vec<Waiting>,
    /// App ids in the order they are on screen.
    order: Vec<String>,
}

impl State {
    /// Where `app` goes in line: its place on screen, then (for apps not on screen) its place in
    /// the suite. `id` breaks ties, so the first to ask goes first.
    fn rank(&self, app: &str, id: u64) -> (usize, usize, u64) {
        let on_screen = self.order.iter().position(|a| a == app).unwrap_or(usize::MAX);
        let in_suite = SUITE.iter().position(|a| a.id == app).unwrap_or(usize::MAX);
        (on_screen, in_suite, id)
    }

    /// The waiting job whose turn is next.
    fn next(&self) -> Option<u64> {
        self.waiting.iter().min_by_key(|w| self.rank(&w.app, w.id)).map(|w| w.id)
    }
}

pub struct Downloads {
    state: Mutex<State>,
    changed: Condvar,
}

/// A job's turn to download. The next job's turn starts when it is dropped (even by unwinding).
pub struct Turn<'a> {
    queue: &'a Downloads,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.queue.lock().busy = false;
        self.queue.changed.notify_all();
    }
}

impl Default for Downloads {
    fn default() -> Self {
        Self::new()
    }
}

impl Downloads {
    pub const fn new() -> Self {
        let state = State { busy: false, next_id: 0, waiting: Vec::new(), order: Vec::new() };
        Self { state: Mutex::new(state), changed: Condvar::new() }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The app ids in the order they are on screen: waiting jobs go in this order.
    pub fn set_order(&self, order: Vec<String>) {
        self.lock().order = order;
        self.changed.notify_all();
    }

    /// Waits until it is `app`'s turn to download. `waiting` is called once, when the job has to
    /// wait. `stop` is asked a few times a second while it waits (so it must not block); its
    /// error ends the wait, and the job leaves the line.
    pub fn turn(&self, app: &str, stop: &dyn Fn() -> Result<(), String>, waiting: &mut dyn FnMut()) -> Result<Turn<'_>, String> {
        let mut state = self.lock();
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.waiting.push(Waiting { id, app: app.to_string() });
        let mut told = false;
        loop {
            if !state.busy && state.next() == Some(id) {
                break;
            }
            if let Err(e) = stop() {
                state.waiting.retain(|w| w.id != id);
                drop(state);
                self.changed.notify_all();
                return Err(e);
            }
            if told {
                state = self.changed.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(PoisonError::into_inner).0;
            } else {
                told = true;
                drop(state);
                waiting();
                state = self.lock();
            }
        }
        state.waiting.retain(|w| w.id != id);
        state.busy = true;
        Ok(Turn { queue: self })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;

    use super::*;

    const GO: fn() -> Result<(), String> = || Ok(());

    /// Holds the line with a running download until the returned sender is used.
    fn hold<'s, 'q>(scope: &'s std::thread::Scope<'s, 'q>, queue: &'q Downloads) -> mpsc::Sender<()> {
        let (started, has_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        scope.spawn(move || {
            let _turn = queue.turn("photocraft", &GO, &mut || panic!("the first download never waits")).unwrap();
            started.send(()).unwrap();
            released.recv().unwrap();
        });
        has_started.recv().unwrap();
        release
    }

    #[test]
    fn a_download_alone_never_waits() {
        let queue = Downloads::new();
        drop(queue.turn("photocraft", &GO, &mut || panic!("nothing else is downloading")).unwrap());
        drop(queue.turn("filmcraft", &GO, &mut || panic!("the first download is over")).unwrap());
    }

    #[test]
    fn downloads_never_run_at_the_same_time() {
        let queue = Downloads::new();
        let (inside, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for (i, app) in SUITE.iter().enumerate() {
                let (queue, inside, most) = (&queue, &inside, &most);
                scope.spawn(move || {
                    std::thread::sleep(Duration::from_millis(i as u64 % 4));
                    let _turn = queue.turn(app.id, &GO, &mut || {}).unwrap();
                    most.fetch_max(inside.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(3));
                    inside.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        assert_eq!(most.load(Ordering::SeqCst), 1);
    }

    /// Queues `apps` (in that order) behind a running download, then lets them all through.
    /// Returns the order they downloaded in.
    fn finishing_order(queue: &Downloads, apps: &[&'static str], before_release: impl FnOnce()) -> Vec<&'static str> {
        let done = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            let release = hold(scope, queue);
            let (turned, turns) = mpsc::channel();
            for app in apps {
                let (turned, done) = (turned.clone(), &done);
                scope.spawn(move || {
                    let _turn = queue.turn(app, &GO, &mut || turned.send(()).unwrap()).unwrap();
                    done.lock().unwrap().push(*app);
                });
                // Each one is waiting before the next is asked for.
                turns.recv().unwrap();
            }
            before_release();
            release.send(()).unwrap();
        });
        done.into_inner().unwrap()
    }

    #[test]
    fn waiting_downloads_go_in_suite_order_without_a_screen_order() {
        let queue = Downloads::new();
        assert_eq!(finishing_order(&queue, &["soundcraft", "wordcraft", "lightcraft"], || {}), ["lightcraft", "wordcraft", "soundcraft"]);
    }

    #[test]
    fn waiting_downloads_go_in_screen_order() {
        let queue = Downloads::new();
        queue.set_order(vec!["wordcraft".into(), "soundcraft".into(), "lightcraft".into()]);
        assert_eq!(finishing_order(&queue, &["lightcraft", "soundcraft", "wordcraft"], || {}), ["wordcraft", "soundcraft", "lightcraft"]);
    }

    #[test]
    fn resorting_the_screen_reorders_the_waiting_downloads() {
        let queue = Downloads::new();
        queue.set_order(vec!["wordcraft".into(), "soundcraft".into(), "lightcraft".into()]);
        let resort = || queue.set_order(vec!["lightcraft".into(), "soundcraft".into(), "wordcraft".into()]);
        assert_eq!(finishing_order(&queue, &["wordcraft", "soundcraft", "lightcraft"], resort), ["lightcraft", "soundcraft", "wordcraft"]);
    }

    #[test]
    fn a_waiting_download_can_be_stopped_and_leaves_the_line() {
        let queue = Downloads::new();
        let cancel = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let release = hold(scope, &queue);
            let (turned, turns) = mpsc::channel();
            let (queue, cancel) = (&queue, &cancel);
            let stop = move || if cancel.load(Ordering::SeqCst) { Err("cancelled".to_string()) } else { Ok(()) };
            let leaving = scope.spawn(move || queue.turn("lightcraft", &stop, &mut || turned.send(()).unwrap()).map(drop));
            turns.recv().unwrap();
            cancel.store(true, Ordering::SeqCst);
            // It leaves while the other download is still running.
            assert_eq!(leaving.join().unwrap(), Err("cancelled".to_string()));
            release.send(()).unwrap();
        });
        assert!(queue.lock().waiting.is_empty());
        drop(queue.turn("wordcraft", &GO, &mut || panic!("the line is empty")).unwrap());
    }

    #[test]
    fn a_download_that_dies_frees_the_line() {
        let queue = Downloads::new();
        std::thread::scope(|scope| {
            let died = scope.spawn(|| {
                let _turn = queue.turn("photocraft", &GO, &mut || {}).unwrap();
                panic!("the download died");
            });
            assert!(died.join().is_err());
        });
        drop(queue.turn("wordcraft", &GO, &mut || panic!("the line is free")).unwrap());
    }
}
