import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:flutter/material.dart';
import 'package:pico/bridge_generated.dart/events.dart';

class PaymentTypeUtils {
  PaymentTypeUtils._();

  static IconData getIcon(PaymentType type) {
    return switch (type) {
      PaymentType.lightning => PhosphorIconsRegular.lightning,
      PaymentType.bitcoin => PhosphorIconsRegular.link,
      PaymentType.ecash => PhosphorIconsRegular.coinVertical,
    };
  }

  /// Arrow encoding payment direction: incoming points down, outgoing up.
  static IconData getDirectionIcon(bool incoming) =>
      incoming ? PhosphorIconsRegular.arrowDown : PhosphorIconsRegular.arrowUp;

  static String getLabel(PaymentType type) {
    return switch (type) {
      PaymentType.lightning => 'Lightning',
      PaymentType.bitcoin => 'Onchain',
      PaymentType.ecash => 'eCash',
    };
  }
}

/// How long ago [time] was, in the terse form a payment row's second line has
/// room for: `Just now`, `12m`, `3h`, `4d`. Days are the largest unit and go
/// on counting past a week — a coarser one would have to be approximate
/// (months differ in length) and the rows it labels are the old ones nobody
/// scrolls to.
///
/// Hours are counted as elapsed time but days as calendar days, which is how
/// the words are read: something from 20:00 last night is "12h" at breakfast,
/// but anything a full day back lands on the day it happened rather than on a
/// rounded count of 24-hour blocks.
String relativeTime(DateTime time) {
  final now = DateTime.now();
  final elapsed = now.difference(time);

  // Also catches a timestamp slightly in the future, which clock skew between
  // the guardians and the phone can produce.
  if (elapsed.inMinutes < 1) return 'Just now';
  if (elapsed.inHours < 1) return '${elapsed.inMinutes}m';
  if (elapsed.inHours < 24) return '${elapsed.inHours}h';

  final days =
      DateTime(
        now.year,
        now.month,
        now.day,
      ).difference(DateTime(time.year, time.month, time.day)).inDays;

  return '${days}d';
}
