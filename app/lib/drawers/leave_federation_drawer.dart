import 'package:flutter/material.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:pico/widgets/settings_card_widget.dart';

/// Confirms removing a mint: the row names the mint over what is about to
/// happen to it, and the confirm button carries the caution amber rather than
/// relying on the wording alone. Same shape as the delete-contact drawer.
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
    return FutureBuilder<String?>(
      future: _name,
      builder: (context, snapshot) {
        return DrawerShell(
          children: [
            BorderedList.column(
              children: [
                SettingsCard(
                  icon: PhosphorIconsRegular.trash,
                  title: 'Remove Mint',
                  subtitle: snapshot.data,
                ),
              ],
            ),
            const SizedBox(height: 16),
            AsyncButton(text: 'Confirm', onPressed: _handleLeaveFederation),
          ],
        );
      },
    );
  }
}
