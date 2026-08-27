import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/bridge_generated.dart/fountain.dart';
import 'package:pico/screens/display_ecash_screen.dart';
import 'package:pico/utils/auth_utils.dart';
import 'package:pico/widgets/amount_entry_widget.dart';

class EcashAmountScreen extends StatefulWidget {
  final PicoClient client;
  final PicoClientFactory clientFactory;

  const EcashAmountScreen({
    super.key,
    required this.client,
    required this.clientFactory,
  });

  @override
  State<EcashAmountScreen> createState() => _EcashAmountScreenState();
}

class _EcashAmountScreenState extends State<EcashAmountScreen> {
  late final PicoClient _client = widget.client;

  Future<void> _handleConfirm(int amountSats) async {
    await requireBiometricAuth(context);

    final ecash = await _client.ecashSend(amountSat: amountSats);

    if (!mounted) return;

    Navigator.of(context).pushReplacement(
      MaterialPageRoute(
        builder:
            (_) => DisplayEcashScreen(
              client: _client,
              ecash: ecash,
              encoder: ECashEncoder(ecash: ecash),
            ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Send eCash')),
      body: SafeArea(
        child: Column(
          children: [
            Expanded(
              child: AmountEntryWidget(
                key: ValueKey(_client.federationId()),
                client: _client,
                onConfirm: _handleConfirm,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
