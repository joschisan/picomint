import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pico/utils/styles.dart';

/// One of the round home actions. Every handler either pushes a screen or
/// opens a drawer — nothing here waits on the network — so the button stays
/// synchronous and has no loading state to show.
class CircularActionButton extends StatelessWidget {
  final IconData icon;
  final String label;
  final VoidCallback onTap;

  const CircularActionButton({
    super.key,
    required this.icon,
    required this.label,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Material(
          color: Theme.of(context).colorScheme.primary,
          shape: const CircleBorder(),
          clipBehavior: Clip.antiAlias,
          child: InkWell(
            onTap: () {
              HapticFeedback.lightImpact();
              onTap();
            },
            child: SizedBox(
              width: 64,
              height: 64,
              child: Icon(
                icon,
                size: mediumIconSize,
                color: Theme.of(context).colorScheme.onPrimary,
              ),
            ),
          ),
        ),
        const SizedBox(height: 8),
        Text(
          label,
          style: smallStyle,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
        ),
      ],
    );
  }
}
