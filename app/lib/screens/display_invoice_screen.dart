import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/widgets/qr_code_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/shareable_row_widget.dart';
import 'package:pico/widgets/detail_row_widget.dart';
import 'package:pico/widgets/amount_rows.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/scrollable_body_widget.dart';

class DisplayInvoiceScreen extends StatelessWidget {
  final PicoClient client;
  final String invoice;
  final int amount;
  final int feeSats;

  const DisplayInvoiceScreen({
    super.key,
    required this.client,
    required this.invoice,
    required this.amount,
    required this.feeSats,
  });

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('Receive Lightning')),
    body: ScrollableBody(
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 16.0),
        child: BleedColumn(
          children: [
            QrCodeWidget(data: invoice),
            const SizedBox(height: 16),
            BorderedList.column(
              children: [
                ShareableRow(data: invoice, label: 'Lightning Invoice'),
                ...amountRows(client: client, amountSats: amount),
                DetailRow(
                  icon: PhosphorIconsRegular.network,
                  label: 'Network Fee',
                  value:
                      '${NumberFormat('#,###').format(feeSats)} sat · ${(feeSats / amount * 100).toStringAsFixed(1)}%',
                ),
              ],
            ),
          ],
        ),
      ),
    ),
  );
}
