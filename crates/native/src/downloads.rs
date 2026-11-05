//! One download at a time, or as many as the user allows ([`Downloads::set_limit`]).
//!
//! Downloads running side by side split the connection, so each crawls and none is ready soon.
//! Here they take turns: a job that asks while the limit is reached waits, and the waiting jobs go
//! in the order their apps are on screen ([`Downloads::set_order`]), not the order they were
//! asked for. A job takes its place in line when it starts ([`Downloads::join`]), so an app higher
//! on screen goes first even when a job below it reaches its download sooner.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use artcraft_launcher_core::SUITE;

struct Waiting {
    id: u64,
    app: String,
}

/// The most downloads the user can let run at once.
pub const MAX_AT_ONCE: usize = 4;

struct State {
    /// Downloads running now.
    running: usize,
    /// Downloads that may run at once (1 to [`MAX_AT_ONCE`], or `usize::MAX` for no limit).
    limit: usize,
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

/// A job's place in line, held from when the job starts until its turn comes. Dropping it (the
/// job failed, was stopped or found its download already done) leaves the line.
pub struct Place<'a> {
    queue: &'a Downloads,
    id: u64,
}

impl Drop for Place<'_> {
    /// Leaves the line and wakes the others, so the next one can start.
    fn drop(&mut self) {
        self.queue.lock().waiting.retain(|w| w.id != self.id);
        self.queue.changed.notify_all();
    }
}

impl<'a> Place<'a> {
    /// Waits until it is this job's turn to download. `waiting` is called once, when the job has
    /// to wait. `stop` is asked a few times a second while it waits (so it must not block); its
    /// error ends the wait, and the job leaves the line.
    pub fn turn(self, stop: &dyn Fn() -> Result<(), String>, waiting: &mut dyn FnMut()) -> Result<Turn<'a>, String> {
        let queue = self.queue;
        let mut state = queue.lock();
        let mut told = false;
        loop {
            if state.running < state.limit && state.next() == Some(self.id) {
                break;
            }
            // The lock is let go before the place leaves the line (locals drop before `self`).
            stop()?;
            if told {
                state = queue.changed.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(PoisonError::into_inner).0;
            } else {
                told = true;
                drop(state);
                waiting();
                state = queue.lock();
            }
        }
        state.running += 1;
        drop(state);
        // Leaving the line wakes the others: with another free slot, the next one can start too.
        drop(self);
        Ok(Turn { queue })
    }
}

/// A job's turn to download. The next job's turn starts when it is dropped (even by unwinding).
pub struct Turn<'a> {
    queue: &'a Downloads,
}

impl Drop for Turn<'_> {
    /// Frees the slot and wakes the waiting jobs.
    fn drop(&mut self) {
        let mut state = self.queue.lock();
        state.running = state.running.saturating_sub(1);
        drop(state);
        self.queue.changed.notify_all();
    }
}

impl Default for Downloads {
    /// An empty queue that lets one download run at a time.
    fn default() -> Self {
        Self::new()
    }
}

impl Downloads {
    /// An empty queue that lets one download run at a time; `const` so it can be a `static`.
    pub const fn new() -> Self {
        let state = State { running: 0, limit: 1, next_id: 0, waiting: Vec::new(), order: Vec::new() };
        Self { state: Mutex::new(state), changed: Condvar::new() }
    }

    /// Locks the state; a job that panicked while holding it doesn't stop the others.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The app ids in the order they are on screen: waiting jobs go in this order.
    pub fn set_order(&self, order: Vec<String>) {
        self.lock().order = order;
        self.changed.notify_all();
    }

    /// How many downloads may run at once: 0 means no limit, and more than [`MAX_AT_ONCE`] counts as that.
    /// Lowering it lets running downloads finish; waiting ones start as they do.
    pub fn set_limit(&self, limit: usize) {
        self.lock().limit = if limit == 0 { usize::MAX } else { limit.min(MAX_AT_ONCE) };
        self.changed.notify_all();
    }

    /// Takes a place in line for `app`. Until it is dropped or used, apps below it on screen wait
    /// for it, even while its job hasn't reached its download yet.
    pub fn join(&self, app: &str) -> Place<'_> {
        let mut state = self.lock();
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.waiting.push(Waiting { id, app: app.to_string() });
        Place { queue: self, id }
    }

    /// Takes a place in line for `app` and waits for its turn: [`Place::turn`].
    pub fn turn(&self, app: &str, stop: &dyn Fn() -> Result<(), String>, waiting: &mut dyn FnMut()) -> Result<Turn<'_>, String> {
        self.join(app).turn(stop, waiting)
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

    /// A download with nothing else running starts at once, and so does the next one after it.
    #[test]
    fn a_download_alone_never_waits() {
        let queue = Downloads::new();
        drop(queue.turn("photocraft", &GO, &mut || panic!("nothing else is downloading")).unwrap());
        drop(queue.turn("filmcraft", &GO, &mut || panic!("the first download is over")).unwrap());
    }

    /// With the default limit, no two of the suite's apps download together.
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

    /// A higher limit lets several run together, never more than it allows.
    #[test]
    fn downloads_never_run_beyond_the_limit() {
        let queue = Downloads::new();
        queue.set_limit(3);
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
        assert!(most.load(Ordering::SeqCst) <= 3);
    }

    /// With a limit of two, a second download starts while the first is still running.
    #[test]
    fn a_second_download_runs_alongside_when_the_limit_allows() {
        let queue = Downloads::new();
        queue.set_limit(2);
        std::thread::scope(|scope| {
            let release = hold(scope, &queue);
            drop(queue.turn("wordcraft", &GO, &mut || panic!("one slot is free")).unwrap());
            release.send(()).unwrap();
        });
    }

    /// A waiting download starts at once when the limit is raised, without the running one
    /// finishing.
    #[test]
    fn raising_the_limit_starts_a_waiting_download() {
        let queue = Downloads::new();
        std::thread::scope(|scope| {
            let release = hold(scope, &queue);
            let (turned, turns) = mpsc::channel();
            let queue = &queue;
            let waiting = scope.spawn(move || queue.turn("wordcraft", &GO, &mut || turned.send(()).unwrap()).map(drop));
            turns.recv().unwrap();
            queue.set_limit(2);
            // It starts while the other download is still running.
            assert_eq!(waiting.join().unwrap(), Ok(()));
            release.send(()).unwrap();
        });
    }

    /// Zero lifts the limit, and anything above the most counts as the most.
    #[test]
    fn zero_lifts_the_limit_and_the_rest_stops_at_the_most() {
        let queue = Downloads::new();
        queue.set_limit(0);
        assert_eq!(queue.lock().limit, usize::MAX);
        queue.set_limit(9);
        assert_eq!(queue.lock().limit, MAX_AT_ONCE);
        queue.set_limit(1);
        assert_eq!(queue.lock().limit, 1);
    }

    /// With no limit, every app downloads at once and none waits for another.
    #[test]
    fn with_no_limit_every_download_runs_at_once() {
        let queue = Downloads::new();
        queue.set_limit(0);
        let turns: Vec<_> = SUITE.iter().map(|app| queue.turn(app.id, &GO, &mut || panic!("{} should not wait", app.id)).unwrap()).collect();
        assert_eq!(queue.lock().running, SUITE.len());
        drop(turns);
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

    /// With no screen order sent, waiting downloads go in suite order, not the order they asked in.
    #[test]
    fn waiting_downloads_go_in_suite_order_without_a_screen_order() {
        let queue = Downloads::new();
        assert_eq!(finishing_order(&queue, &["soundcraft", "wordcraft", "lightcraft"], || {}), ["lightcraft", "wordcraft", "soundcraft"]);
    }

    /// Waiting downloads go in the order their apps are on screen.
    #[test]
    fn waiting_downloads_go_in_screen_order() {
        let queue = Downloads::new();
        queue.set_order(vec!["wordcraft".into(), "soundcraft".into(), "lightcraft".into()]);
        assert_eq!(finishing_order(&queue, &["lightcraft", "soundcraft", "wordcraft"], || {}), ["wordcraft", "soundcraft", "lightcraft"]);
    }

    /// Apps missing from the screen order (hidden by a filter or search) go after the listed ones,
    /// in suite order.
    #[test]
    fn waiting_downloads_not_on_screen_go_last_in_suite_order() {
        let queue = Downloads::new();
        queue.set_order(vec!["wordcraft".into(), "photocraft".into()]);
        let apps = ["soundcraft", "wordcraft", "lightcraft", "photocraft"];
        assert_eq!(finishing_order(&queue, &apps, || {}), ["wordcraft", "photocraft", "lightcraft", "soundcraft"]);
    }

    /// Sending a new screen order changes who goes next among the waiting downloads.
    #[test]
    fn resorting_the_screen_reorders_the_waiting_downloads() {
        let queue = Downloads::new();
        queue.set_order(vec!["wordcraft".into(), "soundcraft".into(), "lightcraft".into()]);
        let resort = || queue.set_order(vec!["lightcraft".into(), "soundcraft".into(), "wordcraft".into()]);
        assert_eq!(finishing_order(&queue, &["wordcraft", "soundcraft", "lightcraft"], resort), ["lightcraft", "soundcraft", "wordcraft"]);
    }

    /// A stopped job leaves the line while the running download carries on, and the line works
    /// afterwards.
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

    /// A job that joined the line waits its turn even when a job below it on screen gets to its
    /// download first.
    #[test]
    fn a_job_higher_on_screen_goes_first_even_when_it_reaches_its_download_later() {
        let queue = Downloads::new();
        queue.set_order(vec!["wordcraft".into(), "lightcraft".into()]);
        let word = queue.join("wordcraft");
        std::thread::scope(|scope| {
            let (turned, turns) = mpsc::channel();
            let queue = &queue;
            let light = scope.spawn(move || queue.turn("lightcraft", &GO, &mut || turned.send(()).unwrap()).map(drop));
            // Nothing is downloading, yet LightCraft waits for WordCraft, which is above it.
            turns.recv().unwrap();
            drop(word.turn(&GO, &mut || panic!("WordCraft is first in line")).unwrap());
            assert_eq!(light.join().unwrap(), Ok(()));
        });
    }

    /// A place dropped before its turn (the job failed or found its file already downloaded) lets
    /// the next job start.
    #[test]
    fn a_place_given_up_before_its_turn_frees_the_line() {
        let queue = Downloads::new();
        queue.set_order(vec!["wordcraft".into(), "lightcraft".into()]);
        let word = queue.join("wordcraft");
        std::thread::scope(|scope| {
            let (turned, turns) = mpsc::channel();
            let queue = &queue;
            let light = scope.spawn(move || queue.turn("lightcraft", &GO, &mut || turned.send(()).unwrap()).map(drop));
            turns.recv().unwrap();
            // WordCraft's job failed, or found its download already done.
            drop(word);
            assert_eq!(light.join().unwrap(), Ok(()));
        });
        assert!(queue.lock().waiting.is_empty());
    }

    /// A download that panics still frees its slot.
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
