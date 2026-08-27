import 'dart:async';

import 'package:flutter/material.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/drawers/scanner_drawer.dart';
import 'package:pico/screens/home_screen.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/balanced_text_widget.dart';
import 'package:pico/widgets/circular_action_button_widget.dart';

/// Where a wallet with no federation lands. Scanning an invite is the only
/// thing to do here, so the screen is just that: the reason and the action.
///
/// Watches for the first federation and hands the wallet to [HomeScreen] for
/// good — leaving the last federation is blocked, so nothing comes back here
/// short of a restart with an empty wallet.
class OnboardingScreen extends StatefulWidget {
  final PicoClientFactory clientFactory;

  const OnboardingScreen({super.key, required this.clientFactory});

  @override
  State<OnboardingScreen> createState() => _OnboardingScreenState();
}

class _OnboardingScreenState extends State<OnboardingScreen> {
  StreamSubscription<List<PicoClient>>? _subscription;

  @override
  void initState() {
    super.initState();
    _subscription = widget.clientFactory.subscribeClients().listen((clients) {
      if (!mounted || clients.isEmpty) return;
      // Clears the scanner and invite drawers along with this screen, so the
      // wallet lands on the home screen with nothing stacked behind it. The
      // invite drawer's own pop is a no-op once its route is gone.
      Navigator.of(context).pushAndRemoveUntil(
        MaterialPageRoute(
          builder:
              (_) => HomeScreen(
                clientFactory: widget.clientFactory,
                initialClients: clients,
              ),
        ),
        (_) => false,
      );
    });
  }

  @override
  void dispose() {
    _subscription?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 32),
              child: BalancedText(
                'Add a mint to transact.',
                textAlign: TextAlign.center,
                style: smallStyle.copyWith(
                  color: Theme.of(context).colorScheme.onSurfaceVariant,
                ),
              ),
            ),
            const SizedBox(height: 24),
            CircularActionButton(
              icon: PhosphorIconsRegular.qrCode,
              label: 'Scan',
              // No client to hand the scanner — with none joined it accepts
              // invite codes only, which is exactly the one input this screen
              // is here to take.
              onTap:
                  () => ScannerDrawer.show(
                    context,
                    client: null,
                    clientFactory: widget.clientFactory,
                  ),
            ),
          ],
        ),
      ),
    );
  }
}
