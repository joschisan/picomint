import 'package:flutter/material.dart';
import 'package:pico/widgets/bordered_list_widget.dart';

/// Marker for a composite widget that manages its own horizontal padding and
/// so must reach the screen edges inside a [BleedColumn].
///
/// The column can only inspect the widget it is handed, not what that widget
/// builds — a [StatefulWidget] returning a [BleedColumn] still reads as an
/// ordinary child and would be inset on top of its own padding. Implementing
/// this opts it out.
abstract interface class Bleeds {}

/// A column where [BorderedList] children bleed to the full width while every
/// other child keeps a 16px horizontal inset — so list rows reach the screen
/// edges while surrounding content (text, buttons, QR codes) stays padded.
///
/// Flex children ([Expanded]/[Spacer]), nested [BleedColumn]s and anything
/// marked [Bleeds] pass through untouched so they keep working and their own
/// lists keep bleeding.
class BleedColumn extends StatelessWidget {
  final List<Widget> children;
  final CrossAxisAlignment crossAxisAlignment;
  final MainAxisAlignment mainAxisAlignment;
  final MainAxisSize mainAxisSize;

  const BleedColumn({
    super.key,
    required this.children,
    this.crossAxisAlignment = CrossAxisAlignment.center,
    this.mainAxisAlignment = MainAxisAlignment.start,
    this.mainAxisSize = MainAxisSize.max,
  });

  static bool _bleeds(Widget child) =>
      child is BorderedList ||
      child is BleedColumn ||
      child is Bleeds ||
      child is Expanded ||
      child is Spacer;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: crossAxisAlignment,
      mainAxisAlignment: mainAxisAlignment,
      mainAxisSize: mainAxisSize,
      children: [
        for (final child in children)
          _bleeds(child)
              ? child
              : Padding(
                padding: const EdgeInsets.symmetric(horizontal: 16),
                child: child,
              ),
      ],
    );
  }
}
