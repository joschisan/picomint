//! Adding a mint.
//!
//! One path, whether or not the seed has been here before. [`add`] downloads
//! the config and scans every account the seed could hold notes under; a seed
//! that never held anything scans to nothing, which costs a round trip and is
//! otherwise indistinguishable from a first add. There is nothing for an
//! integrator to choose between, and so nothing for it to get wrong: skipping
//! the scan for a mint this seed has used strands every note behind the
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
use tracing::debug;

use crate::api::MintApi;
use crate::client::Client;
use crate::ecash::Restore;
use crate::secret::ClientSecret;

/// Download a mint's config, check it against `network` if given, and
/// rebuild whatever the seed already owns there.
///
/// Reads nothing and writes nothing locally, so a failure leaves the wallet
/// exactly as it was. The network check runs before the scans — a rejected
/// mint costs one config download. All or nothing across the accounts:
/// one scan failing abandons the add rather than leaving some accounts
/// restored and others silently starting from zero.
///
/// The scans walk disjoint counter spaces and share nothing, so they run
/// concurrently and the wait is the slowest of them rather than their sum.
pub(crate) async fn add_mint(
    client: &Client,
    invite: &InviteCode,
    network: Option<bitcoin::Network>,
) -> anyhow::Result<(ConsensusConfig, BTreeMap<Account, Restore>)> {
    let config = download(&client.endpoint, invite).await?;

    if network.is_some_and(|network| config.network != network) {
        bail!("Unsupported network {}", config.network);
    }

    let mint = config.calculate_mint_id();

    let api = MintApi::new(client.endpoint.clone(), config.iroh_pks());

    let secret = ClientSecret::new(&client.mnemonic, mint).ecash_secret();

    let scans = try_join_all(
        Account::ALL
            .map(|account| crate::ecash::scan(&api, &secret, &config.ecash, mint, account)),
    )
    .await?;

    let restores = Account::ALL.into_iter().zip(scans).collect();

    Ok((config, restores))
}

/// Downloads the [`ConsensusConfig`] from the issuing guardian named in the
/// invite code. The guardian enforces the invite's expiration and user limit
/// before serving; integrity is guaranteed because the config's computed
/// mint id must match the one committed in the invite code.
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

    if invite_resp.config.calculate_mint_id() != invite.mint {
        bail!("MintId in invite code does not match client config");
    }

    Ok(invite_resp.config)
}
