import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:pico/widgets/settings_card_widget.dart';

/// One of a federation's accounts as the picker sees it: the client to hand
/// back when it is chosen, and the live balance to show while choosing.
typedef AccountOption = ({PicoClient client, ValueListenable<int?> balance});

/// Picks which of a federation's three accounts the pager sits on.
///
/// The pager only carries the accounts worth carrying — primary, plus
/// whichever others hold money or have been asked for — so this is the one
/// place all three are listed. Choosing an empty one is how it gets a page:
/// there is nothing to create, only a balance to start showing.
class SelectAccountDrawer extends StatelessWidget {
  /// Every account of the federation in view, in the order the pager would
  /// swipe them.
  final List<AccountOption> accounts;

  final void Function(PicoClient) onSelect;

  const SelectAccountDrawer({
    super.key,
    required this.accounts,
    required this.onSelect,
  });

  static Future<void> show(
    BuildContext context, {
    required List<AccountOption> accounts,
    required void Function(PicoClient) onSelect,
  }) {
    return DrawerUtils.show(
      context: context,
      child: SelectAccountDrawer(accounts: accounts, onSelect: onSelect),
    );
  }

  @override
  Widget build(BuildContext context) {
    return DrawerShell(
      children: [
        BorderedList.column(
          children: [for (final account in accounts) _row(account)],
        ),
      ],
    );
  }

  Widget _row(AccountOption account) {
    return ValueListenableBuilder<int?>(
      valueListenable: account.balance,
      builder: (context, sats, _) {
        return SettingsCard(
          icon: PhosphorIconsRegular.wallet,
          title: account.client.accountName(),
          // Null until the first value lands, which leaves the row single-line
          // rather than claiming a balance of zero it hasn't read yet.
          subtitle:
              sats == null ? null : '${NumberFormat('#,###').format(sats)} sat',
          // Every row picks, including the account already in view — landing
          // back where you started is a fair answer to opening the list.
          onTap: () {
            Navigator.of(context).pop();
            onSelect(account.client);
          },
        );
      },
    );
  }
}
