import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:pico/utils/styles.dart';

/// The oversized figure both money screens lead with — the home balance and
/// amount entry — with its unit named beneath.
///
/// The figure scales down to fit the width on one line, so short amounts
/// render at the full [amountStyle] size and long ones shrink rather than
/// wrap — at a constant height either way, so a figure that shrinks doesn't
/// drag the rest of the screen up with it. Keeping that in one widget is what
/// stops the two screens from drifting apart in size, padding, or spacing.
class AmountHeadline extends StatelessWidget {
  final Widget figure;
  // What the figure is denominated in: 'Bitcoin', or the currency's name.
  final String unit;

  const AmountHeadline({super.key, required this.figure, required this.unit});

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.center,
      mainAxisSize: MainAxisSize.min,
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 24),
          child: Stack(
            alignment: Alignment.center,
            children: [
              // Reserves the line a figure occupies at full size, laid out
              // and never painted. The figure only ever scales down, so this
              // pins the headline's height: a long amount shrinks in place
              // instead of shortening the block and pulling everything below
              // it up. Every caller's figure tops out at [amountStyle], so a
              // single digit in that style is the line to reserve.
              const Opacity(opacity: 0, child: Text('0', style: amountStyle)),
              FittedBox(fit: BoxFit.scaleDown, child: figure),
            ],
          ),
        ),
        const SizedBox(height: 8),
        Text(
          unit,
          style: mediumStyle.copyWith(
            color: Theme.of(context).colorScheme.onSurfaceVariant,
          ),
        ),
      ],
    );
  }
}

/// A sats figure for [AmountHeadline]: the number at [amountStyle] with a
/// smaller trailing ` sat`. Takes the digits already formatted, so a masked
/// balance renders through the same widget and keeps the unit in place.
class SatsFigure extends StatelessWidget {
  final String number;

  const SatsFigure(this.number, {super.key});

  SatsFigure.sats(int sats, {super.key})
    : number = NumberFormat('#,###').format(sats);

  @override
  Widget build(BuildContext context) {
    return Text.rich(
      textAlign: TextAlign.center,
      TextSpan(
        children: [
          TextSpan(text: number, style: amountStyle),
          TextSpan(text: ' sat', style: amountUnitStyle),
        ],
      ),
    );
  }
}
