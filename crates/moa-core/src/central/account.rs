//! When moa's account fails (K8, C24): a turn that failed, a new thread that couldn't open
//! or a start that failed, for a reason of the account's (its login, its billing, a spent
//! usage limit), writes a `card` that says so and offers `/account B` for each other account
//! moa can run on. Nothing else changes: the next prompt is on the same account, and only
//! your `/account` switches it.

use super::{card, launch, warn};
use crate::Now;
use crate::change::Change;
use crate::config::AccountName;
use crate::core::{Tx, post_commit};
use crate::effect::{Effect, Publish};
use crate::publish::CoreEvent;
use crate::state::State;
use crate::text::cut;

/// What says the account is the trouble, in lowercase: its login, its billing, a spent limit.
pub const ACCOUNT_TROUBLE: [&str; 9] = [
    "authentication required",
    "not logged in",
    "/login",
    "invalid api key",
    "oauth token",
    "credit balance",
    "billing",
    "usage limit",
    "limit reached",
];

/// Whether `why` moa's agent failed is its account's.
pub fn trouble(why: &str) -> bool {
    let why = why.to_lowercase();
    ACCOUNT_TROUBLE.iter().any(|p| why.contains(p))
}

/// The `what` of the account card.
pub const WHAT: &str = "account";

/// The most of why moa's agent failed that the card quotes, in characters.
const WHY_MAX: usize = 200;

/// moa's agent failed on its run's account, as `why` says. When the account is the trouble,
/// and its card wasn't said since a turn last ended well or the account was switched, the
/// card goes in the open thread, else the newest saved thread of any account (F18). With no
/// thread at all, or its commit refused, its text goes out as an error. The log names the
/// account, never why.
pub(super) fn failed(state: &mut State, now: &Now, why: &str) -> Vec<Effect> {
    let moa = &mut state.central;
    let Some(account) = moa.account.clone() else {
        return Vec::new();
    };
    if !trouble(why) || moa.account_card.as_ref() == Some(&account) {
        return Vec::new();
    }
    moa.account_card = Some(account.clone());
    let text = card_text(state, &account, why);
    let instead = match thread(state) {
        None => "no thread holds its card",
        Some(thread) => {
            let mut tx = Tx::new(now);
            let row = card(state, &mut tx, &thread, WHAT, &text);
            if tx.commit(state, vec![Change::Feed(row)]).is_ok() {
                let mut fx = vec![warn(format!(
                    "moa's account {account} failed; a card says so"
                ))];
                fx.extend(post_commit(state, tx));
                return fx;
            }
            "its card couldn't be saved"
        }
    };
    vec![
        warn(format!(
            "moa's account {account} failed; {instead}, so it is said as an error"
        )),
        Effect::Publish(Publish::Event(CoreEvent::Error { message: text })),
    ]
}

/// The thread the card goes in: the open one when it is saved, else the newest saved.
fn thread(state: &State) -> Option<String> {
    let open = state.central.thread.as_ref().map(|o| &o.row.thread_id);
    let newest = || {
        let newest = state
            .threads()
            .max_by(|a, b| (a.started_at, &a.thread_id).cmp(&(b.started_at, &b.thread_id)));
        newest.map(|t| &t.thread_id)
    };
    open.filter(|id| state.thread(id).is_some())
        .or_else(newest)
        .cloned()
}

/// K11: `moa's account <A> failed: <why>.`, then the other accounts moa can run on, each a
/// `/account B` button, or that there is none.
fn card_text(state: &State, account: &AccountName, why: &str) -> String {
    let why = cut(why.lines().next().unwrap_or("").trim(), WHY_MAX);
    // A why that ends its sentence isn't ended twice.
    let why = why.trim_end().trim_end_matches('.');
    let others: Vec<String> = launch::runnable(state)
        .into_iter()
        .filter(|a| a != account)
        .map(|a| format!("`/account {a}`"))
        .collect();
    let rest = match others.is_empty() {
        true => "Nothing was switched, and no other account is set up for moa in moa.toml.".into(),
        false => format!(
            "Nothing was switched. To go on with another account:\n\n{}",
            others.join(" ")
        ),
    };
    format!("moa's account {account} failed: {why}.\n\n{rest}")
}
