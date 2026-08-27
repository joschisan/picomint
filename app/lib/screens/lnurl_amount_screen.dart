import 'package:flutter/material.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/bridge_generated.dart/lnurl.dart';
import 'package:pico/screens/contact_name_entry_screen.dart';
import 'package:pico/screens/confirm_lnurl_send_screen.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/amount_entry_widget.dart';
import 'package:share_plus/share_plus.dart';

class LnurlAmountScreen extends StatefulWidget {
  final PicoClient client;
  final PicoClientFactory clientFactory;
  final LnurlWrapper lnurl;
  final PayResponseWrapper payResponse;
  final String? contactName;

  const LnurlAmountScreen({
    super.key,
    required this.client,
    required this.clientFactory,
    required this.lnurl,
    required this.payResponse,
    this.contactName,
  });

  @override
  State<LnurlAmountScreen> createState() => _LnurlAmountScreenState();
}

class _LnurlAmountScreenState extends State<LnurlAmountScreen> {
  late String? _contactName = widget.contactName;

  /// Resolves the invoice for the entered amount, selects the gateway for that
  /// specific invoice (exact fee, with the direct-swap shortcut applied), then
  /// hands both off to the confirmation screen.
  Future<void> _handleConfirm(int amountSats) async {
    final invoice = await lnurlResolve(
      payResponse: widget.payResponse,
      amountSats: amountSats,
    );

    final gateway = await widget.client.lnSelectGatewayForInvoice(
      invoice: invoice,
    );

    final feeSats = gateway.gatewayFeeForInvoice(invoice: invoice);

    if (!mounted) return;

    Navigator.of(context).pushReplacement(
      MaterialPageRoute(
        builder:
            (_) => ConfirmLnurlSendScreen(
              client: widget.client,
              invoice: invoice,
              amountSats: amountSats,
              gateway: gateway,
              feeSats: feeSats,
              contactName: _contactName,
            ),
      ),
    );
  }

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

  void _handleShare() {
    SharePlus.instance.share(ShareParams(text: widget.lnurl.encode()));
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      resizeToAvoidBottomInset: false,
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
            )
          else
            IconButton(
              icon: const Icon(PhosphorIconsRegular.copy, size: smallIconSize),
              onPressed: _handleShare,
            ),
        ],
      ),
      body: SafeArea(
        maintainBottomViewPadding: true,
        child: AmountEntryWidget(
          key: ValueKey(widget.client.federationId()),
          client: widget.client,
          onConfirm: _handleConfirm,
          buttonText: 'Continue',
        ),
      ),
    );
  }
}
