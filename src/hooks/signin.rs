//! Signing in to Azure, and waiting for it to actually happen.
//!
//! `az login` opens a browser and returns immediately, so the only way to know
//! it worked is to ask again. Every sign-in button needs that, and every one
//! used to implement it separately — which is how four of them ended up not
//! implementing it at all: two cleared their error and changed nothing, one
//! set "Checking…" and never resolved it, and one reloaded its data straight
//! away, racing the browser it had just opened.
//!
//! The shape of this API is deliberate, because the first version of it
//! repeated those mistakes in a new form:
//!
//!   * it returned a `Result` that four of five callers discarded, so a
//!     missing `az` produced silence — so failures are reported here instead;
//!   * it left "am I waiting?" to each caller, so three buttons sat inert for
//!     two minutes — so the busy flag is a required argument, not an optional
//!     courtesy;
//!   * it called back on every poll, so callers all wrote the same
//!     `if matches!(state, LoggedIn)` — so it calls back once, on success.
//!
//! It lives under `hooks` rather than `services` because waiting means driving
//! a signal: `services` stays pure and testable without a renderer, and this is
//! the other kind of thing.

use dioxus::prelude::*;

use crate::services::activity;
use crate::services::azure::{self, AzLoginState};

/// How long to keep asking. Long enough for a browser sign-in with MFA, short
/// enough that an abandoned one stops.
const ATTEMPTS: usize = 24;
const INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Opens `az login`, shows that it is waiting, and runs `on_signed_in` once
/// the session is actually live.
///
/// `busy` is held true for the whole wait, so a caller cannot forget to show
/// one — the button it belongs to has something to disable or label. It is
/// cleared on every exit: success, timeout, or a failure to start `az`.
///
/// Nothing is returned. A failure to start the CLI is logged to the activity
/// feed and surfaced through `busy` going false, because a `Result` here was
/// ignored by almost everyone who called it.
pub fn sign_in_and_wait<F>(tenant: &str, mut busy: Signal<bool>, on_signed_in: F)
where
    F: FnOnce(AzLoginState) + 'static,
{
    let tenant = tenant.trim().to_string();
    let arg = (!tenant.is_empty()).then(|| tenant.clone());

    if let Err(e) = azure::open_login(arg.as_deref()) {
        activity::error("az login could not be started", tenant, e);
        busy.set(false);
        return;
    }

    activity::info(
        "Opened az login",
        if tenant.is_empty() {
            "default tenant".to_string()
        } else {
            tenant
        },
    );
    busy.set(true);

    spawn(async move {
        for _ in 0..ATTEMPTS {
            tokio::time::sleep(INTERVAL).await;
            let state = tokio::task::spawn_blocking(azure::check_login)
                .await
                .unwrap_or(AzLoginState::NotLoggedIn);

            if let AzLoginState::LoggedIn { ref account, .. } = state {
                activity::ok("Logged in to Azure", account.clone());
                busy.set(false);
                // The state goes with it: the welcome screen needs the account
                // and subscription, everyone else ignores the argument.
                on_signed_in(state);
                return;
            }
        }
        // Ran out of patience rather than succeeded — say so, or the button
        // just quietly comes back to life with nothing having changed.
        activity::error(
            "Sign-in not detected",
            "",
            "Gave up waiting for `az login` to complete.",
        );
        busy.set(false);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_is_long_enough_for_a_browser_sign_in() {
        // Two minutes. Shorter and an MFA prompt outlives the poll, which is
        // the failure this module exists to prevent.
        assert!(ATTEMPTS as u64 * INTERVAL.as_secs() >= 120);
    }

    #[test]
    fn the_wait_is_short_enough_to_give_up_on() {
        // An abandoned sign-in must not leave a button disabled all afternoon.
        assert!(ATTEMPTS as u64 * INTERVAL.as_secs() <= 180);
    }
}
