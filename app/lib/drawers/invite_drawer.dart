import 'package:flutter/material.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:pico/widgets/settings_card_widget.dart';

/// Confirms a scanned invite. One button, because there is only one thing to
/// do: `join` rebuilds whatever this seed already owns at the mint, so adding
/// and restoring are the same act. Asking the user to remember whether they
/// had used this mint before was asking them to answer for the wallet — and
/// the wrong answer stranded the eCash a restore would have found.
///
/// Calls into the factory itself rather than firing caller callbacks; the
/// scanner that pushed this drawer has already popped, so the drawer's own
/// context is the only reliable one to pop from once the call returns.
class InviteDrawer extends StatelessWidget {
  final InviteCodeWrapper invite;
  final PicoClientFactory clientFactory;

  const InviteDrawer({
    super.key,
    required this.invite,
    required this.clientFactory,
  });

  static Future<void> show(
    BuildContext context, {
    required InviteCodeWrapper invite,
    required PicoClientFactory clientFactory,
  }) {
    return DrawerUtils.show(
      context: context,
      child: InviteDrawer(invite: invite, clientFactory: clientFactory),
    );
  }

  // The scan runs inside `join`, so the button spins for its duration and the
  // drawer only closes once the wallet is actually there. No toast: the mint
  // is selected on arrival and its row names it, which says more than a
  // one-off message would.
  Future<void> _handleAdd(BuildContext context) async {
    await clientFactory.join(invite: invite);
    if (!context.mounted) return;
    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    return DrawerShell(
      children: [
        BorderedList.column(
          children: const [
            SettingsCard(
              icon: PhosphorIconsRegular.stack,
              title: 'Add Mint',
              subtitle: 'Add a new mint',
            ),
          ],
        ),
        const SizedBox(height: 16),
        AsyncButton(text: 'Confirm', onPressed: () => _handleAdd(context)),
      ],
    );
  }
}
