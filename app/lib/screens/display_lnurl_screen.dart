import 'package:flutter/material.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/screens/invoice_amount_screen.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/balanced_text_widget.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/qr_code_widget.dart';
import 'package:pico/widgets/scrollable_body_widget.dart';
import 'package:pico/widgets/settings_card_widget.dart';
import 'package:pico/widgets/shareable_row_widget.dart';
import 'package:url_launcher/url_launcher.dart';

/// Where Receive Lightning lands: the reusable code first, since it takes any
/// amount any number of times. A fixed-amount Bolt11 invoice is the app-bar
/// action, and the point-of-sale terminal a row beneath the code it runs on.
class DisplayLnurlScreen extends StatelessWidget {
  final PicoClient client;
  final PicoClientFactory clientFactory;
  final String lnurl;
  final String currencyCode;

  const DisplayLnurlScreen({
    super.key,
    required this.client,
    required this.clientFactory,
    required this.lnurl,
    required this.currencyCode,
  });

  /// Opens the kasse web point-of-sale, pre-loaded with this Lightning Url and
  /// currency so it lands directly on amount entry.
  Future<void> _openCheckout() async {
    final url = Uri.https('joschisan.github.io', '/kasse/', {
      'lnurl': lnurl,
      'currency': currencyCode,
    });
    await launchUrl(url, mode: LaunchMode.externalApplication);
  }

  void _createInvoice(BuildContext context) {
    // Replace this reusable-LNURL screen with amount entry rather than stacking
    // on top, so the amount screen reads as an alternative to this view.
    Navigator.of(context).pushReplacement(
      MaterialPageRoute(
        builder:
            (_) => InvoiceAmountScreen(
              client: client,
              clientFactory: clientFactory,
            ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      title: const Text('Receive Lightning'),
      actions: [
        IconButton(
          icon: const Icon(PhosphorIconsRegular.dotsNine, size: smallIconSize),
          onPressed: () => _createInvoice(context),
        ),
      ],
    ),
    body: ScrollableBody(
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 16.0),
        child: BleedColumn(
          children: [
            QrCodeWidget(data: lnurl),
            const SizedBox(height: 16),
            BorderedList.column(
              children: [
                ShareableRow(data: lnurl, label: 'Lightning Url'),
                SettingsCard(
                  icon: PhosphorIconsRegular.dotsNine,
                  title: 'Point of Sale',
                  subtitle: 'Open in browser',
                  onTap: _openCheckout,
                ),
              ],
            ),
            Expanded(
              child: Center(
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 32),
                  child: BalancedText(
                    'This is a reusable payment code.',
                    style: smallStyle.copyWith(
                      color: Theme.of(context).colorScheme.onSurfaceVariant,
                    ),
                    textAlign: TextAlign.center,
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    ),
  );
}
