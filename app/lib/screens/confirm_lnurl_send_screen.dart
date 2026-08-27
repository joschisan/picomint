import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/detail_row_widget.dart';
import 'package:pico/widgets/amount_rows.dart';
import 'package:pico/widgets/warning_card_widget.dart';
import 'package:pico/utils/auth_utils.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/scrollable_body_widget.dart';

class ConfirmLnurlSendScreen extends StatefulWidget {
  final PicoClient client;
  final Bolt11InvoiceWrapper invoice;
  final int amountSats;
  final GatewayInfoWrapper gateway;
  final int feeSats;
  final String? contactName;

  const ConfirmLnurlSendScreen({
    super.key,
    required this.client,
    required this.invoice,
    required this.amountSats,
    required this.gateway,
    required this.feeSats,
    this.contactName,
  });

  @override
  State<ConfirmLnurlSendScreen> createState() => _ConfirmLnurlSendScreenState();
}

class _ConfirmLnurlSendScreenState extends State<ConfirmLnurlSendScreen> {
  Future<void> _handleConfirm() async {
    await requireBiometricAuth(context);

    await widget.client.lnSend(
      gateway: widget.gateway,
      invoice: widget.invoice,
    );

    if (!mounted) return;

    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text(widget.contactName ?? 'Send Lightning')),
      body: ScrollableBody(
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 16),
          child: BleedColumn(
            children: [
              BorderedList.column(
                children: [
                  ...amountRows(
                    client: widget.client,
                    amountSats: widget.amountSats,
                  ),
                  DetailRow(
                    icon: PhosphorIconsRegular.network,
                    label: 'Network Fee',
                    value:
                        '${NumberFormat('#,###').format(widget.feeSats)} sat · ${(widget.feeSats / widget.amountSats * 100).toStringAsFixed(1)}%',
                  ),
                ],
              ),
              const Spacer(),
              if (widget.feeSats > widget.amountSats * 0.02) ...[
                WarningCard(
                  icon: PhosphorIconsRegular.warning,
                  text:
                      'High Relative Fee of ${(widget.feeSats / widget.amountSats * 100).toStringAsFixed(1)}%',
                ),
                const SizedBox(height: 16),
              ],
              AsyncButton(text: 'Confirm', onPressed: _handleConfirm),
            ],
          ),
        ),
      ),
    );
  }
}
