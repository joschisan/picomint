import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/events.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/payment_summary_row_widget.dart';
import 'package:pico/widgets/detail_row_widget.dart';
import 'package:pico/widgets/amount_rows.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/widgets/warning_card_widget.dart';
import 'package:pico/utils/auth_utils.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/utils/notification_utils.dart';

/// Confirms a Lightning payment. The gateway is selected and its fee quoted
/// as soon as the drawer opens; the fee row shows a spinner until the quote
/// arrives and the confirm button stays disabled without one. A failed quote
/// pops the drawer and surfaces an error notification. The quoted gateway is
/// passed to [PicoClient.lnSend] so the fee shown here matches what is charged.
class LightningSendDrawer extends StatefulWidget {
  final PicoClient client;
  final Bolt11InvoiceWrapper invoice;

  const LightningSendDrawer({
    super.key,
    required this.client,
    required this.invoice,
  });

  static Future<void> show(
    BuildContext context, {
    required PicoClient client,
    required Bolt11InvoiceWrapper invoice,
  }) {
    return DrawerUtils.show(
      context: context,
      child: LightningSendDrawer(client: client, invoice: invoice),
    );
  }

  @override
  State<LightningSendDrawer> createState() => _LightningSendDrawerState();
}

/// The selected gateway paired with the fee it quoted for this invoice. Kept
/// together so `lnSend` can't be handed a gateway other than the one the
/// displayed fee came from.
typedef _Quote = ({GatewayInfoWrapper gateway, int feeSats});

class _LightningSendDrawerState extends State<LightningSendDrawer> {
  _Quote? _quote;

  @override
  void initState() {
    super.initState();
    _loadQuote();
  }

  Future<void> _loadQuote() async {
    try {
      final gateway = await widget.client.lnSelectGatewayForInvoice(
        invoice: widget.invoice,
      );

      final feeSats = gateway.gatewayFeeForInvoice(invoice: widget.invoice);

      if (!mounted) return;

      setState(() => _quote = (gateway: gateway, feeSats: feeSats));
    } catch (error) {
      if (!mounted) return;

      Navigator.of(context).pop();
      NotificationUtils.showError(context, error.toString());
    }
  }

  Future<void> _handleConfirm(_Quote quote) async {
    await requireBiometricAuth(context);

    await widget.client.lnSend(gateway: quote.gateway, invoice: widget.invoice);

    if (!mounted) return;

    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final amountSats = widget.invoice.amountSats();
    final quote = _quote;

    return DrawerShell(
      children: [
        BorderedList.column(
          children: [
            const PaymentSummaryRow(
              paymentType: PaymentType.lightning,
              incoming: false,
              status: 'Send',
            ),
            ...amountRows(client: widget.client, amountSats: amountSats),
            DetailRow(
              icon: PhosphorIconsRegular.network,
              label: 'Network Fee',
              value:
                  quote == null
                      ? null
                      : '${NumberFormat('#,###').format(quote.feeSats)} sat · ${NumberFormat('#,##0.#').format(quote.feeSats / amountSats * 100)}%',
            ),
          ],
        ),
        if (quote != null && quote.feeSats > amountSats * 0.02) ...[
          const SizedBox(height: 16),
          WarningCard(
            icon: PhosphorIconsRegular.warning,
            text:
                'High Relative Fee of ${NumberFormat('#,##0.#').format(quote.feeSats / amountSats * 100)}%',
          ),
        ],
        const SizedBox(height: 16),
        AsyncButton(
          text: 'Confirm',
          enabled: quote != null,
          onPressed: () => _handleConfirm(quote!),
        ),
      ],
    );
  }
}
