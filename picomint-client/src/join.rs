//! Joining a federation.
//!
//! One path, whether or not the seed has been here before. [`join`] downloads
//! the config and scans every account the seed could hold notes under; a seed
//! that never held anything scans to nothing, which costs a round trip and is
//! otherwise indistinguishable from a first join. There is nothing for an
//! integrator to choose between, and so nothing for it to get wrong: picking
//! "add" for a federation this seed has used strands every note behind the
//! counters it would re-derive from zero.

use std::collections::BTreeMap;

use anyhow::bail;
use futures::future::try_join_all;
use iroh::Endpoint;
use picomint_core::config::ConsensusConfig;
use picomint_core::core::Account;
use picomint_core::invite::InviteCode;
use picomint_core::methods::{ConfigRequest, ConfigResponse, CoreMethod};
use picomint_core::module::Method;
use picomint_sqlite::WriteTx;
use tracing::debug;

use crate::api::FederationApi;
use crate::mint::Restore;
use crate::secret::{ClientSecret, Mnemonic};

/// A federation ready to be joined: its config, and what a scan of the seed
/// found in every account.
///
/// Holds no database handle and has written nothing. [`Join::commit`] is the
/// whole of what it lands, in a dbtx the caller owns.
pub struct Join {
    config: ConsensusConfig,
    restores: BTreeMap<Account, Restore>,
}

impl Join {
    /// The federation's config, verified against the invite code it came
    /// from. Persist it — [`crate::Client::new`] takes it on every later
    /// startup, when there is no join to produce it.
    pub fn config(&self) -> &ConsensusConfig {
        &self.config
    }

    /// Land the join: every account's counter mark and the notes its scan
    /// found. The whole of what joining writes, so a [`crate::Client`] built
    /// against this database afterwards opens on the restored balance.
    ///
    /// Belongs in the same dbtx that marks the federation as joined, so a
    /// crash leaves either both or neither. A wallet joined without these
    /// marks resumes from counter zero and re-derives nonces the federation
    /// has already signed, stranding every note behind them — which is why
    /// this is not a step an integrator can be handed and forget.
    pub fn commit(&self, dbtx: &WriteTx) {
        for (account, restore) in &self.restores {
            crate::mint::commit_scan(dbtx, *account, restore);
        }
    }
}

/// Download a federation's config and rebuild whatever the seed already owns
/// there.
///
/// Reads nothing and writes nothing locally, so a failure leaves the wallet
/// exactly as it was. All or nothing across the accounts: one scan failing
/// abandons the join rather than leaving some accounts restored and others
/// silently starting from zero.
///
/// The scans walk disjoint counter spaces and share nothing, so they run
/// concurrently and the wait is the slowest of them rather than their sum.
pub async fn join(
    endpoint: &Endpoint,
    mnemonic: &Mnemonic,
    invite: &InviteCode,
) -> anyhow::Result<Join> {
    let config = download(endpoint, invite).await?;

    let federation = config.calculate_federation_id();

    let peer_node_ids = config
        .peers
        .iter()
        .map(|(peer, endpoint)| (*peer, endpoint.iroh_pk))
        .collect();

    let api = FederationApi::new(endpoint.clone(), peer_node_ids);

    let secret = ClientSecret::new(mnemonic, federation).mint_secret();

    let scans = try_join_all(
        Account::ALL
            .map(|account| crate::mint::scan(&api, &secret, &config.mint, federation, account)),
    )
    .await?;

    let restores = Account::ALL.into_iter().zip(scans).collect();

    Ok(Join { config, restores })
}

/// Downloads the [`ConsensusConfig`] from the issuing guardian named in the
/// invite code. The guardian enforces the invite's expiration and user limit
/// before serving; integrity is guaranteed because the config's computed
/// federation id must match the one committed in the invite code.
async fn download(endpoint: &Endpoint, invite: &InviteCode) -> anyhow::Result<ConsensusConfig> {
    debug!(
        invite = %picomint_base32::encode(invite),
        node_id = %invite.node_id,
        "Downloading client config via invite code"
    );

    let invite_resp: ConfigResponse = picomint_rpc::request(
        endpoint,
        invite.node_id,
        Method::Core(CoreMethod::Config(ConfigRequest {
            invite_id: invite.invite_id,
        })),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Failed to download client config from invite peer"))?;

    if invite_resp.config.calculate_federation_id() != invite.federation {
        bail!("FederationId in invite code does not match client config");
    }

    Ok(invite_resp.config)
}
