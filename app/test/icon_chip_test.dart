import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pico/widgets/icon_chip_widget.dart';

void main() {
  Future<void> pump(WidgetTester tester, Widget chip) =>
      tester.pumpWidget(MaterialApp(home: Scaffold(body: chip)));

  Color badgeColour(WidgetTester tester) {
    final container = tester.widget<Container>(
      find.descendant(
        of: find.byType(IconChip),
        matching: find.byType(Container),
      ),
    );
    return ((container.decoration as BoxDecoration).color)!;
  }

  Color iconColour(WidgetTester tester) =>
      tester.widget<Icon>(find.byType(Icon)).color!;

  testWidgets('tints the badge and keeps the icon in the given colour', (
    tester,
  ) async {
    await pump(tester, const IconChip(icon: Icons.wallet, color: Colors.green));

    expect(iconColour(tester), Colors.green);
    expect(badgeColour(tester).a, closeTo(0.1, 0.001));
    // Compare channels: withValues yields a plain Color, and Colors.green
    // is a MaterialColor, so the two are never `==` despite matching.
    expect(
      badgeColour(tester).withValues(alpha: 1),
      Colors.green.withValues(alpha: 1),
    );
  });

  testWidgets('falls back to the scheme primary when no colour is given', (
    tester,
  ) async {
    await pump(tester, const IconChip(icon: Icons.wallet));

    final primary =
        Theme.of(tester.element(find.byType(IconChip))).colorScheme.primary;

    expect(iconColour(tester), primary);
    expect(badgeColour(tester).a, closeTo(0.1, 0.001));
  });
}
