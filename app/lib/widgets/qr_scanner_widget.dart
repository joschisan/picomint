import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

/// The camera preview, filling its parent edge-to-edge. The scanner is a
/// full-screen route, so there is no rounded in-sheet variant and no paste
/// control of its own — pasting lives in the home app bar, where it reaches
/// the same input handling without opening the camera.
class QrScannerWidget extends StatefulWidget {
  final void Function(String input) onScan;

  const QrScannerWidget({super.key, required this.onScan});

  @override
  State<QrScannerWidget> createState() => _QrScannerWidgetState();
}

class _QrScannerWidgetState extends State<QrScannerWidget> {
  final _controller = MobileScannerController();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _onDetect(BarcodeCapture capture) {
    if (!mounted) return;
    if (capture.barcodes.isEmpty) return;
    if (capture.barcodes.first.rawValue == null) return;

    widget.onScan(capture.barcodes.first.rawValue!);
  }

  @override
  Widget build(BuildContext context) {
    return MobileScanner(controller: _controller, onDetect: _onDetect);
  }
}
