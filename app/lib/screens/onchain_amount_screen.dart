import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/screens/confirm_onchain_send_screen.dart';
import 'package:pico/widgets/amount_entry_widget.dart';
import 'package:pico/widgets/max_action_widget.dart';

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

    _confirm(amountSats: amountSats, feeSats: feeSats, isMax: false);
  }

  /// Runs from the app bar's Max action, straight to the confirmation
  /// screen, so emptying the account is reviewed like any other send. The
  /// amount shown is this tap's quote; the send re-prices at the feerate
  /// current when it is submitted, so a feerate that moves in between moves
  /// the amount with it.
  Future<void> _handleConfirmMax() async {
    final amountSats = await widget.client.onchainMaxAmount();

    if (amountSats <= 0) throw 'This account cannot cover the onchain fee';

    final feeSats = await widget.client.onchainCalculateFees(
      address: widget.address,
      amountSats: amountSats,
    );

    _confirm(amountSats: amountSats, feeSats: feeSats, isMax: true);
  }

  void _confirm({
    required int amountSats,
    required int feeSats,
    required bool isMax,
  }) {
    if (!mounted) return;

    Navigator.of(context).pushReplacement(
      MaterialPageRoute(
        builder:
            (_) => ConfirmOnchainSendScreen(
              client: widget.client,
              address: widget.address,
              amountSats: amountSats,
              feeSats: feeSats,
              isMax: isMax,
            ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Send Onchain'),
        actions: [MaxAction(onPressed: _handleConfirmMax)],
      ),
      body: SafeArea(
        child: AmountEntryWidget(
          client: widget.client,
          onConfirm: _handleConfirm,
          buttonText: 'Continue',
        ),
      ),
    );
  }
}
