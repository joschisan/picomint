import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/screens/onboarding_screen.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/utils/notification_utils.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/scrollable_body_widget.dart';
import 'package:pico/widgets/seed_phrase_list_widget.dart';

class ConfirmRecoveryPhraseScreen extends StatelessWidget {
  final DatabaseWrapper db;
  final List<String> seedPhrase;

  const ConfirmRecoveryPhraseScreen({
    super.key,
    required this.db,
    required this.seedPhrase,
  });

  Future<void> _recoverWallet(BuildContext context) async {
    final mnemonic = await parseMnemonic(words: seedPhrase);

    if (mnemonic == null) {
      if (context.mounted) {
        NotificationUtils.showError(context, 'Failed to parse recovery phrase');
      }
      return;
    }

    final clientFactory = await PicoClientFactory.init(
      db: db,
      mnemonic: mnemonic,
    );

    if (context.mounted) {
      Navigator.of(context).pushAndRemoveUntil(
        MaterialPageRoute(
          builder: (context) => OnboardingScreen(clientFactory: clientFactory),
        ),
        (route) => false,
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Recovery Phrase')),
      body: ScrollableBody(
        child: Padding(
          padding: const EdgeInsets.only(top: 16, bottom: 32),
          child: BleedColumn(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              seedPhraseList(context, seedPhrase),
              const SizedBox(height: 16),
              AsyncButton(
                text: 'Confirm',
                onPressed: () => _recoverWallet(context),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
