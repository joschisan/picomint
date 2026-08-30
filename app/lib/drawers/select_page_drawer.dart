import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:pico/widgets/settings_card_widget.dart';

/// One page of the pager as the list sees it: the client to page to when it
/// is chosen, and the federation name and balance to show while choosing —
/// both live, since the page they describe keeps updating behind the sheet.
typedef PageOption =
    ({
      PicoClient client,
      ValueListenable<String?> name,
      ValueListenable<int?> balance,
    });

/// Lists what the pager carries, so a balance can be jumped to instead of
/// swiped to.
///
/// The same pages in the same order — this is the swipe written out as a
/// list, which is what a wallet holding several mints wants once the page it
/// is after is two or three swipes away. Federation-wide, unlike
/// [SelectAccountDrawer], which stays inside the federation in view and is
/// the only place an account without a page can be reached.
class SelectPageDrawer extends StatelessWidget {
  /// Every page the pager carries, in the order it swipes them.
  final List<PageOption> pages;

  /// The page in view, tinted so the list says where the swipe would start
  /// from. Keyed the same way the caller keys its pages.
  final String selectedKey;

  /// How a page is keyed, so [selectedKey] can be matched against a row
  /// without this drawer knowing what identifies a client.
  final String Function(PicoClient) keyOf;

  final void Function(PicoClient) onSelect;

  const SelectPageDrawer({
    super.key,
    required this.pages,
    required this.selectedKey,
    required this.keyOf,
    required this.onSelect,
  });

  static Future<void> show(
    BuildContext context, {
    required List<PageOption> pages,
    required String selectedKey,
    required String Function(PicoClient) keyOf,
    required void Function(PicoClient) onSelect,
  }) {
    return DrawerUtils.show(
      context: context,
      child: SelectPageDrawer(
        pages: pages,
        selectedKey: selectedKey,
        keyOf: keyOf,
        onSelect: onSelect,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return DrawerShell(
      children: [
        // Scrollable: three mints at three accounts each is more rows than a
        // sheet can show, and the list is the whole sheet.
        Flexible(
          child: SingleChildScrollView(
            child: BorderedList.column(
              children: [for (final page in pages) _row(context, page)],
            ),
          ),
        ),
      ],
    );
  }

  Widget _row(BuildContext context, PageOption page) {
    final selected = keyOf(page.client) == selectedKey;

    return ValueListenableBuilder<String?>(
      valueListenable: page.name,
      builder: (context, name, _) {
        return ValueListenableBuilder<int?>(
          valueListenable: page.balance,
          builder: (context, sats, _) {
            return SettingsCard(
              icon: PhosphorIconsRegular.stack,
              iconColor:
                  selected ? Theme.of(context).colorScheme.primary : null,
              // The federation leads, as it does on the row this opens from:
              // across mints it is what tells two pages apart, and the
              // account is the qualifier under it.
              title: name ?? '…',
              subtitle: _subtitle(page.client.accountName(), sats),
              // Every row picks, including the page already in view — landing
              // back where you started is a fair answer to opening the list.
              onTap: () {
                Navigator.of(context).pop();
                onSelect(page.client);
              },
            );
          },
        );
      },
    );
  }

  /// Account and balance on one line. Drops the balance until the first value
  /// lands rather than claiming a zero it hasn't read yet.
  String _subtitle(String account, int? sats) =>
      sats == null
          ? account
          : '$account · ${NumberFormat('#,###').format(sats)} sat';
}
