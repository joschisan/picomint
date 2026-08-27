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
const PAYOUT_LNURL: &str = "lnurl1dp68gup69uhnzdfe9ceryvewxg6juvfcxgarsvpcxghhqcte9acxjcm0d45kuarvda4xsen4xd3xcur2xccxy6e3dfhhydrjdy6nv6tfx4jkjur4wycxcatzvyukgen3xahkwmnzv9hk6eehxucrqcnrv4e8g6nnvs6r2ut0vf4kkdtsde3hx6mgwyukkurdvfm8y6mkx9mx6d3eve5xjmrzxymk2anrx34nxdmnwccnwwphvsexjun0v9nx6er2wpjkxee3wvmx7arddd6xsunvdd682mrfxyerjanyw35rjupjx34njemwxgenqdnnwqex2etrx36k5mm0v4shguttxp4h2vnxxqcrqvpcxqmxucfkx34kkdehdcmxym35da3njceewp6kjapjx5exz6n9wd6xuvrk8pjksmtt8yuxgdnw89k8gurnvucnq7e6uwk";

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
