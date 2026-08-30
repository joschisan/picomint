import 'dart:async';

import 'package:flutter/material.dart';
import 'package:pico/utils/styles.dart';
import 'package:intl/intl.dart';
import 'package:pico/bridge_generated.dart/events.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/utils/currency_utils.dart';
import 'package:pico/utils/payment_utils.dart';
import 'package:pico/widgets/amount_visibility.dart';
import 'package:pico/widgets/icon_chip_widget.dart';

enum _Status { ok, warning, error }

_Status? _classify(PaymentEvent event) => switch (event) {
  PaymentEvent_TxReject() => _Status.error,
  PaymentEvent_LnSendRefund() => _Status.warning,
  PaymentEvent_LnSendFailure() => _Status.error,
  PaymentEvent_MintSendFailure() => _Status.error,
  PaymentEvent_MintFailure() => _Status.error,
  PaymentEvent_WalletSendFailure() => _Status.error,
  _ => null,
};

class PaymentCard extends StatefulWidget {
  final PicoClientFactory clientFactory;
  final OperationSummary event;
  final VoidCallback onTap;

  const PaymentCard({
    super.key,
    required this.clientFactory,
    required this.event,
    required this.onTap,
  });

  @override
  State<PaymentCard> createState() => _PaymentCardState();
}

class _PaymentCardState extends State<PaymentCard> {
  _Status _status = _Status.ok;
  StreamSubscription<PaymentEvent>? _subscription;

  DateTime get _time =>
      DateTime.fromMillisecondsSinceEpoch(widget.event.timestamp);

  late String _age = relativeTime(_time);
  Timer? _clock;

  @override
  void initState() {
    super.initState();
    _subscription = widget.clientFactory
        .subscribePaymentEvents(operationId: widget.event.operationId)
        .listen((e) {
          if (!mounted) return;
          final next = _classify(e);
          if (next != null) setState(() => _status = next);
        });
    // A relative label goes stale where the federation name it replaced never
    // could: a settled payment emits no further events, so a row built as
    // "Just now" would still read that an hour on. Half a minute keeps it
    // inside the finest step the label has, and only a step that actually
    // changes the text costs a rebuild.
    _clock = Timer.periodic(const Duration(seconds: 30), (_) {
      final next = relativeTime(_time);
      if (mounted && next != _age) setState(() => _age = next);
    });
  }

  @override
  void dispose() {
    _subscription?.cancel();
    _clock?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final formattedAmount = NumberFormat(
      '#,###',
    ).format(widget.event.amountSats);
    final sign = widget.event.incoming ? '+' : '-';

    // The rail names the row, so the leading chip carries direction instead —
    // otherwise the rail would be stated twice and direction nowhere. Failures
    // tint the chip and the amount rather than swapping the icon out.
    final (iconColor, amountColor) = switch (_status) {
      _Status.error => (Colors.red, Colors.red),
      _Status.warning => (warningColor, warningColor),
      _Status.ok => (null, widget.event.incoming ? scheme.primary : null),
    };

    // Amount and its unit stack in the trailing slot, so the number column
    // lines up down the list regardless of how long the rail names are.
    final fiatParts = historicalFiatParts(widget.event, sign: sign);
    final (amountText, unitText) = switch (AmountDisplay.of(context)) {
      BalanceDisplay.hidden => (maskedAmount, 'sat'),
      // Fall back to sats when no rate was snapshotted for this op.
      BalanceDisplay.fiat when fiatParts != null => (
        fiatParts.number,
        fiatParts.unit,
      ),
      _ => ('$sign$formattedAmount', 'sat'),
    };

    final subColor = scheme.onSurfaceVariant;

    return ListTile(
      onTap: widget.onTap,
      contentPadding: listTilePadding,
      leading: IconChip(
        icon: PaymentTypeUtils.getDirectionIcon(widget.event.incoming),
        color: iconColor,
      ),
      // Rail over age in the title slot (rather than `subtitle`) so the tile
      // keeps the single-line height of the other rows — matching `DetailRow`.
      // When the payment happened places it far better than which mint it went
      // through: a wallet holds two or three mints, so naming one narrows
      // nothing, while the list is ordered by time and every row's neighbours
      // are the rows either side of it in time.
      title: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            PaymentTypeUtils.getLabel(widget.event.paymentType),
            style: mediumStyle,
          ),
          Text(
            _age,
            style: smallStyle.copyWith(color: subColor),
            overflow: TextOverflow.ellipsis,
          ),
        ],
      ),
      trailing: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        crossAxisAlignment: CrossAxisAlignment.end,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(amountText, style: mediumStyle.copyWith(color: amountColor)),
          Text(unitText, style: smallStyle.copyWith(color: subColor)),
        ],
      ),
    );
  }
}
