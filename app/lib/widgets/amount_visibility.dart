import 'package:flutter/widgets.dart';

/// How a monetary amount renders: plain sats, converted to fiat, or masked.
/// Shared by the home balance controls and the history screen's switcher.
enum BalanceDisplay { sats, fiat, hidden }

/// Placeholder rendered in place of a monetary amount when [BalanceDisplay.hidden].
const maskedAmount = '•••••';

/// Broadcasts how amounts should render to descendant balances and payment
/// tiles, without prop-drilling. Defaults to [BalanceDisplay.sats] when no
/// ancestor is present, so widgets reused outside a providing subtree (e.g. a
/// drawer) just show plain sats.
class AmountDisplay extends InheritedWidget {
  final BalanceDisplay display;

  const AmountDisplay({super.key, required this.display, required super.child});

  static BalanceDisplay of(BuildContext context) {
    final widget = context.dependOnInheritedWidgetOfExactType<AmountDisplay>();
    return widget?.display ?? BalanceDisplay.sats;
  }

  @override
  bool updateShouldNotify(AmountDisplay oldWidget) =>
      display != oldWidget.display;
}
