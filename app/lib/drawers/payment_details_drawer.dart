import 'dart:async';

import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:pico/bridge_generated.dart/currency.dart';
import 'package:pico/bridge_generated.dart/events.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/utils/currency_utils.dart';
import 'package:pico/utils/drawer_utils.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/amount_visibility.dart';
import 'package:pico/widgets/drawer_shell_widget.dart';
import 'package:share_plus/share_plus.dart';

class PaymentDetailsDrawer extends StatefulWidget {
  final PicoClientFactory clientFactory;
  final OperationSummary event;
  // How the opening screen was rendering amounts. Passed in (rather than read
  // from an ancestor `AmountDisplay`) because the drawer is a modal route and
  // so sits outside the screen's provider subtree.
  final BalanceDisplay display;

  const PaymentDetailsDrawer({
    super.key,
    required this.clientFactory,
    required this.event,
    required this.display,
  });

  static Future<void> show(
    BuildContext context, {
    required PicoClientFactory clientFactory,
    required OperationSummary event,
    required BalanceDisplay display,
  }) {
    return DrawerUtils.show(
      context: context,
      child: PaymentDetailsDrawer(
        clientFactory: clientFactory,
        event: event,
        display: display,
      ),
    );
  }

  @override
  State<PaymentDetailsDrawer> createState() => _PaymentDetailsDrawerState();
}

class _PaymentDetailsDrawerState extends State<PaymentDetailsDrawer> {
  final List<PaymentEvent> _events = [];
  StreamSubscription<PaymentEvent>? _subscription;

  @override
  void initState() {
    super.initState();
    _subscription = widget.clientFactory
        .subscribePaymentEvents(operationId: widget.event.operationId)
        .listen((e) {
          if (!mounted) return;
          setState(() => _events.add(e));
        });
  }

  @override
  void dispose() {
    _subscription?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    // Just the event timeline. The shell supplies the 16px above and below;
    // the horizontal insets are added here because a Flexible passes through
    // the shell uninset. The left takes only 8px — the dot column is already
    // a margin of its own, so a full 16 would inset the timeline twice.
    return DrawerShell(
      children: [
        if (_events.isNotEmpty)
          Flexible(
            child: SingleChildScrollView(
              padding: const EdgeInsets.only(left: 8, right: 16),
              child: AmountDisplay(
                display: widget.display,
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    for (var i = 0; i < _events.length; i++)
                      _TimelineRow(
                        event: _events[i],
                        summary: widget.event,
                        clientFactory: widget.clientFactory,
                        isLast: i == _events.length - 1,
                        // Milliseconds since the previous event; null for the
                        // first event, which has nothing to measure against.
                        deltaMs:
                            i == 0
                                ? null
                                : (_events[i].timestamp -
                                        _events[i - 1].timestamp)
                                    .toInt(),
                      ),
                  ],
                ),
              ),
            ),
          ),
      ],
    );
  }
}

class _TimelineRow extends StatelessWidget {
  final PaymentEvent event;
  // The owning operation — carries the frozen fiat snapshot used to convert
  // this row's amounts when the fiat toggle is active.
  final OperationSummary summary;
  final PicoClientFactory clientFactory;
  final bool isLast;
  final int? deltaMs;

  const _TimelineRow({
    required this.event,
    required this.summary,
    required this.clientFactory,
    required this.isLast,
    required this.deltaMs,
  });

  @override
  Widget build(BuildContext context) {
    final desc = _describe(event, summary, clientFactory, context);
    final scheme = Theme.of(context).colorScheme;

    return IntrinsicHeight(
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // Dot + connecting line column, sized to largeIconSize so the
          // dots share one vertical axis down the timeline.
          SizedBox(
            width: largeIconSize,
            child: Column(
              children: [
                const SizedBox(height: 4),
                Container(
                  width: 12,
                  height: 12,
                  decoration: BoxDecoration(
                    color: desc.tone,
                    shape: BoxShape.circle,
                  ),
                ),
                if (!isLast)
                  Expanded(
                    child: Container(width: 2, color: scheme.outlineVariant),
                  ),
              ],
            ),
          ),
          const SizedBox(width: 16),

          // Header label + optional subheader. When the description has
          // an `onTap`, the subheader doubles as the tappable action
          // surface (no inline icon — the wording itself signals intent).
          Expanded(
            child: Padding(
              padding: EdgeInsets.only(bottom: isLast ? 0 : 36),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Expanded(child: Text(desc.label, style: mediumStyle)),
                      // Elapsed time since the previous timeline event.
                      if (deltaMs != null)
                        Text(
                          '$deltaMs ms',
                          style: smallStyle.copyWith(
                            color: scheme.onSurfaceVariant,
                          ),
                        ),
                    ],
                  ),
                  if (desc.subtitle != null)
                    GestureDetector(
                      onTap: desc.onTap,
                      behavior: HitTestBehavior.opaque,
                      child: Text(
                        desc.subtitle!,
                        style: smallStyle.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _Description {
  final String label;
  final String? subtitle;
  final Color tone;
  final VoidCallback? onTap;

  const _Description({
    required this.label,
    required this.tone,
    this.subtitle,
    this.onTap,
  });
}

String _sats(int n) => '${NumberFormat('#,###').format(n)} sat';

/// The frozen exchange rate captured on [summary] at payment time, as a BTC
/// price in the snapshotted currency. `null` when the operation carries no
/// snapshot or has no headline amount to derive the rate from — callers then
/// fall back to sats. Derived from `fiatAmount / amountSats` so timeline
/// sub-amounts (fees, change) convert at the same rate as the headline value
/// already shown on the payment card.
({FiatCurrency currency, double btcPrice})? _frozenRate(
  OperationSummary summary,
) {
  final fiat = summary.fiatAmount;
  final code = summary.fiatCurrencyCode;
  final sats = summary.amountSats.toInt();
  if (fiat == null || code == null || sats == 0) return null;

  final currency = findFiatCurrency(code: code);
  if (currency == null) return null;
  return (currency: currency, btcPrice: fiat / (sats / 100000000.0));
}

/// A formatter for the timeline's sat amounts that honors the active
/// [BalanceDisplay]: the frozen-rate fiat value when toggled to fiat (falling
/// back to sats when this operation carries no snapshot), otherwise plain sats.
/// Hidden never reaches here as masking — the drawer shows sats in that case,
/// since the individual amounts aren't sensitive once you've opened a payment.
String Function(int) _amountFormatter(
  BuildContext context,
  OperationSummary summary,
) {
  if (AmountDisplay.of(context) == BalanceDisplay.fiat) {
    final rate = _frozenRate(summary);
    if (rate != null) {
      return (sats) =>
          formatFiat(rate.currency, sats / 100000000.0 * rate.btcPrice);
    }
  }
  return _sats;
}

void _share(String text) {
  SharePlus.instance.share(ShareParams(text: text));
}

_Description _describe(
  PaymentEvent event,
  OperationSummary summary,
  PicoClientFactory clientFactory,
  BuildContext context,
) {
  final scheme = Theme.of(context).colorScheme;
  final neutral = scheme.onSurfaceVariant;
  final success = scheme.primary;
  final failure = Colors.red;
  final warning = Colors.amber.shade700;

  // Renders amounts in sats, fiat, or masked per the active toggle.
  final amount = _amountFormatter(context, summary);

  return switch (event) {
    // ── Core ────────────────────────────────────────────────────────────
    PaymentEvent_TxCreate(:final changeSats, :final feeSats) => _Description(
      label: 'Transaction Created',
      subtitle: '${amount(changeSats.toInt())} · ${amount(feeSats.toInt())}',
      tone: neutral,
    ),
    PaymentEvent_TxAccept() => _Description(
      label: 'Transaction Accepted',
      tone: neutral,
    ),
    PaymentEvent_TxReject() => _Description(
      label: 'Transaction Rejected',
      tone: failure,
    ),

    // ── Lightning ───────────────────────────────────────────────────────
    PaymentEvent_LnSend(:final amountSats, :final feeSats) => _Description(
      label: 'Send Lightning',
      subtitle: '${amount(amountSats.toInt())} · ${amount(feeSats.toInt())}',
      tone: neutral,
    ),
    PaymentEvent_LnSendSuccess(:final preimage) => _Description(
      label: 'Send Success',
      subtitle: 'Tap to share Preimage',
      tone: success,
      onTap: () => _share(preimage),
    ),
    PaymentEvent_LnSendRefund(:final expired) => _Description(
      label: 'Refund',
      subtitle: expired ? 'contract expired' : 'gateway cancelled',
      tone: warning,
    ),
    PaymentEvent_LnSendFailure() => _Description(
      label: 'Send Failure',
      subtitle: 'missing preimage',
      tone: failure,
    ),
    PaymentEvent_LnReceive(:final amountSats, :final feeSats) => _Description(
      label: 'Receive Lightning',
      subtitle: '${amount(amountSats.toInt())} · ${amount(feeSats.toInt())}',
      tone: neutral,
    ),

    // ── Mint (ECash) ────────────────────────────────────────────────────
    PaymentEvent_MintSend(:final amountSats) => _Description(
      label: 'Send eCash',
      subtitle: amount(amountSats.toInt()),
      tone: neutral,
    ),
    PaymentEvent_MintSendSuccess(:final ecash) => _Description(
      label: 'Send Success',
      subtitle: 'Tap to share eCash',
      tone: success,
      onTap: () => _share(ecash),
    ),
    PaymentEvent_MintSendFailure() => _Description(
      label: 'Send Failure',
      tone: failure,
    ),
    PaymentEvent_MintRemint() => _Description(
      label: 'Remint eCash',
      tone: neutral,
    ),
    PaymentEvent_MintReceive(:final amountSats) => _Description(
      label: 'Receive eCash',
      subtitle: amount(amountSats.toInt()),
      tone: neutral,
    ),
    PaymentEvent_MintSuccess(:final amountSats) => _Description(
      label: 'Mint Success',
      subtitle: amount(amountSats.toInt()),
      tone: success,
    ),
    PaymentEvent_MintFailure() => _Description(
      label: 'Mint Failure',
      subtitle: 'threshold signature invalid',
      tone: failure,
    ),

    // ── Wallet (on-chain) ───────────────────────────────────────────────
    PaymentEvent_WalletSend(:final amountSats, :final feeSats) => _Description(
      label: 'Send Onchain',
      subtitle: '${amount(amountSats.toInt())} · ${amount(feeSats.toInt())}',
      tone: neutral,
    ),
    PaymentEvent_WalletSendSuccess(:final txid) => _Description(
      label: 'Send Success',
      subtitle: 'Tap to share txid',
      tone: success,
      onTap: () => _share(txid),
    ),
    PaymentEvent_WalletSendFailure() => _Description(
      label: 'Send Failure',
      subtitle: 'missing txid',
      tone: failure,
    ),
    PaymentEvent_WalletReceive(:final amountSats, :final feeSats) =>
      _Description(
        label: 'Receive Onchain',
        subtitle: '${amount(amountSats.toInt())} · ${amount(feeSats.toInt())}',
        tone: neutral,
      ),
  };
}
