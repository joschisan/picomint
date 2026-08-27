import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';

Widget _list({Key? key}) =>
    BorderedList.column(key: key, children: const [SizedBox(height: 10)]);

/// Stands in for a composite like RecentPayments: a StatefulWidget that builds
/// a BleedColumn and manages its own padding.
class _BleedingSection extends StatefulWidget implements Bleeds {
  const _BleedingSection();

  @override
  State<_BleedingSection> createState() => _BleedingSectionState();
}

class _BleedingSectionState extends State<_BleedingSection> {
  @override
  Widget build(BuildContext context) => BleedColumn(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    mainAxisSize: MainAxisSize.min,
    children: [
      const SizedBox(key: Key('section-header'), height: 10),
      _list(key: const Key('section-rows')),
    ],
  );
}

/// The same shape without the marker, to pin down what the marker changes.
class _UnmarkedSection extends StatelessWidget {
  const _UnmarkedSection();

  @override
  Widget build(BuildContext context) => _list();
}

void main() {
  const screenWidth = 400.0;

  Future<void> pump(WidgetTester tester, List<Widget> children) {
    tester.view.physicalSize = const Size(screenWidth, 800);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    return tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: BleedColumn(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: children,
          ),
        ),
      ),
    );
  }

  testWidgets('ordinary children are inset 16px on both sides', (tester) async {
    await pump(tester, [const SizedBox(key: Key('plain'), height: 10)]);

    final rect = tester.getRect(find.byKey(const Key('plain')));
    expect(rect.left, 16);
    expect(rect.right, screenWidth - 16);
  });

  testWidgets('a BorderedList reaches both edges', (tester) async {
    await pump(tester, [_list(key: const Key('list'))]);

    final rect = tester.getRect(find.byKey(const Key('list')));
    expect(rect.left, 0);
    expect(rect.right, screenWidth);
  });

  testWidgets('a Bleeds-marked composite reaches both edges, and its own rows '
      'do too while its header stays inset', (tester) async {
    await pump(tester, [const _BleedingSection()]);

    final section = tester.getRect(find.byType(_BleedingSection));
    expect(section.left, 0, reason: 'marker must defeat the outer inset');
    expect(section.right, screenWidth);

    // The nested column still does its job: rows bleed, header is inset once.
    final rows = tester.getRect(find.byKey(const Key('section-rows')));
    expect(rows.left, 0);
    expect(rows.right, screenWidth);

    final header = tester.getRect(find.byKey(const Key('section-header')));
    expect(header.left, 16, reason: 'inset exactly once, not twice');
    expect(header.right, screenWidth - 16);
  });

  testWidgets('without the marker the same composite is inset — the bug this '
      'guards against', (tester) async {
    await pump(tester, [const _UnmarkedSection()]);

    final rect = tester.getRect(find.byType(_UnmarkedSection));
    expect(rect.left, 16);
    expect(rect.right, screenWidth - 16);
  });
}
