import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/bridge_generated.dart/fountain.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/screens/display_ecash_screen.dart';
import 'package:pico/utils/auth_utils.dart';
import 'package:pico/widgets/amount_entry_widget.dart';
import 'package:pico/widgets/max_action_widget.dart';

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

    _display(await _client.ecashSend(amountSat: amountSats));
  }

  /// Spends the notes themselves, so the bundle is for whatever the account
  /// holds rather than for a figure in sats. Null is an account with no notes
  /// left — the balance reaching zero behind the entry screen — and there is
  /// nothing to show for it.
  Future<void> _handleConfirmMax() async {
    await requireBiometricAuth(context);

    final ecash = await _client.ecashSendMax();

    if (ecash == null) throw 'This account is empty';

    _display(ecash);
  }

  /// Hands the notes to the screen that shows them. Until it has them they
  /// are held nowhere else: the send returned them by value and took them out
  /// of the account.
  void _display(ECashWrapper ecash) {
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
      appBar: AppBar(
        title: const Text('Send eCash'),
        actions: [MaxAction(onPressed: _handleConfirmMax)],
      ),
      body: SafeArea(
        child: Column(
          children: [
            Expanded(
              child: AmountEntryWidget(
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
