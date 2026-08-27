import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/utils/currency_utils.dart';
import 'package:pico/widgets/detail_row_widget.dart';

/// Builds the amount rows for a bordered detail list: always an "Amount in
/// Bitcoin" row, followed by an "Amount in `<currency>`" row when a cached
/// exchange rate is available.
///
/// The fiat row is converted from the cached rate without triggering a network
/// fetch, and is omitted entirely (rather than left as an empty cell) when no
/// rate has been cached yet — or when [client] is null (e.g. the issuing
/// federation is unknown), in which case only the Bitcoin row is shown.
List<Widget> amountRows({
  required PicoClient? client,
  required int amountSats,
}) {
  final rows = <Widget>[
    DetailRow(
      icon: PhosphorIconsRegular.currencyBtc,
      label: 'Bitcoin',
      value: '${NumberFormat('#,###').format(amountSats)} sat',
    ),
  ];

  final fiat = client == null ? null : cachedFiat(client, amountSats);
  if (fiat != null) {
    rows.add(
      DetailRow(
        icon: PhosphorIconsRegular.currencyDollar,
        label: fiat.currency.name,
        value: formatFiat(fiat.currency, fiat.value),
      ),
    );
  }

  return rows;
}
