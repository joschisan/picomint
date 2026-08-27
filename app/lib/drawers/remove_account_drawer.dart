import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/utils/account_utils.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/widgets/amount_rows.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:pico/widgets/settings_card_widget.dart';

/// Confirms taking an account off the pager, which first moves everything it
/// holds to primary.
///
/// Nothing is destroyed here, unlike removing a mint: the account keeps
/// existing and its history stays in the log — it simply stops having a page.
/// The move is a real ecash payment between two accounts of the same
/// federation, so the notes are reissued to primary's own nonces and a later
/// recovery from the seed finds them where they now are rather than where
/// they were.
class RemoveAccountDrawer extends StatefulWidget {
  final PicoClient account;

  /// Shown while the sheet is up. The transfer reads the balance itself, so
  /// nothing here decides how much moves.
  final ValueListenable<int?> balance;

  final VoidCallback onSuccess;

  const RemoveAccountDrawer({
    super.key,
    required this.account,
    required this.balance,
    required this.onSuccess,
  });

  static Future<void> show(
    BuildContext context, {
    required PicoClient account,
    required ValueListenable<int?> balance,
    required VoidCallback onSuccess,
  }) {
    return DrawerUtils.show(
      context: context,
      child: RemoveAccountDrawer(
        account: account,
        balance: balance,
        onSuccess: onSuccess,
      ),
    );
  }

  @override
  State<RemoveAccountDrawer> createState() => _RemoveAccountDrawerState();
}

class _RemoveAccountDrawerState extends State<RemoveAccountDrawer> {
  /// Sweeps the balance to primary, then hands back to the caller to drop the
  /// page. One call rather than a send and a receive from here: between the
  /// two the notes are held in neither account, and the amount to send is the
  /// exact balance — a millisat figure this side never sees. A failure
  /// surfaces the way any other payment's does, with the account left where
  /// it is.
  Future<void> _handleRemove() async {
    await widget.account.transferToPrimary();

    if (!mounted) return;

    Navigator.of(context).pop();
    widget.onSuccess();
  }

  @override
  Widget build(BuildContext context) {
    return ValueListenableBuilder<int?>(
      valueListenable: widget.balance,
      builder: (context, sats, _) {
        return DrawerShell(
          children: [
            BorderedList.column(
              children: [
                // Untinted, unlike removing a mint: nothing is destroyed
                // here. The balance moves to primary and the account stops
                // having a page — both of which the user can simply undo.
                SettingsCard(
                  icon: PhosphorIconsRegular.signOut,
                  title: 'Remove ${widget.account.accountName()} Account',
                  subtitle: 'Transfer eCash to $primaryAccount Account',
                ),
                // What the move is worth, on the same rows a payment
                // confirmation uses — because that is what this is.
                ...amountRows(client: widget.account, amountSats: sats ?? 0),
              ],
            ),
            const SizedBox(height: 16),
            AsyncButton(text: 'Confirm', onPressed: _handleRemove),
          ],
        );
      },
    );
  }
}
