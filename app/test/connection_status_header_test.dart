import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pico/widgets/connection_status_header_widget.dart';

void main() {
  Future<void> pump(
    WidgetTester tester, {
    required int online,
    required int total,
  }) {
    return tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ConnectionStatusHeader(
            name: 'Federation',
            online: online,
            total: total,
          ),
        ),
      ),
    );
  }

  // picomint sizes federations as 3f + 1 (ALLOWED_FEDERATION_SIZES) and signs
  // at 2f + 1, so these are the thresholds every real federation can hit.
  const thresholds = {4: 3, 7: 5, 10: 7, 13: 9, 16: 11, 19: 13, 22: 15};

  thresholds.forEach((total, threshold) {
    testWidgets('$total guardians: online at $threshold, offline below', (
      tester,
    ) async {
      await pump(tester, online: threshold, total: total);
      expect(
        find.text('Online'),
        findsOneWidget,
        reason: '$threshold of $total meets the 2f+1 signing threshold',
      );

      await pump(tester, online: threshold - 1, total: total);
      expect(
        find.text('Offline'),
        findsOneWidget,
        reason: '${threshold - 1} of $total is below threshold',
      );
    });
  });

  testWidgets('all guardians online reads as online', (tester) async {
    await pump(tester, online: 4, total: 4);
    expect(find.text('Online'), findsOneWidget);
  });

  testWidgets('no guardians online reads as offline', (tester) async {
    await pump(tester, online: 0, total: 4);
    expect(find.text('Offline'), findsOneWidget);
  });
}
