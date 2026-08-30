import 'package:flutter/material.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:pico/widgets/settings_card_widget.dart';

/// Confirms removing a contact: the row states the action over who it lands
/// on. Same shape as the leave-federation drawer.
class DeleteContactDrawer extends StatefulWidget {
  final String? name;
  final Future<void> Function() onDelete;
  final VoidCallback onSuccess;

  const DeleteContactDrawer({
    super.key,
    required this.name,
    required this.onDelete,
    required this.onSuccess,
  });

  static Future<void> show(
    BuildContext context, {
    required String? name,
    required Future<void> Function() onDelete,
    required VoidCallback onSuccess,
  }) {
    return DrawerUtils.show(
      context: context,
      child: DeleteContactDrawer(
        name: name,
        onDelete: onDelete,
        onSuccess: onSuccess,
      ),
    );
  }

  @override
  State<DeleteContactDrawer> createState() => _DeleteContactDrawerState();
}

class _DeleteContactDrawerState extends State<DeleteContactDrawer> {
  Future<void> _handleDelete() async {
    await widget.onDelete();

    if (!mounted) return;

    Navigator.of(context).pop();
    widget.onSuccess();
  }

  @override
  Widget build(BuildContext context) {
    return DrawerShell(
      children: [
        BorderedList.column(
          children: [
            SettingsCard(
              icon: PhosphorIconsRegular.trash,
              title: 'Remove Contact',
              subtitle: widget.name,
            ),
          ],
        ),
        const SizedBox(height: 16),
        AsyncButton(text: 'Confirm', onPressed: _handleDelete),
      ],
    );
  }
}
