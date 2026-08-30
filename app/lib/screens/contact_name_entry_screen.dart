import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/bridge_generated.dart/lnurl.dart';
import 'package:pico/drawers/delete_contact_drawer.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/text_entry_body_widget.dart';
import 'package:share_plus/share_plus.dart';

class ContactNameEntryScreen extends StatefulWidget {
  final PicoClientFactory clientFactory;
  final LnurlWrapper lnurl;
  final String? initialName;
  final Future<void> Function()? onDelete;

  const ContactNameEntryScreen({
    super.key,
    required this.clientFactory,
    required this.lnurl,
    this.initialName,
    this.onDelete,
  });

  @override
  State<ContactNameEntryScreen> createState() => _ContactNameEntryScreenState();
}

class _ContactNameEntryScreenState extends State<ContactNameEntryScreen> {
  late final _controller = TextEditingController(text: widget.initialName);
  final _focusNode = FocusNode();

  @override
  void initState() {
    super.initState();
    _focusNode.requestFocus();
  }

  @override
  void dispose() {
    _controller.dispose();
    _focusNode.dispose();
    super.dispose();
  }

  Future<void> _handleConfirm() async {
    final name = _controller.text.trim();

    if (name.isEmpty) {
      throw 'Please enter a name';
    }

    await widget.clientFactory.saveContact(lnurl: widget.lnurl, name: name);

    if (!mounted) return;

    Navigator.of(context).pop(name);
  }

  void _handleDelete() {
    DeleteContactDrawer.show(
      context,
      name: widget.initialName,
      onDelete: widget.onDelete!,
      onSuccess: () => Navigator.of(context).pop(),
    );
  }

  void _handleShare() {
    SharePlus.instance.share(ShareParams(text: widget.lnurl.encode()));
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      resizeToAvoidBottomInset: true,
      appBar: AppBar(
        title: const Text('Contact Name'),
        actions: [
          // Deleting routes through a confirm drawer — a guard against
          // fat-fingering a contact away.
          if (widget.onDelete != null)
            IconButton(
              icon: const Icon(PhosphorIconsRegular.trash, size: smallIconSize),
              onPressed: _handleDelete,
            ),
          IconButton(
            icon: const Icon(PhosphorIconsRegular.copy, size: smallIconSize),
            onPressed: _handleShare,
          ),
        ],
      ),
      body: TextEntryBody(
        controller: _controller,
        focusNode: _focusNode,
        onConfirm: _handleConfirm,
        keyboardType: TextInputType.name,
        textCapitalization: TextCapitalization.words,
      ),
    );
  }
}
