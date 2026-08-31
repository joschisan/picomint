//! Paying out the cut a client has collected.
//!
//! The account a cut accrues in is an ordinary one, so collecting it is a
//! Lightning max send funded from that account — no privileged path, and
//! nothing that has to succeed. A sweep that finds no gateway, cannot reach
//! the payout endpoint, or loses the payment simply leaves the balance where
//! it is for the next pass. Spending the account is also the one thing
//! picomint never charges a cut on, so a sweep does not pay itself a fee it
//! would then have to sweep.

use std::time::Duration;

use picomint_core::Amount;
use picomint_core::core::Account;
use tokio::time::sleep;
use tracing::warn;

use crate::module::ClientContext;

/// How long between passes, and how long the first one waits.
///
/// A pass costs a local database read and nothing else until there is enough
/// to send, so this is chosen to make a cut leave promptly rather than to
/// keep the cost down.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Balance a fee account has to reach before any of it is swept.
///
/// A Lightning payment costs a gateway's base fee whatever its size, so
/// sweeping the moment anything arrives would spend the cut on collecting it.
const SWEEP_THRESHOLD: Amount = Amount::from_sat(1000);

/// Sweep `account` to `lnurl` forever, one pass at a time. A pass over the
/// threshold is one [`crate::ln::send_max`], which empties the
/// account.
///
/// Passes are sequential, so a slow payment delays the next pass rather than
/// racing it — which is what keeps two sweeps from both deciding the same
/// balance is theirs to send.
pub(crate) async fn sweep(ctx: ClientContext, account: Account, lnurl: String) {
    loop {
        sleep(SWEEP_INTERVAL).await;

        let balance = crate::mint::balance(&ctx.db.begin_read(), ctx.federation(), account);

        if balance.msat < SWEEP_THRESHOLD.msat {
            continue;
        }

        if let Err(error) = sweep_once(&ctx, account, &lnurl).await {
            warn!(%account, %error, "Fee sweep did not go through");
        }
    }
}

async fn sweep_once(ctx: &ClientContext, account: Account, lnurl: &str) -> anyhow::Result<()> {
    let (gateway_pk, gateway_info) = crate::ln::select_gateway(ctx)?;

    crate::ln::send_max(ctx, account, gateway_pk, gateway_info, lnurl)
        .await
        .map(|_| ())
}
