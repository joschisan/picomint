import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:flutter/material.dart';
import 'package:overlay_support/overlay_support.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/icon_chip_widget.dart';

/// Two notifications, both for things the user has to know about: an error and
/// a confirmation. Outcomes the UI already shows — funds arriving, a payment
/// settling — are left to the list rows rather than announced twice.
class NotificationUtils {
  static const _defaultNotificationDuration = Duration(milliseconds: 2500);

  static OverlaySupportEntry _showNotification(
    BuildContext context,
    String title,
    String message,
    IconData icon,
    Color iconColor,
    Duration duration,
  ) {
    return showOverlayNotification(
      (overlayContext) => Material(
        color: Colors.transparent,
        child: Container(
          width: double.infinity,
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 16),
          // One surface for every notification — the chip carries the
          // severity, so the sheet itself doesn't also need tinting.
          decoration: BoxDecoration(
            color: Color.alphaBlend(
              Theme.of(
                overlayContext,
              ).colorScheme.primary.withValues(alpha: 0.05),
              Theme.of(overlayContext).colorScheme.surface,
            ),
            borderRadius: const BorderRadius.vertical(
              bottom: cornerRadiusValue,
            ),
          ),
          child: SafeArea(
            bottom: false,
            child: Row(
              children: [
                IconChip(icon: icon, color: iconColor),
                const SizedBox(width: 16),
                Expanded(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(title, style: mediumStyle),
                      Text(
                        message,
                        style: smallStyle.copyWith(
                          color:
                              Theme.of(
                                overlayContext,
                              ).colorScheme.onSurfaceVariant,
                        ),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
      duration: duration,
      position: NotificationPosition.top,
    );
  }

  static void showError(BuildContext context, String message) {
    _showNotification(
      context,
      'Error',
      message,
      PhosphorIconsRegular.warning,
      Colors.amber,
      _defaultNotificationDuration,
    );
  }

  static void showSuccess(BuildContext context, String message) {
    _showNotification(
      context,
      'Success',
      message,
      PhosphorIconsRegular.checkCircle,
      Colors.green,
      _defaultNotificationDuration,
    );
  }
}
