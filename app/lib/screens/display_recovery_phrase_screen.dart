import 'package:flutter/material.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/balanced_text_widget.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/scrollable_body_widget.dart';
import 'package:pico/widgets/seed_phrase_list_widget.dart';

class DisplayRecoveryPhraseScreen extends StatelessWidget {
  final List<String> seedPhrase;

  const DisplayRecoveryPhraseScreen({super.key, required this.seedPhrase});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return Scaffold(
      appBar: AppBar(title: const Text('Recovery Phrase')),
      body: ScrollableBody(
        child: Padding(
          padding: const EdgeInsets.only(top: 16, bottom: 32),
          child: BleedColumn(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              BalancedText(
                'Your recovery phrase is the only way to restore your wallet '
                'if you lose access to this device.',
                textAlign: TextAlign.center,
                style: smallStyle.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
              const SizedBox(height: 24),
              seedPhraseList(context, seedPhrase),
            ],
          ),
        ),
      ),
    );
  }
}
