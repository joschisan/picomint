use maud::{Markup, html};

use crate::consensus::wallet;
use crate::ui::copiable_text;

pub async fn render(server: &crate::consensus::server::Server) -> Markup {
    let network = server.cfg.consensus.network;
    let federation_wallet = wallet::federation_wallet_ui(server);
    let consensus_block_count = wallet::consensus_block_count_ui(server);
    let consensus_fee_rate = wallet::consensus_feerate_ui(server);
    let send_fee = wallet::send_fee_ui(server);
    let receive_fee = wallet::receive_fee_ui(server);
    let pending_tx_chain = wallet::pending_tx_chain_ui(server);
    let transaction_count = wallet::transaction_count_ui(server);
    let restore_keys = wallet::restore_keys_ui(server);

    let total_pending_vbytes = pending_tx_chain.iter().map(|info| info.vbytes).sum::<u64>();

    let total_pending_fee = pending_tx_chain
        .iter()
        .map(|info| info.fee.to_sat())
        .sum::<u64>();

    let stale_chain = pending_tx_chain
        .last()
        .is_some_and(|tx| consensus_block_count > tx.created + 18);

    html! {
        div class="card h-100" {
            div class="card-header dashboard-header" { "Wallet" }
            div class="card-body" {
                div class="mb-4" {
                    table class="table" {
                        tr {
                            th { "Network" }
                            td { (network) }
                        }
                        @if let Some(wallet) = federation_wallet {
                            tr {
                                th { "Value in Custody" }
                                td { (format!("{:.8} BTC", wallet.value.to_btc())) }
                            }
                            tr {
                                th { "Transaction Chain Tip" }
                                td {
                                    a href={ "https://mempool.space/tx/" (wallet.outpoint.txid) } class="btn btn-sm btn-outline-primary" target="_blank" {
                                        "mempool.space"
                                    }
                                }
                            }
                            tr {
                                th { "Transaction Count" }
                                td { (transaction_count) }
                            }
                        }
                        tr {
                            th { "Consensus Block Count" }
                            td { (consensus_block_count) }
                        }
                        tr {
                            th { "Consensus Fee Rate" }
                            td {
                                @if let Some(fee_rate) = consensus_fee_rate {
                                    (fee_rate) " sat/vbyte"
                                } @else {
                                    "No consensus fee rate available"
                                }
                            }
                        }
                        tr {
                            th { "Send Fee" }
                            td {
                                @if let Some(fee) = send_fee {
                                    (fee.to_sat()) " sat"
                                } @else {
                                    "No send fee available"
                                }
                            }
                        }
                        tr {
                            th { "Receive Fee" }
                            td {
                                @if let Some(fee) = receive_fee {
                                    (fee.to_sat()) " sat"
                                } @else {
                                    "No receive fee available"
                                }
                            }
                        }
                    }
                }

                @if !pending_tx_chain.is_empty() {
                    div class="mb-4" {
                        h5 { "Pending Transaction Chain" }
                        @if stale_chain {
                            div class="alert alert-danger" role="alert" {
                                "Warning: Transaction has been pending for more than 18 blocks!"
                            }
                        }

                        table class="table" {
                            thead {
                                tr {
                                    th { "Index" }
                                    th { "Value in Custody" }
                                    th { "Fee" }
                                    th { "vBytes" }
                                    th { "Feerate" }
                                    th { "Age" }
                                    th { "Transaction" }
                                }
                            }
                            tbody {
                                @for tx in pending_tx_chain {
                                    tr {
                                        td { (tx.index) }
                                        td {
                                            @if tx.output >= tx.input {
                                                span class="text-success" { "+" (tx.output - tx.input) }
                                            } @else {
                                                span class="text-danger" { "-" (tx.input - tx.output) }
                                            }
                                        }
                                        td { (tx.fee.to_sat()) }
                                        td { (tx.vbytes) }
                                        td { (tx.feerate()) }
                                        td { (consensus_block_count.saturating_sub(tx.created)) }
                                        td {
                                            a href={ "https://mempool.space/tx/" (tx.txid) } class="btn btn-sm btn-outline-primary" target="_blank" {
                                                "mempool.space"
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        div class="alert alert-info" role="alert" {
                            "Total feerate of pending chain: " strong { (total_pending_fee / total_pending_vbytes) " sat/vbyte" }
                        }
                    }
                }

                @if let Some((tweaked_agg_pk, tweaked_sks)) = &restore_keys {
                    div class="accordion mt-4" id="shutdownAccordion" {
                        div class="accordion-item" {
                            h2 class="accordion-header" {
                                button class="accordion-button collapsed" type="button" data-bs-toggle="collapse" data-bs-target="#shutdownCollapse" aria-expanded="false" aria-controls="shutdownCollapse" {
                                    "Federation Shutdown"
                                }
                            }
                            div id="shutdownCollapse" class="accordion-collapse collapse" data-bs-parent="#shutdownAccordion" {
                                div class="accordion-body" {
                                    div class="alert alert-warning mb-3" {
                                        "To restore your remaining funds after decommissioning the federation, please go to the "
                                        a href="https://restore.picomint.org" target="_blank" { "restore tool" }
                                        " and follow the instructions. It interpolates a threshold of tweaked guardian key shares into the secret key of the current federation UTXO, verifies it against the tweaked aggregate key and sweeps the UTXO with the resulting taproot key. The keys change with every transaction. All guardians must be fully synced before extracting their shares, otherwise the shares will not match the current federation UTXO."
                                    }

                                    div class="mb-3" {
                                        p class="mb-2" { strong { "Aggregate Public Key (hex)" } }
                                        (copiable_text(tweaked_agg_pk))
                                    }

                                    div class="mb-3" {
                                        p class="mb-2" { strong { "Your Secret Key Share (hex)" } }
                                        (copiable_text(tweaked_sks))
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
