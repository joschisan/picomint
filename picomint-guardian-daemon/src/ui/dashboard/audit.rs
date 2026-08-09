use bitcoin::SignedAmount;
use maud::{Markup, html};
use picomint_core::module::audit::AuditSummary;

fn format_btc(msat: i64) -> String {
    format!("{:.8} BTC", SignedAmount::from_sat(msat / 1000).to_btc())
}

pub fn render(audit_summary: &AuditSummary) -> Markup {
    let rows = [
        ("Mint", audit_summary.mint),
        ("Wallet", audit_summary.wallet),
        ("Ln", audit_summary.ln),
    ];

    html! {
        div class="card h-100" {
            div class="card-header dashboard-header" { "Audit" }
            div class="card-body" {
                div class="mb-3" {
                    div class="alert alert-info" {
                        "Total Net Assets: " strong { (format_btc(audit_summary.total)) }
                    }
                }

                table class="table table-striped" {
                    thead {
                        tr {
                            th { "Module" }
                            th { "Assets" }
                        }
                    }
                    tbody {
                        @for (kind, net_assets) in rows {
                            tr {
                                td { (kind) }
                                td { (format_btc(net_assets)) }
                            }
                        }
                    }
                }
            }
        }
    }
}
