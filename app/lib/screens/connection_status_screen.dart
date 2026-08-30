import 'package:flutter/material.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/connection_status_header_widget.dart';
import 'package:pico/widgets/icon_chip_widget.dart';
import 'package:pico/widgets/section_header_widget.dart';

/// Per-guardian reachability for one federation. Leaving it lives in the
/// settings drawer alongside the row that opens this screen, so there is no
/// destructive action up here.
class ConnectionStatusScreen extends StatefulWidget {
  final PicoClient client;

  const ConnectionStatusScreen({super.key, required this.client});

  @override
  State<ConnectionStatusScreen> createState() => _ConnectionStatusScreenState();
}

class _ConnectionStatusScreenState extends State<ConnectionStatusScreen> {
  // The same stream the home ring reads — backed by the client's kept-alive
  // connections and emitting the current snapshot first, so dots don't
  // flicker in. Each entry is `(name, rttMs)`: a non-null RTT means that
  // guardian is connected, and carries its round-trip time in milliseconds.
  late final Stream<List<(String, double?)>> _stream =
      widget.client.subscribeConnectionStatus();

  // Resolved once, above the status stream, so status updates don't re-trigger
  // the lookup.
  late final Future<String?> _name = widget.client.federationName();

  // Round-trip time, sampled at connect. Sub-10ms links keep one decimal so
  // a fast guardian doesn't collapse to a misleading "0 ms".
  String _formatRtt(double ms) =>
      '${ms < 10 ? ms.toStringAsFixed(1) : ms.round()} ms';

  @override
  Widget build(BuildContext context) {
    final color = Theme.of(context).colorScheme.primary;

    return Scaffold(
      appBar: AppBar(title: const Text('Connectivity')),
      body: FutureBuilder<String?>(
        future: _name,
        builder: (context, nameSnapshot) {
          final name = nameSnapshot.data ?? 'Mint';

          return StreamBuilder<List<(String, double?)>>(
            stream: _stream,
            builder: (context, snapshot) {
              final statuses = snapshot.data;
              if (statuses == null) {
                return const Center(child: smallSpinner);
              }

              final online = statuses.where((s) => s.$2 != null).length;

              return SingleChildScrollView(
                physics: const BouncingScrollPhysics(),
                padding: const EdgeInsets.fromLTRB(0, 16, 0, 32),
                child: BleedColumn(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const SectionHeader(title: 'Mint'),
                    BorderedList.column(
                      children: [
                        ConnectionStatusHeader(
                          name: name,
                          online: online,
                          total: statuses.length,
                        ),
                      ],
                    ),
                    const SizedBox(height: 16),
                    const SectionHeader(title: 'Guardians'),
                    BorderedList.column(
                      children: [
                        for (final (name, rttMs) in statuses)
                          ListTile(
                            contentPadding: listTilePadding,
                            leading: IconChip(
                              icon: PhosphorIconsRegular.hardDrives,
                              color: rttMs != null ? null : Colors.amber,
                            ),
                            // Stack name/status in the title (not subtitle) to
                            // keep the single-line tile height.
                            title: Column(
                              mainAxisAlignment: MainAxisAlignment.center,
                              crossAxisAlignment: CrossAxisAlignment.start,
                              mainAxisSize: MainAxisSize.min,
                              children: [
                                Text(name, style: mediumStyle),
                                Text(
                                  rttMs != null ? 'Online' : 'Offline',
                                  style: smallStyle.copyWith(
                                    color: rttMs != null ? color : null,
                                  ),
                                ),
                              ],
                            ),
                            // Pico measures the link, so the round-trip time
                            // rides along where conduit shows nothing.
                            trailing:
                                rttMs != null
                                    ? Text(_formatRtt(rttMs), style: smallStyle)
                                    : null,
                          ),
                      ],
                    ),
                  ],
                ),
              );
            },
          );
        },
      ),
    );
  }
}
