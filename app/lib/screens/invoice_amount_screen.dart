import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/screens/display_invoice_screen.dart';
import 'package:pico/widgets/amount_entry_widget.dart';

class InvoiceAmountScreen extends StatefulWidget {
  final PicoClient client;
  final PicoClientFactory clientFactory;

  const InvoiceAmountScreen({
    super.key,
    required this.client,
    required this.clientFactory,
  });

  @override
  State<InvoiceAmountScreen> createState() => _InvoiceAmountScreenState();
}

class _InvoiceAmountScreenState extends State<InvoiceAmountScreen> {
  late final PicoClient _client = widget.client;

  Future<void> _handleConfirm(int amountSats) async {
    final gateway = await _client.lnSelectGateway();

    final feeSats = gateway.gatewayFeeForReceiveAmount(amountSats: amountSats);

    final invoice = await _client.lnReceive(
      gateway: gateway,
      amountSat: amountSats,
    );

    if (!mounted) return;

    Navigator.of(context).pushReplacement(
      MaterialPageRoute(
        builder:
            (_) => DisplayInvoiceScreen(
              client: _client,
              invoice: invoice,
              amount: amountSats,
              feeSats: feeSats,
            ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Receive Lightning')),
      body: SafeArea(
        child: AmountEntryWidget(
          key: ValueKey(_client.federationId()),
          client: _client,
          onConfirm: _handleConfirm,
        ),
      ),
    );
  }
}
