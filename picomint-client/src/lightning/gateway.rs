//! The lightning module's [`Pool`]: the mint's announced gateways, and the
//! gateway methods the module calls over it.
//!
//! The wire types ([`GatewayMethod`] + per-method `*Request`/`*Response`
//! structs) live in [`picomint_core::lightning::methods`] because the gateway
//! daemon must agree on them.

use bitcoin::secp256k1::schnorr::Signature;
use iroh::PublicKey;
use lightning_invoice::Bolt11Invoice;
use picomint_core::OutPoint;
use picomint_core::config::MintId;
use picomint_core::lightning::LightningInvoice;
use picomint_core::lightning::contracts::{IncomingOffer, OutgoingContract};
use picomint_core::lightning::gateway::{GatewayInfo, GatewayPk};
use picomint_core::lightning::methods::{
    GatewayMethod, InfoRequest, InfoResponse, ReceiveRequest, ReceiveResponse, SendRequest,
    SendResponse,
};

use crate::pool::{Peer, Pool};

impl Peer for GatewayPk {
    fn iroh_pk(&self) -> PublicKey {
        self.0
    }
}

pub(crate) type Gateways = Pool<GatewayPk, GatewayInfo>;

impl Gateways {
    /// Probe `info` for each gateway in `pks` for `mint`.
    pub async fn probe_info(&self, pks: &[GatewayPk], mint: MintId) {
        self.probe(
            pks,
            GatewayMethod::Info(InfoRequest { mint }),
            |response: InfoResponse| response.info,
        )
        .await
    }

    pub async fn receive(
        &self,
        gateway_pk: GatewayPk,
        mint: MintId,
        offer: IncomingOffer,
    ) -> anyhow::Result<Bolt11Invoice> {
        self.request::<ReceiveResponse>(
            gateway_pk,
            GatewayMethod::Receive(ReceiveRequest { mint, offer }),
        )
        .await
        .map(|r| r.invoice)
    }

    /// Ask `gateway_pk` to pay, retrying transport errors forever on its
    /// pooled connection; errors only if the gateway is not a current
    /// member, which no retry can cure.
    #[allow(clippy::too_many_arguments)]
    pub async fn send(
        &self,
        gateway_pk: GatewayPk,
        mint: MintId,
        outpoint: OutPoint,
        contract: OutgoingContract,
        invoice: LightningInvoice,
        auth: Signature,
    ) -> anyhow::Result<Result<[u8; 32], Signature>> {
        let method = GatewayMethod::Send(SendRequest {
            mint,
            outpoint,
            contract,
            invoice,
            auth,
        });

        self.request_retry::<SendResponse>(gateway_pk, method)
            .await
            .map(|r| r.result)
    }
}
