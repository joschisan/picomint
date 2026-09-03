use maud::{Markup, html};
use picomint_redb::ReadTx;

use crate::consensus::db::consensus_block_count;
use crate::consensus::wallet;
use crate::ui::dashboard::kv;

/// Renders the pending transaction chain as its own card; nothing when the
/// chain is empty.
pub fn render_pending(server: &crate::consensus::server::Server, dbtx: &ReadTx) -> Markup {
    let consensus_block_count = consensus_block_count(server, dbtx);
    let pending_tx_chain = wallet::pending_tx_chain(dbtx);

    if pending_tx_chain.is_empty() {
        return html! {};
    }

    let total_pending_vbytes = pending_tx_chain.iter().map(|info| info.vbytes).sum::<u64>();

    let total_pending_fee = pending_tx_chain
        .iter()
        .map(|info| info.fee.to_sat())
        .sum::<u64>();

    let stale_chain = pending_tx_chain
        .last()
        .is_some_and(|tx| consensus_block_count > tx.created + 18);

    html! {
        div class="card" {
            div class="card-header" {
                span class="card-title" { "Pending Transactions" }
            }

            @if stale_chain {
                div class="card-section" {
                    div class="alert alert-danger" {
                        "Warning: Transaction has been pending for more than 18 blocks!"
                    }
                }
            }

            table {
                thead {
                    tr {
                        th { "Index" }
                        th { "Value in Custody" }
                        th { "Fee" }
                        th { "vBytes" }
                        th { "Feerate" }
                        th { "Age" }
                        th style="text-align: right" { "Transaction" }
                    }
                }
                tbody {
                    @for tx in pending_tx_chain {
                        tr {
                            td class="mono muted" { (tx.index) }
                            td class="mono" {
                                @if tx.output >= tx.input {
                                    span class="pos" { "+" (tx.output - tx.input) }
                                } @else {
                                    span class="neg" { "-" (tx.input - tx.output) }
                                }
                            }
                            td class="mono" { (tx.fee.to_sat()) }
                            td class="mono" { (tx.vbytes) }
                            td class="mono" { (tx.feerate()) }
                            td class="mono" { (consensus_block_count.saturating_sub(tx.created)) }
                            td style="text-align: right" {
                                a href={ "https://mempool.space/tx/" (tx.txid) } target="_blank" {
                                    "mempool.space"
                                }
                            }
                        }
                    }
                }
            }

            div class="summary-row" {
                span class="summary-row-label" { "Total feerate of pending chain" }
                span class="summary-row-value" { (total_pending_fee / total_pending_vbytes) " sat/vbyte" }
            }
        }
    }
}

pub fn render(server: &crate::consensus::server::Server, dbtx: &ReadTx) -> Markup {
    let federation_wallet = wallet::federation_wallet(dbtx);
    let consensus_fee_rate = wallet::consensus_feerate(server, dbtx).map(|f| f / 1000);
    let transaction_count = wallet::total_txs(dbtx);

    html! {
        div class="card" {
            div class="card-header" {
                span class="card-title" { "Wallet" }
            }

            (kv("Network", html! { (server.cfg.consensus.network) }))

            @if let Some(wallet) = federation_wallet {
                div class="kv" {
                    span class="kv-label" { "Transaction Tip" }
                    a href={ "https://mempool.space/tx/" (wallet.outpoint.txid) } target="_blank" {
                        "mempool.space"
                    }
                }
                (kv("Transaction Count", html! { (transaction_count) }))
            }

            (kv("Fee Rate", html! {
                @if let Some(fee_rate) = consensus_fee_rate {
                    (fee_rate) " sat/vbyte"
                } @else {
                    "No fee rate available"
                }
            }))
        }
    }
}
