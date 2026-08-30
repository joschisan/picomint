import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/detail_row_widget.dart';
import 'package:pico/widgets/amount_rows.dart';
import 'package:pico/widgets/shareable_row_widget.dart';
import 'package:pico/widgets/warning_card_widget.dart';
import 'package:pico/utils/auth_utils.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/scrollable_body_widget.dart';

class ConfirmOnchainSendScreen extends StatefulWidget {
  final PicoClient client;
  final BitcoinAddressWrapper address;
  final int amountSats;
  final int feeSats;
  // Whether this empties the account. The amount above is what it was quoted
  // at — the send itself re-prices, since it funds from every note and sizes
  // the output to what they cover.
  final bool isMax;

  const ConfirmOnchainSendScreen({
    super.key,
    required this.client,
    required this.address,
    required this.amountSats,
    required this.feeSats,
    this.isMax = false,
  });

  @override
  State<ConfirmOnchainSendScreen> createState() =>
      _ConfirmOnchainSendScreenState();
}

class _ConfirmOnchainSendScreenState extends State<ConfirmOnchainSendScreen> {
  Future<void> _handleConfirm() async {
    await requireBiometricAuth(context);

    if (widget.isMax) {
      await widget.client.onchainSendMax(address: widget.address);
    } else {
      await widget.client.onchainSend(
        address: widget.address,
        amountSats: widget.amountSats,
      );
    }

    if (!mounted) return;

    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Send Onchain')),
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
                  ShareableRow(
                    data: widget.address.toString(),
                    label: 'Bitcoin Address',
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
