import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/bridge_generated.dart/lnurl.dart';
import 'package:pico/screens/confirm_lnurl_send_screen.dart';
import 'package:pico/widgets/amount_entry_widget.dart';
import 'package:pico/widgets/max_action_widget.dart';

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
  /// Resolves the invoice for the entered amount, selects a gateway and
  /// quotes its fee, then hands both off to the confirmation screen.
  Future<void> _handleConfirm(int amountSats) async {
    final invoice = await lnurlResolve(
      payResponse: widget.payResponse,
      amountSats: amountSats,
    );

    final gateway = await widget.client.lnSelectGateway();

    final feeSats = gateway.gatewayFeeForInvoice(invoice: invoice);

    _confirm(
      invoice: invoice,
      amountSats: amountSats,
      gateway: gateway,
      feeSats: feeSats,
      isMax: false,
    );
  }

  /// Runs from the app bar's Max action, straight to the confirmation
  /// screen: selects a gateway, prices the max through it, and hands over
  /// the lnurl itself — no invoice: the max send resolves its own at pay
  /// time, sized by the code that spends it, so the account is emptied even
  /// if the balance moves while the confirmation is up. The selected
  /// gateway rides along, so the fee reviewed is the fee paid.
  ///
  /// A max the payee would not accept is an error, not a capped payment: a
  /// payment capped to the payee's limit would leave notes behind, and the
  /// max path exists precisely to leave none.
  Future<void> _handleConfirmMax() async {
    final gateway = await widget.client.lnSelectGateway();

    final amountSats = await widget.client.lnMaxAmount(gateway: gateway);

    if (amountSats <= 0) throw 'This account is empty';

    if (amountSats < widget.payResponse.minSats) {
      throw 'This account holds less than this address accepts';
    }

    if (widget.payResponse.maxSats < amountSats) {
      throw 'This account holds more than this address accepts';
    }

    _confirm(
      invoice: null,
      amountSats: amountSats,
      gateway: gateway,
      feeSats: gateway.gatewayFeeForAmount(amountSats: amountSats),
      isMax: true,
    );
  }

  void _confirm({
    required Bolt11InvoiceWrapper? invoice,
    required int amountSats,
    required GatewayInfoWrapper gateway,
    required int feeSats,
    required bool isMax,
  }) {
    if (!mounted) return;

    Navigator.of(context).pushReplacement(
      MaterialPageRoute(
        builder:
            (_) => ConfirmLnurlSendScreen(
              client: widget.client,
              clientFactory: widget.clientFactory,
              invoice: invoice,
              lnurl: widget.lnurl,
              amountSats: amountSats,
              gateway: gateway,
              feeSats: feeSats,
              contactName: widget.contactName,
              isMax: isMax,
            ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      resizeToAvoidBottomInset: false,
      appBar: AppBar(
        title: Text(widget.contactName ?? 'Send Lightning'),
        actions: [MaxAction(onPressed: _handleConfirmMax)],
      ),
      body: SafeArea(
        maintainBottomViewPadding: true,
        child: AmountEntryWidget(
          client: widget.client,
          onConfirm: _handleConfirm,
          buttonText: 'Continue',
        ),
      ),
    );
  }
}
