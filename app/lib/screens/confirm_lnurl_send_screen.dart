import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/bridge_generated.dart/lnurl.dart';
import 'package:pico/screens/contact_name_entry_screen.dart';
import 'package:pico/utils/styles.dart';
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
  final PicoClientFactory clientFactory;
  // Resolved ahead for an ordinary send, so the number reviewed is pinned
  // into the invoice paid. Null on a max send, which resolves its own: the
  // amount is picomint's to size fresh at pay time, and pinning a figure
  // here could only make it stale.
  final Bolt11InvoiceWrapper? invoice;
  final LnurlWrapper lnurl;
  final int amountSats;
  final GatewayInfoWrapper gateway;
  final int feeSats;
  final String? contactName;
  // Whether this empties the account: the send goes by the lnurl rather
  // than the invoice, funds from every note and leaves none.
  final bool isMax;

  const ConfirmLnurlSendScreen({
    super.key,
    required this.client,
    required this.clientFactory,
    required this.invoice,
    required this.lnurl,
    required this.amountSats,
    required this.gateway,
    required this.feeSats,
    this.contactName,
    this.isMax = false,
  });

  @override
  State<ConfirmLnurlSendScreen> createState() => _ConfirmLnurlSendScreenState();
}

class _ConfirmLnurlSendScreenState extends State<ConfirmLnurlSendScreen> {
  late String? _contactName = widget.contactName;

  Future<void> _handleSaveContact() async {
    final name = await Navigator.of(context).push<String>(
      MaterialPageRoute(
        builder:
            (_) => ContactNameEntryScreen(
              clientFactory: widget.clientFactory,
              lnurl: widget.lnurl,
            ),
      ),
    );

    if (mounted && name != null) {
      setState(() => _contactName = name);
    }
  }

  Future<void> _handleConfirm() async {
    await requireBiometricAuth(context);

    if (widget.isMax) {
      await widget.client.lnSendMax(
        gateway: widget.gateway,
        lnurl: widget.lnurl.encode(),
      );
    } else {
      await widget.client.lnSend(
        gateway: widget.gateway,
        invoice: widget.invoice!,
      );
    }

    if (!mounted) return;

    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(_contactName ?? 'Send Lightning'),
        actions: [
          if (_contactName == null)
            IconButton(
              icon: const Icon(
                PhosphorIconsRegular.userPlus,
                size: smallIconSize,
              ),
              onPressed: _handleSaveContact,
            ),
        ],
      ),
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
