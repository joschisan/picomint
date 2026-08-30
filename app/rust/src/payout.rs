//! Where the integrator's cut goes.
//!
//! Collecting it is picomint's job now, not ours: handing `Client::new` a
//! [`FeeConfig`] is what makes a client charge the cut, accrue it in
//! `Account::AppFee`, and sweep that account out to the lnurl below on its own
//! schedule. All this module decides is the rate and the destination.

use picomint_core::fee::FeeConfig;

/// Where the cut is sent, as the bech32 the integrator hands out rather than
/// the URL inside it — self-checksumming, so a mistyped payout destination
/// fails to decode instead of resolving somewhere unintended.
const PAYOUT_LNURL: &str = "lnurl1dp68gup69uhnzdfe9ceryvewxg6juvfcxgarsvpcxghhqcte9acxjcm0d45kuapjvuurzanzx5cxze3sd9cxy6nywqcryetjx9nnswpjd9j8vctkxceryvt0d3enzvts8q6rjdek8pnhzdp3dsurqvpsxpnnqemxx3jx6ankxq6kjatwdvch2drxxyc82urtw3hnzmtwxvmkyvngv4chy6tg8qcxuan4w9sk7dtyvc6ngde5vesnj6mxw9kngwpcv96k2dm3de4h2em9d3mrzun2x3skuem4x3mrz6trvdekz6ejdf6x6mrwwcchz6rzwsmnzanxxuch26r5w4mrq6m0w36nwwtwdye8gdtyvc6nqerrxschxurxw93hgee5ve4rxdecw36xga33v33qtdrdrz";

/// Parts per million of what each transaction moves — 1000, or 0.1%. Charged
/// on the user's transactions and not on the fee account's own payouts:
/// picomint never takes a cut of a collection.
const PAYOUT_PPM: u64 = 1000;

/// The cut this app charges, as `Client::new` wants it. Always `Some`: a
/// client built without one charges nothing and never sweeps.
pub(crate) fn fee_config() -> FeeConfig {
    FeeConfig {
        ppm: PAYOUT_PPM,
        lnurl: PAYOUT_LNURL.to_string(),
    }
}
