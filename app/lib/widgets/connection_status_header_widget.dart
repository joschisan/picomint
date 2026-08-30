import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:flutter/material.dart';
import 'package:pico/utils/federation_utils.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/icon_chip_widget.dart';

/// Overall mint reachability shown above the guardian list, as a row that
/// mirrors the guardian rows: the mint's icon-chip badge with its name as the
/// header and an online/offline status beneath.
///
/// The mint is reachable once [federationOperational] holds. The badge carries
/// the primary colour while reachable and turns amber when too few guardians
/// are connected — the same split the mint row on home uses.
class ConnectionStatusHeader extends StatelessWidget {
  final String name;
  final int online;
  final int total;

  const ConnectionStatusHeader({
    super.key,
    required this.name,
    required this.online,
    required this.total,
  });

  @override
  Widget build(BuildContext context) {
    final operational = federationOperational(online: online, total: total);
    final color =
        operational ? Theme.of(context).colorScheme.primary : Colors.amber;

    return ListTile(
      contentPadding: listTilePadding,
      leading: IconChip(icon: PhosphorIconsRegular.stack, color: color),
      title: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(name, style: mediumStyle),
          Text(
            operational ? 'Online' : 'Offline',
            style: smallStyle.copyWith(color: operational ? color : null),
          ),
        ],
      ),
    );
  }
}
