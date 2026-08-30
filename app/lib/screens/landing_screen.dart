import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'dart:async';
import 'package:flutter/material.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/screens/input_recovery_phrase_screen.dart';
import 'package:pico/screens/onboarding_screen.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/scrollable_body_widget.dart';

const _variants = [
  (PhosphorIconsRegular.lightning, 'Lightning'),
  (PhosphorIconsRegular.link, 'Onchain'),
  (PhosphorIconsRegular.coinVertical, 'eCash'),
];

class LandingScreen extends StatefulWidget {
  final DatabaseWrapper db;

  const LandingScreen({super.key, required this.db});

  @override
  State<LandingScreen> createState() => _LandingScreenState();
}

class _LandingScreenState extends State<LandingScreen> {
  final _pageController = PageController();
  late final Timer _timer;

  @override
  void initState() {
    super.initState();
    _timer = Timer.periodic(const Duration(seconds: 2), (_) {
      _pageController.nextPage(
        duration: const Duration(milliseconds: 500),
        curve: Curves.easeInOut,
      );
    });
  }

  @override
  void dispose() {
    _timer.cancel();
    _pageController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: ScrollableBody(
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 16.0),
          child: BleedColumn(
            mainAxisAlignment: MainAxisAlignment.center,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Expanded(
                child: Center(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      SizedBox(
                        height: 64,
                        child: PageView.builder(
                          controller: _pageController,
                          physics: const NeverScrollableScrollPhysics(),
                          itemBuilder: (context, index) {
                            final (icon, name) =
                                _variants[index % _variants.length];
                            return Row(
                              mainAxisAlignment: MainAxisAlignment.center,
                              children: [
                                Icon(
                                  icon,
                                  size: heroIconSize,
                                  color: Theme.of(context).colorScheme.primary,
                                ),
                                const SizedBox(width: 16),
                                Text(name, style: heroStyle),
                              ],
                            );
                          },
                        ),
                      ),
                      const SizedBox(height: 16),
                      Text('Powered by Picomint', style: mediumStyle),
                    ],
                  ),
                ),
              ),
              AsyncButton(
                text: 'Generate New Wallet',
                onPressed: () async {
                  final mnemonic = await generateMnemonic();

                  final clientFactory = await PicoClientFactory.init(
                    db: widget.db,
                    mnemonic: mnemonic,
                  );

                  if (!context.mounted) return;

                  Navigator.of(context).pushReplacement(
                    MaterialPageRoute(
                      builder:
                          (context) =>
                              OnboardingScreen(clientFactory: clientFactory),
                    ),
                  );
                },
              ),
              const SizedBox(height: 24),
              GestureDetector(
                onTap: () {
                  Navigator.of(context).push(
                    MaterialPageRoute(
                      builder:
                          (context) => InputRecoveryPhraseScreen(
                            db: widget.db,
                            partialSeedPhrase: const [],
                          ),
                    ),
                  );
                },
                child: Text(
                  'Already have a wallet?',
                  style: mediumStyle.copyWith(
                    color: Theme.of(context).colorScheme.primary,
                  ),
                  textAlign: TextAlign.center,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
