import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/currency.dart';
import 'package:pico/utils/currency_utils.dart';
import 'package:pico/widgets/amount_headline_widget.dart';
import 'package:pico/widgets/async_button_widget.dart';

class AmountEntryWidget extends StatefulWidget {
  final PicoClient client;
  final Future<void> Function(int amountSats) onConfirm;
  final void Function(int currentAmount)? onAmountChanged;
  final String buttonText;

  const AmountEntryWidget({
    super.key,
    required this.client,
    required this.onConfirm,
    this.onAmountChanged,
    this.buttonText = 'Confirm',
  });

  @override
  State<AmountEntryWidget> createState() => _AmountEntryWidgetState();
}

class _AmountEntryWidgetState extends State<AmountEntryWidget> {
  int _currentAmount = 0;
  bool _enterFiat = false;
  // Snapshot the selected currency once on entry. The user can't change
  // currency from this flow, so a fixed string is correct and spares a db read
  // per keystroke — the home screen, which must track live switches, reads it
  // fresh instead (see `cachedFiatAmount`).
  late final String _currencyCode = widget.client.currencyCode();

  @override
  void initState() {
    super.initState();
    // Refresh the exchange-rate cache on entry so the fiat amount rows on the
    // following confirm/display screen stay populated even if the rate cached
    // at home start has since expired.
    widget.client.prefetchExchangeRates();
  }

  FiatCurrency get _currency {
    return findFiatCurrency(code: _currencyCode)!;
  }

  void _onKeyboardTap(String value) {
    // The amount display auto-shrinks to fit, so up to 10 digits stay on one
    // line — a 10th digit reaches 10 BTC in sats (1,000,000,000 sat).
    if (_currentAmount.toString().length >= 10) return;

    setState(() {
      _currentAmount = _currentAmount * 10 + int.parse(value);
    });

    // Notify parent about amount change (always in sat)
    _notifyParentAmountChanged();
  }

  void _onBackspace() {
    if (_currentAmount > 0) {
      setState(() {
        _currentAmount = _currentAmount ~/ 10;
      });

      // Notify parent about amount change (always in sat)
      _notifyParentAmountChanged();
    }
  }

  void _onClear() {
    setState(() {
      _currentAmount = 0;
    });

    // Notify parent about amount change
    widget.onAmountChanged?.call(0);
  }

  Future<void> _notifyParentAmountChanged() async {
    if (widget.onAmountChanged == null) return;

    if (_enterFiat) {
      final amountSats = await widget.client.fiatToSats(
        amountFiat: _fiatAmount,
        currencyCode: _currencyCode,
      );
      widget.onAmountChanged?.call(amountSats);
    } else {
      // Already in sat
      widget.onAmountChanged?.call(_currentAmount);
    }
  }

  double get _fiatAmount => _currentAmount / pow(10, _currency.decimalDigits);

  String _formatFiatAmount() => formatFiat(_currency, _fiatAmount);

  /// Swaps the unit the figure is read in. The digits stay put and change
  /// meaning — a deliberate reinterpretation, since they are the user's.
  void _toggleCurrency() {
    setState(() {
      _enterFiat = !_enterFiat;
    });

    // Prefetch exchange rates when switching to fiat mode
    if (_enterFiat) {
      widget.client.prefetchExchangeRates();
    }

    // Same digits, different sat value — parent's fee preview would
    // otherwise stay frozen on the old interpretation.
    _notifyParentAmountChanged();
  }

  Future<void> _handleConfirm() async {
    if (_currentAmount == 0) {
      throw 'Please enter an amount';
    }

    final amountSats =
        _enterFiat
            ? await widget.client.fiatToSats(
              amountFiat: _fiatAmount,
              currencyCode: _currencyCode,
            )
            : _currentAmount;

    await widget.onConfirm(amountSats);
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        // Amount display - fills remaining space above confirm button
        Expanded(
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: _toggleCurrency,
            child: Center(
              child: AmountHeadline(
                unit: _enterFiat ? _currency.name : 'Bitcoin',
                figure:
                    _enterFiat
                        ? Text(
                          _formatFiatAmount(),
                          textAlign: TextAlign.center,
                          style: amountStyle,
                        )
                        : SatsFigure.sats(_currentAmount),
              ),
            ),
          ),
        ),

        // Confirm button
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16.0),
          child: AsyncButton(
            text: widget.buttonText,
            onPressed: _handleConfirm,
          ),
        ),

        const SizedBox(height: 16),

        // Custom number pad
        GridView.count(
          crossAxisCount: 3,
          shrinkWrap: true,
          physics: const NeverScrollableScrollPhysics(),
          padding: EdgeInsets.zero,
          mainAxisSpacing: 8,
          crossAxisSpacing: 8,
          childAspectRatio: 2.0,
          children: [
            _buildNumberButton('1'),
            _buildNumberButton('2'),
            _buildNumberButton('3'),
            _buildNumberButton('4'),
            _buildNumberButton('5'),
            _buildNumberButton('6'),
            _buildNumberButton('7'),
            _buildNumberButton('8'),
            _buildNumberButton('9'),
            _buildActionButton(PhosphorIconsRegular.x, _onClear),
            _buildNumberButton('0'),
            _buildActionButton(PhosphorIconsRegular.arrowLeft, _onBackspace),
          ],
        ),
      ],
    );
  }

  Widget _buildNumberButton(String number) {
    return Material(
      color: Colors.transparent,
      borderRadius: cornerRadius,
      child: InkWell(
        borderRadius: cornerRadius,
        onTap: () => _onKeyboardTap(number),
        child: Center(child: Text(number, style: largeStyle)),
      ),
    );
  }

  Widget _buildActionButton(IconData icon, VoidCallback onTap) {
    return Material(
      color: Colors.transparent,
      borderRadius: cornerRadius,
      child: InkWell(
        borderRadius: cornerRadius,
        onTap: onTap,
        child: Center(child: Icon(icon, size: smallIconSize)),
      ),
    );
  }
}
