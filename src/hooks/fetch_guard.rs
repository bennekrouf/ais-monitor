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
    pub fn begin(&mut self) -> u64 {
        let next = *self.generation.read() + 1;
        self.generation.set(next);
        next
    }

    /// Whether `token`'s load is still the one whose result the panel wants.
    pub fn is_current(&self, token: u64) -> bool {
        *self.generation.read() == token
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
