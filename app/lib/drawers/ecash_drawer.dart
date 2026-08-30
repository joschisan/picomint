import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/amount_rows.dart';
import 'package:pico/widgets/async_button_widget.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/widgets/payment_summary_row_widget.dart';
import 'package:pico/bridge_generated.dart/events.dart';

/// Confirms receiving an out-of-band ecash bundle, into the account named by
/// [destination].
class EcashDrawer extends StatefulWidget {
  /// The account in view when the bundle arrived. Used when it belongs to the
  /// bundle's federation — the balance the user is looking at is the one they
  /// mean — and ignored otherwise, since notes can only be received by the
  /// federation that issued them.
  final PicoClient selected;
  final PicoClientFactory clientFactory;
  final ECashWrapper ecash;

  const EcashDrawer({
    super.key,
    required this.selected,
    required this.clientFactory,
    required this.ecash,
  });

  static Future<bool?> show(
    BuildContext context, {
    required PicoClient selected,
    required PicoClientFactory clientFactory,
    required ECashWrapper ecash,
  }) {
    return DrawerUtils.show<bool>(
      context: context,
      child: EcashDrawer(
        selected: selected,
        clientFactory: clientFactory,
        ecash: ecash,
      ),
    );
  }

  @override
  State<EcashDrawer> createState() => _EcashDrawerState();
}

class _EcashDrawerState extends State<EcashDrawer> {
  // Cached so the lookup doesn't re-fire on every rebuild.
  late final Future<PicoClient?> _destination = _resolveDestination();

  /// Which account the bundle lands in. The selected one when it belongs to
  /// the issuing federation, so scanning while parked on a page pays into the
  /// balance shown on it. Otherwise the bundle names a federation and nothing
  /// more, and the factory answers with its first account — or `null` if the
  /// user isn't joined to it at all.
  Future<PicoClient?> _resolveDestination() async {
    if (widget.selected.federationId() == widget.ecash.federationId()) {
      return widget.selected;
    }

    return widget.clientFactory.client(
      federationId: widget.ecash.federationId(),
    );
  }

  Future<void> _handleReceive() async {
    final destination = await _destination;
    if (destination == null) throw Exception('Mint is unknown');

    await destination.ecashReceive(ecash: widget.ecash);

    if (!mounted) return;

    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    return DrawerShell(
      children: [
        // The list stays the shell's direct child so it bleeds to the sheet
        // edges; the async part sits inside it as one more row.
        BorderedList.column(
          children: [
            const PaymentSummaryRow(
              paymentType: PaymentType.ecash,
              incoming: true,
              status: 'Receive',
            ),
            // The fiat row needs a client for the cached rate, resolved
            // async — until it lands (or if the federation is unknown) only
            // the Bitcoin amount shows.
            FutureBuilder<PicoClient?>(
              future: _destination,
              builder: (context, snapshot) {
                return Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: amountRows(
                    client: snapshot.data,
                    amountSats: widget.ecash.amountSats(),
                  ),
                );
              },
            ),
          ],
        ),
        const SizedBox(height: 16),
        AsyncButton(text: 'Receive', onPressed: _handleReceive),
      ],
    );
  }
}
