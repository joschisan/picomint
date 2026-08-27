import 'package:flutter/material.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:pico/widgets/settings_card_widget.dart';

/// Confirms removing a mint: the row states what goes with it, and the
/// confirm button repeats it in the error colour rather than relying on the
/// wording alone.
class LeaveFederationDrawer extends StatefulWidget {
  final PicoClient client;
  final PicoClientFactory clientFactory;
  final VoidCallback onSuccess;

  const LeaveFederationDrawer({
    super.key,
    required this.client,
    required this.clientFactory,
    required this.onSuccess,
  });

  static Future<void> show(
    BuildContext context, {
    required PicoClient client,
    required PicoClientFactory clientFactory,
    required VoidCallback onSuccess,
  }) {
    return DrawerUtils.show(
      context: context,
      child: LeaveFederationDrawer(
        client: client,
        clientFactory: clientFactory,
        onSuccess: onSuccess,
      ),
    );
  }

  @override
  State<LeaveFederationDrawer> createState() => _LeaveFederationDrawerState();
}

class _LeaveFederationDrawerState extends State<LeaveFederationDrawer> {
  // Resolved once: the call hands back a fresh future each time, so building
  // it inline would re-resolve on every rebuild the button's spinner causes.
  late final Future<String?> _name = widget.client.federationName();

  Future<void> _handleLeaveFederation() async {
    await widget.clientFactory.leave(
      federationId: widget.client.federationId(),
    );

    if (!mounted) return;

    Navigator.of(context).pop();
    widget.onSuccess();
  }

  @override
  Widget build(BuildContext context) {
    final error = Theme.of(context).colorScheme.error;

    return FutureBuilder<String?>(
      future: _name,
      builder: (context, snapshot) {
        return DrawerShell(
          children: [
            BorderedList.column(
              children: [
                SettingsCard(
                  icon: PhosphorIconsRegular.signOut,
                  iconColor: error,
                  title: 'Remove ${snapshot.data ?? 'Mint'}',
                  subtitle: 'Delete eCash',
                ),
              ],
            ),
            const SizedBox(height: 16),
            AsyncButton(
              text: 'Confirm',
              color: error,
              onPressed: _handleLeaveFederation,
            ),
          ],
        );
      },
    );
  }
}
