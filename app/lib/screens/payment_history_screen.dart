import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:intl/intl.dart';
import 'package:pico/bridge_generated.dart/events.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/amount_visibility.dart';
import 'package:pico/widgets/grouped_list_widget.dart';
import 'package:pico/widgets/payment_card_widget.dart';
import 'package:pico/drawers/payment_details_drawer.dart';
import 'package:pico/utils/payment_utils.dart';

class PaymentHistoryScreen extends StatefulWidget {
  final PicoClientFactory clientFactory;
  final List<OperationSummary> operations;

  const PaymentHistoryScreen({
    super.key,
    required this.clientFactory,
    required this.operations,
  });

  @override
  State<PaymentHistoryScreen> createState() => _PaymentHistoryScreenState();
}

class _PaymentHistoryScreenState extends State<PaymentHistoryScreen> {
  bool _lightning = false;
  bool _bitcoin = false;
  bool _ecash = false;
  bool _incoming = false;
  bool _outgoing = false;
  // Toggles every row between its sats amount and the fiat value snapshotted
  // at payment time. Off by default — rows show sats.
  bool _showFiat = false;

  static String _formatDateHeader(DateTime date) {
    final now = DateTime.now();
    final today = DateTime(now.year, now.month, now.day);
    final dateDay = DateTime(date.year, date.month, date.day);
    final difference = today.difference(dateDay).inDays;

    return switch (difference) {
      0 => 'Today',
      1 => 'Yesterday',
      _ => DateFormat('EEEE d MMMM').format(date),
    };
  }

  bool get _anyType => _lightning || _bitcoin || _ecash;
  bool get _anyDirection => _incoming || _outgoing;

  List<OperationSummary> get _filteredOperations {
    return widget.operations.where((p) {
      if (_anyType) {
        final matchesType = switch (p.paymentType) {
          PaymentType.lightning => _lightning,
          PaymentType.bitcoin => _bitcoin,
          PaymentType.ecash => _ecash,
        };
        if (!matchesType) return false;
      }
      if (_anyDirection) {
        if (p.incoming ? !_incoming : !_outgoing) return false;
      }
      return true;
    }).toList();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Payment History'),
        actions: [
          // Icon previews the unit you'd switch to: $ while showing sats,
          // ₿ while showing fiat.
          IconButton(
            icon: Icon(
              _showFiat
                  ? PhosphorIconsRegular.currencyBtc
                  : PhosphorIconsRegular.currencyDollar,
              size: smallIconSize,
            ),
            onPressed: () => setState(() => _showFiat = !_showFiat),
          ),
        ],
      ),
      body: AmountDisplay(
        display: _showFiat ? BalanceDisplay.fiat : BalanceDisplay.sats,
        child: GroupedList<OperationSummary>(
          items: _filteredOperations,
          groupKey:
              (operation) => _formatDateHeader(
                DateTime.fromMillisecondsSinceEpoch(operation.timestamp),
              ),
          header: Padding(
            padding: const EdgeInsets.fromLTRB(16, 16, 16, 16),
            // Each button gets an equal share of the row, so the five
            // filters can never overflow the screen width.
            child: Row(
              children: [
                Expanded(
                  child: _FilterButton(
                    icon: PhosphorIconsRegular.lightning,
                    active: _lightning,
                    onTap: () => setState(() => _lightning = !_lightning),
                  ),
                ),
                Expanded(
                  child: _FilterButton(
                    icon: PhosphorIconsRegular.link,
                    active: _bitcoin,
                    onTap: () => setState(() => _bitcoin = !_bitcoin),
                  ),
                ),
                Expanded(
                  child: _FilterButton(
                    icon: PhosphorIconsRegular.coinVertical,
                    active: _ecash,
                    onTap: () => setState(() => _ecash = !_ecash),
                  ),
                ),
                Expanded(
                  child: _FilterButton(
                    icon: PaymentTypeUtils.getDirectionIcon(true),
                    active: _incoming,
                    onTap: () => setState(() => _incoming = !_incoming),
                  ),
                ),
                Expanded(
                  child: _FilterButton(
                    icon: PaymentTypeUtils.getDirectionIcon(false),
                    active: _outgoing,
                    onTap: () => setState(() => _outgoing = !_outgoing),
                  ),
                ),
              ],
            ),
          ),
          itemBuilder:
              (context, payment) => PaymentCard(
                key: ValueKey(payment.operationId),
                clientFactory: widget.clientFactory,
                event: payment,
                onTap:
                    () => PaymentDetailsDrawer.show(
                      context,
                      clientFactory: widget.clientFactory,
                      event: payment,
                      display:
                          _showFiat ? BalanceDisplay.fiat : BalanceDisplay.sats,
                    ),
              ),
        ),
      ),
    );
  }
}

class _FilterButton extends StatelessWidget {
  final IconData icon;
  final bool active;
  final VoidCallback onTap;

  const _FilterButton({
    required this.icon,
    required this.active,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    final colorScheme = Theme.of(context).colorScheme;

    // Center so the button hugs its circle instead of stretching to the
    // full width of the Expanded slot.
    return Center(
      child: GestureDetector(
        onTap: () {
          HapticFeedback.lightImpact();
          onTap();
        },
        child: Container(
          width: 48,
          height: 48,
          decoration: BoxDecoration(
            shape: BoxShape.circle,
            // Inactive filters read like the icon chips: the same colour at a
            // 10% tint, rather than a neutral grey.
            color:
                active
                    ? colorScheme.primary
                    : colorScheme.primary.withValues(alpha: 0.1),
          ),
          child: Icon(
            icon,
            size: smallIconSize,
            color: active ? colorScheme.onPrimary : colorScheme.primary,
          ),
        ),
      ),
    );
  }
}
