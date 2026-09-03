use maud::{Markup, html};

use crate::bitcoind::BitcoindRpcStatus;
use crate::ui::dashboard::kv;

pub fn render(status: &Option<BitcoindRpcStatus>) -> Markup {
    html! {
        div class="card" {
            div class="card-header" {
                span class="card-title" { "Bitcoin Connection" }
            }
            @if let Some(status) = status {
                (kv("Network", html! { (format!("{:?}", status.network)) }))
                (kv("Block Count", html! { (status.block_count) }))

                @if let Some(fee_rate) = status.fee_rate {
                    (kv("Fee Rate", html! { (fee_rate.sat_per_kvb / 1000) " sat/vB" }))
                }

                @if let Some(sync) = status.sync_progress {
                    (kv("Sync Progress", html! { (format!("{:.1}%", sync * 100.0)) }))

                    @if sync < 0.999 {
                        div class="card-section" {
                            div class="alert alert-warning" {
                                "The bitcoin backend is not fully synced yet. We need to wait for it to sync before we can participate in consensus."
                            }
                        }
                    }
                }
            } @else {
                div class="card-section" {
                    div class="alert alert-danger" {
                        "Failed to connect to bitcoin backend. Please establish a connection in order to participate in consensus."
                    }
                }
            }
        }
    }
}
