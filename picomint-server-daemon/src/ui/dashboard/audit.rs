use maud::{Markup, html};
use picomint_core::module::audit::AuditSummary;

pub fn render(audit_summary: &AuditSummary) -> Markup {
    let rows = [
        ("Mint", audit_summary.mint),
        ("Wallet", audit_summary.wallet),
        ("Ln", audit_summary.ln),
    ];

    html! {
        div class="card h-100" {
            div class="card-header dashboard-header" { "Audit Summary" }
            div class="card-body" {
                div class="mb-3" {
                    div class="alert alert-info" {
                        "Total Net Assets: " strong { (format!("{} msat", audit_summary.total)) }
                    }
                }

                table class="table table-striped" {
                    thead {
                        tr {
                            th { "Module Kind" }
                            th { "Net Assets (msat)" }
                        }
                    }
                    tbody {
                        @for (kind, net_assets) in rows {
                            tr {
                                td { (kind) }
                                td { (net_assets) }
                            }
                        }
                    }
                }
            }
        }
    }
}
