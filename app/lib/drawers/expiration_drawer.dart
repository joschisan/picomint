import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/widgets/balanced_text_widget.dart';

class ExpirationDrawer extends StatelessWidget {
  final PicoClientFactory clientFactory;
  final int date;
  final InviteCodeWrapper? successor;

  const ExpirationDrawer({
    super.key,
    required this.clientFactory,
    required this.date,
    this.successor,
  });

  static Future<void> show(
    BuildContext context, {
    required PicoClientFactory clientFactory,
    required int date,
    InviteCodeWrapper? successor,
  }) {
    return DrawerUtils.show(
      context: context,
      child: ExpirationDrawer(
        clientFactory: clientFactory,
        date: date,
        successor: successor,
      ),
    );
  }

  String _formatDate() {
    return DateFormat.yMMMMd().format(
      DateTime.fromMillisecondsSinceEpoch(date * 1000),
    );
  }

  Future<void> _joinSuccessor(BuildContext context) async {
    await clientFactory.join(invite: successor!);

    if (!context.mounted) return;

    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final formattedDate = _formatDate();

    return DrawerShell(
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16),
          child: BalancedText(
            'This mint will expire on $formattedDate, please migrate your funds before this date.',
            textAlign: TextAlign.center,
            style: smallStyle.copyWith(
              color: Theme.of(context).colorScheme.onSurfaceVariant,
            ),
          ),
        ),

        if (successor != null) ...[
          const SizedBox(height: 16),
          AsyncButton(
            text: 'Add Successor Mint',
            onPressed: () => _joinSuccessor(context),
          ),
        ],
      ],
    );
  }
}
