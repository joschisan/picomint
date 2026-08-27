import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/screens/confirm_onchain_send_screen.dart';
import 'package:pico/widgets/amount_entry_widget.dart';

class OnchainAmountScreen extends StatefulWidget {
  final PicoClient client;
  final PicoClientFactory clientFactory;
  final BitcoinAddressWrapper address;

  const OnchainAmountScreen({
    super.key,
    required this.client,
    required this.clientFactory,
    required this.address,
  });

  @override
  State<OnchainAmountScreen> createState() => _OnchainAmountScreenState();
}

class _OnchainAmountScreenState extends State<OnchainAmountScreen> {
  Future<void> _handleConfirm(int amountSats) async {
    final feeSats = await widget.client.onchainCalculateFees(
      address: widget.address,
      amountSats: amountSats,
    );

    if (!mounted) return;

    Navigator.of(context).pushReplacement(
      MaterialPageRoute(
        builder:
            (_) => ConfirmOnchainSendScreen(
              client: widget.client,
              address: widget.address,
              amountSats: amountSats,
              feeSats: feeSats,
            ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Send Onchain')),
      body: SafeArea(
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
