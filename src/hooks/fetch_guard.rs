//! Making a superseded fetch discard its own result.
//!
//! Every panel here follows the same shape: a `use_effect` kicks off a load on
//! mount, and a Refresh button kicks off the same load again. Both `spawn`
//! into the same signals, and nothing connects them — so two loads can be in
//! flight at once and the one that *finishes* last wins, not the one that
//! *started* last. Click Refresh twice, or click it while the initial load is
//! still running, and the panel can settle on the older answer. The failure is
//! invisible: the data looks plausible, it is just stale, and it stays stale
//! until something else triggers a redraw.
//!
//! **The counter is only ever touched with `peek`, never `read`.** `read`
//! subscribes the calling scope to the signal, and `begin` is called from
//! inside `use_effect` — so a `read` there subscribes the effect to a signal
//! the very next line writes, and the effect reruns forever. The symptom is
//! not a subtle one: the window flickers, nothing is clickable, and the app
//! spawns `az` processes as fast as it can. `peek` reads the same value
//! without subscribing. (`home_panel`'s poll closure carries the same warning
//! for the same reason.)
//!
//! A generation counter fixes it. Each load takes a token before it starts;
//! before publishing anything it checks whether its token is still the current
//! one, and a load that has been superseded returns without touching a single
//! signal — not its data, not its error, not its loading flag. The newest load
//! owns all of them.

use dioxus::prelude::*;

/// Hand out a guard for one panel's fetches. Call once per component.
pub fn use_fetch_guard() -> FetchGuard {
    FetchGuard {
        generation: use_signal(|| 0u64),
    }
}

#[derive(Clone, Copy)]
pub struct FetchGuard {
    generation: Signal<u64>,
}

impl FetchGuard {
    /// Claim the panel for a new load, superseding any in flight. Returns the
    /// token to check with [`FetchGuard::is_current`] before publishing.
    ///
    /// `peek`, never `read` — see the note on [`FetchGuard`].
    pub fn begin(&mut self) -> u64 {
        let next = *self.generation.peek() + 1;
        self.generation.set(next);
        next
    }

    /// Whether `token`'s load is still the one whose result the panel wants.
    ///
    /// `peek` for the same reason: this is called from inside spawned tasks,
    /// which subscribe the scope that spawned them.
    pub fn is_current(&self, token: u64) -> bool {
        *self.generation.peek() == token
    }
}

#[cfg(test)]
mod tests {
    /// The guard's logic without a renderer: a plain counter with the same
    /// rules, exercised as the panels use it.
    #[derive(Default)]
    struct Bare {
        generation: u64,
    }
    impl Bare {
        fn begin(&mut self) -> u64 {
            self.generation += 1;
            self.generation
        }
        fn is_current(&self, token: u64) -> bool {
            self.generation == token
        }
    }

    #[test]
    fn a_lone_fetch_is_current_when_it_finishes() {
        let mut g = Bare::default();
        let token = g.begin();
        assert!(g.is_current(token));
    }

    /// The actual bug: two loads in flight, the first finishing last. It must
    /// not publish.
    #[test]
    fn an_overtaken_fetch_does_not_publish() {
        let mut g = Bare::default();
        let first = g.begin();
        let second = g.begin();
        // `second` returns first — still current, so it publishes.
        assert!(g.is_current(second));
        // `first` returns afterwards and must stay silent.
        assert!(!g.is_current(first));
    }

    #[test]
    fn only_the_newest_of_many_survives() {
        let mut g = Bare::default();
        let tokens: Vec<u64> = (0..5).map(|_| g.begin()).collect();
        let current: Vec<bool> = tokens.iter().map(|t| g.is_current(*t)).collect();
        assert_eq!(current, vec![false, false, false, false, true]);
    }
}

/// Regression tests that drive a real `VirtualDom`.
///
/// The arithmetic tests above would have passed on the code that shipped in
/// v0.3.31 — the counter always counted correctly. The bug was about
/// *subscriptions*: taking the generation with `read` subscribed the calling
/// scope to a signal that was written on the next line, so a `use_effect`
/// that claimed a token re-ran forever. The window flickered, nothing was
/// clickable, and `az` was spawned as fast as the machine allowed.
///
/// A subscription bug is only observable to something that re-runs scopes, so
/// these mount an actual `VirtualDom` and count how many times the effect
/// fires. Each covers one shape a panel actually uses; each one fails against
/// the `read` version.
#[cfg(test)]
mod render_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    /// Mount `app` and settle it, with a deadline.
    ///
    /// With the bug present, `rebuild_in_place` never returns — the scope
    /// re-dirties itself as fast as it is rendered — so a direct call would
    /// hang the test binary, and CI with it. Mounting on a worker thread with
    /// a deadline turns "loops forever" into a assertable result. The worker
    /// is abandoned on timeout; the process reaps it when the suite ends.
    fn mount_and_settle(app: fn() -> Element, counter: &'static AtomicUsize) -> Result<usize, ()> {
        counter.store(0, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        // `VirtualDom` is not `Send`, so it is built inside the worker.
        std::thread::spawn(move || {
            let mut dom = VirtualDom::new(app);
            dom.rebuild_in_place();
            for _ in 0..50 {
                dom.render_immediate(&mut dioxus_core::NoOpMutations);
            }
            let _ = tx.send(());
        });
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(()) => Ok(counter.load(Ordering::SeqCst)),
            Err(_) => Err(()),
        }
    }

    fn assert_settled_once(app: fn() -> Element, counter: &'static AtomicUsize, shape: &str) {
        match mount_and_settle(app, counter) {
            Ok(1) => {}
            Ok(n) => panic!(
                "{shape}: the effect ran {n} times for a single mount — the guard is \
                 subscribing the effect to the generation signal it writes"
            ),
            Err(()) => panic!(
                "{shape}: mounting never settled in 5s — the effect is re-triggering \
                 itself. This is the flicker-and-unclickable regression from v0.3.31; \
                 `begin`/`is_current` must use `peek`, not `read`."
            ),
        }
    }

    // ── Shape 1: claim a token in an effect ─────────────────────────────
    // What `rbac_panel`, `app_settings_panel`, `home_panel` and the chain
    // discovery in `main_screen` all do on mount.

    static CLAIM_RUNS: AtomicUsize = AtomicUsize::new(0);

    fn claims_in_effect() -> Element {
        let mut guard = use_fetch_guard();
        use_effect(move || {
            CLAIM_RUNS.fetch_add(1, Ordering::SeqCst);
            let _token = guard.begin();
        });
        rsx! { div {} }
    }

    #[test]
    fn claiming_a_token_does_not_retrigger_the_effect_that_claimed_it() {
        assert_settled_once(claims_in_effect, &CLAIM_RUNS, "begin in an effect");
    }

    // ── Shape 2: claim, then check before publishing ────────────────────
    // The full round trip. `is_current` reads the same signal, so it needs
    // the same treatment — a fix applied to `begin` alone would still loop.

    static CHECK_RUNS: AtomicUsize = AtomicUsize::new(0);

    fn claims_and_checks_in_effect() -> Element {
        let mut guard = use_fetch_guard();
        use_effect(move || {
            CHECK_RUNS.fetch_add(1, Ordering::SeqCst);
            let token = guard.begin();
            // Stands in for the load resolving and asking whether to publish.
            let _still_wanted = guard.is_current(token);
        });
        rsx! { div {} }
    }

    #[test]
    fn checking_a_token_does_not_retrigger_the_effect_either() {
        assert_settled_once(
            claims_and_checks_in_effect,
            &CHECK_RUNS,
            "begin + is_current in an effect",
        );
    }

    // ── Shape 3: two guards in one component ────────────────────────────
    // `functions_panel` holds a discovery guard and a metrics guard; a
    // per-guard signal must not make the other guard's scope rerun.

    static PAIR_RUNS: AtomicUsize = AtomicUsize::new(0);

    fn two_guards_in_one_component() -> Element {
        let mut first = use_fetch_guard();
        let mut second = use_fetch_guard();
        use_effect(move || {
            PAIR_RUNS.fetch_add(1, Ordering::SeqCst);
            let a = first.begin();
            let b = second.begin();
            let _ = first.is_current(a) && second.is_current(b);
        });
        rsx! { div {} }
    }

    #[test]
    fn two_guards_in_one_component_still_settle() {
        assert_settled_once(
            two_guards_in_one_component,
            &PAIR_RUNS,
            "two guards in one component",
        );
    }

    // ── Shape 4: a guard used from a spawned task ───────────────────────
    // `observability_panel`'s log tail claims its token inside `spawn`, and
    // the guarded panels all call `is_current` from one.
    //
    // Read the assertion here for exactly what it is: a smoke test. It was
    // written expecting to catch the same bug and it does **not** — it passes
    // against the `read` version too, because a reactive read inside a
    // spawned task does not re-run the effect that spawned it the way a
    // synchronous read in the effect body does. It is kept because it pins
    // that difference and proves the task really executes, but the tests
    // above are the ones standing between this crate and the v0.3.31
    // regression. Do not treat this one as covering it.

    static SPAWNED_RUNS: AtomicUsize = AtomicUsize::new(0);
    static TASK_BODY_RUNS: AtomicUsize = AtomicUsize::new(0);

    fn claims_inside_a_spawned_task() -> Element {
        let mut guard = use_fetch_guard();
        use_effect(move || {
            SPAWNED_RUNS.fetch_add(1, Ordering::SeqCst);
            spawn(async move {
                TASK_BODY_RUNS.fetch_add(1, Ordering::SeqCst);
                let token = guard.begin();
                let _still_wanted = guard.is_current(token);
            });
        });
        rsx! { div {} }
    }

    #[test]
    fn a_guard_used_inside_a_spawned_task_settles() {
        TASK_BODY_RUNS.store(0, Ordering::SeqCst);
        assert_settled_once(
            claims_inside_a_spawned_task,
            &SPAWNED_RUNS,
            "begin inside a spawned task",
        );
        assert!(
            TASK_BODY_RUNS.load(Ordering::SeqCst) > 0,
            "the spawned task never ran, so this test asserts nothing at all"
        );
    }

    // ── The guard still does its job under a real mount ─────────────────
    // Settling is necessary but not sufficient: a `begin` that did nothing at
    // all would also settle. This pins the behaviour the guard exists for.

    static SUPERSEDED: AtomicUsize = AtomicUsize::new(0);
    static PUBLISHED: AtomicUsize = AtomicUsize::new(0);

    fn two_loads_race() -> Element {
        let mut guard = use_fetch_guard();
        use_effect(move || {
            SUPERSEDED.fetch_add(1, Ordering::SeqCst);
            // Two loads start; the first is overtaken before either resolves.
            let first = guard.begin();
            let second = guard.begin();
            // They resolve in the wrong order — the stale one last.
            for token in [second, first] {
                if guard.is_current(token) {
                    PUBLISHED.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        rsx! { div {} }
    }

    #[test]
    fn only_the_newest_load_publishes_under_a_real_mount() {
        PUBLISHED.store(0, Ordering::SeqCst);
        assert_settled_once(two_loads_race, &SUPERSEDED, "two racing loads");
        assert_eq!(
            PUBLISHED.load(Ordering::SeqCst),
            1,
            "exactly one of the two loads should have published"
        );
    }
}
