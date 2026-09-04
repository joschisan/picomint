use maud::{Markup, html};
use picomint_core::expiry::ExpiryStatus;
use picomint_redb::ReadTx;

use crate::consensus::onchain;
use crate::ui::dashboard::{BACKUP_CONFIG_ROUTE, CLEAR_EXPIRY_ROUTE, expiry};
use crate::ui::{copiable_text, modal_header};

fn action_item(title: &str, desc: &str, onclick: &str) -> Markup {
    html! {
        div class="action-item" onclick=(onclick) {
            div {
                div class="action-item-title" { (title) }
                div class="action-item-desc" { (desc) }
            }
        }
    }
}

/// Renders the Actions launcher dialog (opened from the top bar) and the
/// expiry and sweep dialogs it links to. Picking a launcher item closes
/// the launcher and opens the matching action dialog; Download Backup is
/// a direct download link. The sweep item only appears once the wallet
/// has restore keys.
pub fn render(
    server: &crate::consensus::server::Server,
    dbtx: &ReadTx,
    expiry_status: Option<&ExpiryStatus>,
) -> Markup {
    let restore_keys = onchain::restore_keys(server, dbtx);

    html! {
        // `autofocus` on the <dialog> itself makes showModal() focus the
        // dialog rather than its first button, so the close X never opens
        // pre-highlighted.
        dialog id="actions-modal" autofocus {
            (modal_header("Actions"))
            div {
                (action_item(
                    "Generate Invite",
                    "Onboard users to your mint.",
                    "this.closest('dialog').close();document.getElementById('invite-modal').showModal()",
                ))
                (action_item(
                    "Add Gateway",
                    "Enable lightning payments.",
                    "this.closest('dialog').close();document.getElementById('gateway-modal').showModal()",
                ))
                a class="action-item" href=(BACKUP_CONFIG_ROUTE) download="config.json"
                    onclick="this.closest('dialog').close()" {
                    div {
                        div class="action-item-title" { "Download Backup" }
                        div class="action-item-desc" { "Save keys to restore this guardian." }
                    }
                }
                @match expiry_status {
                    Some(status) => {
                        // Clears the announcement directly — no modal; the
                        // HX-Refresh response reloads the page and the item
                        // flips back to "Announce Expiry Date".
                        div class="action-item" hx-post=(CLEAR_EXPIRY_ROUTE) hx-swap="none" {
                            div {
                                div class="action-item-title" { "Remove Expiry Date" }
                                div class="action-item-desc" {
                                    @match chrono::DateTime::from_timestamp(status.timestamp as i64, 0) {
                                        Some(date) => { (date.format("%B %-d, %Y")) }
                                        None => { "Expiry announced." }
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        (action_item(
                            "Announce Expiry Date",
                            "Instruct users to migrate funds.",
                            "this.closest('dialog').close();document.getElementById('expiry-modal').showModal()",
                        ))
                    }
                }
                @if restore_keys.is_some() {
                    (action_item(
                        "Sweep Wallet",
                        "Sweep remaining funds after expiry.",
                        "this.closest('dialog').close();document.getElementById('sweep-modal').showModal()",
                    ))
                }
            }
        }

        @if expiry_status.is_none() {
            dialog id="expiry-modal" autofocus {
                (modal_header("Announce Expiry Date"))
                div class="modal-body" {
                    div id="expiry-section" {
                        (expiry::expiry_form(None))
                    }
                }
            }
        }

        @if let Some((tweaked_agg_pk, tweaked_sks)) = &restore_keys {
            dialog id="sweep-modal" autofocus {
                (modal_header("Sweep Wallet"))
                div class="modal-body" {
                    div class="alert alert-warning" {
                        "To restore your remaining funds after decommissioning the mint, please go to the "
                        a href="https://restore.picomint.org" target="_blank" { "restore tool" }
                        " and follow the instructions."
                    }

                    div class="field" {
                        span class="field-label" { "Aggregate Public Key (hex)" }
                        (copiable_text(tweaked_agg_pk))
                    }

                    div class="field" {
                        span class="field-label" { "Your Secret Key Share (hex)" }
                        (copiable_text(tweaked_sks))
                    }
                }
            }
        }
    }
}
