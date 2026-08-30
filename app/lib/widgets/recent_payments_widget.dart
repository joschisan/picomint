import 'dart:async';

import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/events.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/widgets/animated_entry_widget.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/payment_card_widget.dart';
import 'package:pico/widgets/section_header_widget.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/screens/payment_history_screen.dart';

class RecentPayments extends StatefulWidget implements Bleeds {
  final PicoClientFactory clientFactory;
  final Stream<List<OperationSummary>> stream;
  final void Function(OperationSummary) onTransactionTap;

  const RecentPayments({
    super.key,
    required this.clientFactory,
    required this.stream,
    required this.onTransactionTap,
  });

  @override
  State<RecentPayments> createState() => _RecentPaymentsState();
}

class _RecentPaymentsState extends State<RecentPayments> {
  List<OperationSummary> _payments = [];
  StreamSubscription<List<OperationSummary>>? _subscription;

  /// Payments in the first snapshot render without the entry animation; only
  /// payments appearing after the screen opened grow in.
  Set<String>? _initialIds;

  @override
  void initState() {
    super.initState();
    _subscription = widget.stream.listen(_onSnapshot);
  }

  void _onSnapshot(List<OperationSummary> snapshot) {
    if (!mounted) return;

    setState(() {
      _payments = snapshot.reversed.toList();
      _initialIds ??= _payments.map((p) => p.operationId).toSet();
    });
  }

  @override
  void dispose() {
    _subscription?.cancel();
    super.dispose();
  }

  Future<void> _openHistory() async {
    final operations = await widget.clientFactory.listOperations();

    if (!mounted) return;

    Navigator.of(context).push(
      MaterialPageRoute(
        builder:
            (_) => PaymentHistoryScreen(
              clientFactory: widget.clientFactory,
              operations: operations.reversed.toList(),
            ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    // A BleedColumn so a hosting BleedColumn passes this through uninset:
    // the rows carry their own content padding and must reach the screen
    // edges, while the header and empty-state text take the standard inset.
    if (_payments.isEmpty) {
      return BleedColumn(
        children: [
          const SizedBox(height: 64),
          Text(
            'You have no payments yet.',
            textAlign: TextAlign.center,
            style: smallStyle.copyWith(
              color: Theme.of(context).colorScheme.onSurfaceVariant,
            ),
          ),
        ],
      );
    }

    return BleedColumn(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        SectionHeader(
          title: 'Recent Payments',
          action: 'History',
          onAction: _openHistory,
        ),
        BorderedList.column(
          children: [
            for (var i = 0; i < _payments.length; i++)
              KeyedSubtree(
                key: ValueKey(_payments[i].operationId),
                child: AnimatedEntry(
                  animate: !_initialIds!.contains(_payments[i].operationId),
                  child: PaymentCard(
                    clientFactory: widget.clientFactory,
                    event: _payments[i],
                    onTap: () => widget.onTransactionTap(_payments[i]),
                  ),
                ),
              ),
          ],
        ),
      ],
    );
  }
}
