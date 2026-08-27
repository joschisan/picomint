import 'package:intl/intl.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/currency.dart';
import 'package:pico/bridge_generated.dart/events.dart';

/// Formats a fiat [amount] for the given [currency] without its unit, e.g.
/// `12.50`. An optional [sign] leads the number, e.g. `+12.50`.
String formatFiatNumber(
  FiatCurrency currency,
  double amount, {
  String sign = '',
}) {
  final pattern =
      currency.decimalDigits > 0
          ? '#,##0.${'0' * currency.decimalDigits}'
          : '#,##0';
  return '$sign${NumberFormat(pattern).format(amount)}';
}

/// Formats a fiat [amount] for the given [currency] with a leading symbol,
/// e.g. `$ 12.50`, where the symbol reads as a currency marker rather than a
/// trailing unit.
///
/// The only whole-value fiat form: every surface that shows an amount as
/// money leads with the symbol. The payment cards are the one place that
/// doesn't go through here — they split the number from a lowercase currency
/// code so it sits where `sat` would, via [historicalFiatParts].
String formatFiat(FiatCurrency currency, double amount) =>
    '${currency.symbol} ${formatFiatNumber(currency, amount)}';

/// Converts [amountSats] to the user's fiat currency using the cached exchange
/// rate, without triggering a network fetch. Returns the currency and the
/// converted value, or `null` when no rate has been cached yet. Formatting is
/// the caller's — see [formatFiat].
({FiatCurrency currency, double value})? cachedFiat(
  PicoClient client,
  int amountSats,
) {
  // Read the selected currency live — this path feeds the home screen, which
  // must reflect a currency switched in settings without rebuilding clients.
  final code = client.currencyCode();
  final fiat = client.satsToFiat(amountSats: amountSats, currencyCode: code);
  if (fiat == null) return null;

  return (currency: findFiatCurrency(code: code)!, value: fiat);
}

/// Splits the fiat value captured at payment time (snapshotted on the summary
/// by the Rust recorder) into the formatted number and its lowercase currency
/// code (e.g. `usd`), for the two-line trailing of the payment cards — keeping
/// the unit consistent with `sat`. Returns `null` when no rate was stored for
/// this operation (payments predating the feature, or none cached when they
/// landed). Unlike [cachedFiat] this needs no client — it reads the
/// frozen rate, so history shows each payment's value as of when it happened.
({String number, String unit})? historicalFiatParts(
  OperationSummary event, {
  String sign = '',
}) {
  final amount = event.fiatAmount;
  final code = event.fiatCurrencyCode;
  if (amount == null || code == null) return null;

  final currency = findFiatCurrency(code: code);
  if (currency == null) return null;

  return (
    number: formatFiatNumber(currency, amount, sign: sign),
    unit: currency.code.toLowerCase(),
  );
}
