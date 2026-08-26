//! Paying out the cut a client has collected.
//!
//! The account a cut accrues in is an ordinary one, so collecting it is an
//! ordinary Lightning send funded from that account — no privileged path, and
//! nothing that has to succeed. A sweep that finds no gateway, cannot reach
//! the payout endpoint, or loses the payment simply leaves the balance where
//! it is for the next pass. Spending the account is also the one thing
//! picomint never charges a cut on, so a sweep does not pay itself a fee it
//! would then have to sweep.

use std::sync::Arc;
use std::time::Duration;

use picomint_core::Amount;
use picomint_core::config::FederationId;
use picomint_core::core::Account;
use picomint_core::fee::FeeConfig;
use thiserror::Error;
use tokio::time::sleep;
use tracing::warn;

use crate::Client;

client_table!(
    /// The federation's announced fee, as this client last read it back
    /// under threshold consensus. Absent until a refresh finds one, which is
    /// also how a withdrawn announcement is recorded.
    OperatorFeeTable,
    () => FeeConfig,
    "operator-fee",
);

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum RefreshOperatorFeeError {
    #[error("Failed to request the federation fee")]
    FailedToRequestOperatorFee,
}

impl Client {
    /// Read the cached federation fee. Populated by
    /// [`Self::refresh_operator_fee`] (run once at startup); returns `None`
    /// until that completes successfully or if the federation announces no
    /// fee.
    pub fn operator_fee(&self) -> Option<FeeConfig> {
        self.db()
            .begin_read()
            .get(&OperatorFeeTable(self.federation()), &())
    }

    /// Re-fetch the announced fee via threshold consensus and reconcile the
    /// local cache. Inserts on `Some(_)`, removes on `None`.
    pub async fn refresh_operator_fee(client: Arc<Self>) -> Result<(), RefreshOperatorFeeError> {
        let fee = client
            .api()
            .fee_config()
            .await
            .map_err(|_| RefreshOperatorFeeError::FailedToRequestOperatorFee)?;

        let dbtx = client.db().begin_write();

        match fee {
            Some(fee) => {
                dbtx.insert(&OperatorFeeTable(client.federation()), &(), &fee);
            }
            None => {
                dbtx.remove(&OperatorFeeTable(client.federation()), &());
            }
        }

        dbtx.commit();

        Ok(())
    }
}

/// Drop the announcement cache. Called by [`Client::wipe`].
pub(crate) fn wipe_tables(dbtx: &picomint_redb::WriteTx, federation: FederationId) {
    dbtx.delete_table(&OperatorFeeTable(federation));
}

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

/// Sweep `account` to `lnurl` forever, one pass at a time.
///
/// Passes are sequential, so a slow payment delays the next pass rather than
/// racing it — which is what keeps two sweeps from both deciding the same
/// balance is theirs to send.
pub(crate) async fn sweep(client: Arc<Client>, account: Account, lnurl: String) {
    loop {
        sleep(SWEEP_INTERVAL).await;

        if let Err(error) = sweep_once(&client, account, &lnurl).await {
            warn!(%account, %error, "Fee sweep did not go through");
        }
    }
}

/// Sweep the federation's cut to wherever its announcement says, forever.
///
/// The destination comes from the cache [`Client::refresh_operator_fee`]
/// fills, so a pass before that lands, or after the federation withdraws its
/// announcement, has nowhere to send and does nothing.
pub(crate) async fn sweep_operator_fee(client: Arc<Client>) {
    loop {
        sleep(SWEEP_INTERVAL).await;

        let Some(fee) = client.operator_fee() else {
            continue;
        };

        if let Err(error) = sweep_once(&client, Account::OperatorFee, &fee.lnurl).await {
            warn!(%error, "Federation fee sweep did not go through");
        }
    }
}

/// Send half of what `account` holds, if it holds enough to bother.
///
/// Half rather than all of it because the payment is funded from the same
/// account: the gateway's cut and the federation's fees come out of the
/// balance too, and what stays behind covers them with room to spare. The
/// remainder is swept by a later pass, so a large balance drains over
/// several of them rather than needing the fee arithmetic done up front.
async fn sweep_once(client: &Client, account: Account, lnurl: &str) -> Result<(), String> {
    let balance = client.get_balance(account);

    if balance.msat < SWEEP_THRESHOLD.msat {
        return Ok(());
    }

    let url = picomint_lnurl::parse_lnurl(lnurl).ok_or("Payout lnurl is not an lnurl")?;

    let info = picomint_lnurl::request(&url).await?;

    let invoice = picomint_lnurl::get_invoice(&info, balance.msat / 2)
        .await?
        .pr;

    // Selected against the invoice rather than blind: when the payout
    // endpoint is served by a gateway this federation already knows, the
    // payment becomes a direct ecash swap and carries no routing fee at all.
    let (gateway_pk, gateway_info) = client
        .ln()
        .select_gateway(Some(&invoice))
        .map_err(|e| e.to_string())?;

    client
        .ln()
        .send(account, gateway_pk, gateway_info, invoice)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}
