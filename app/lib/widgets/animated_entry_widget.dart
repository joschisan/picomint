import 'package:flutter/material.dart';

/// Vertically expands its child on mount when `animate` is true; snaps
/// open instantly otherwise. Hosting screens flip an `_initialBuildDone`
/// flag in a post-frame callback so the first batch of items render
/// statically and only later insertions get the entry animation.
class AnimatedEntry extends StatefulWidget {
  final Widget child;
  final bool animate;
  final Duration duration;

  const AnimatedEntry({
    super.key,
    required this.child,
    this.animate = true,
    this.duration = const Duration(milliseconds: 500),
  });

  @override
  State<AnimatedEntry> createState() => _AnimatedEntryState();
}

class _AnimatedEntryState extends State<AnimatedEntry>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    duration: widget.duration,
    vsync: this,
    value: widget.animate ? 0.0 : 1.0,
  );

  late final Animation<double> _animation = CurvedAnimation(
    parent: _controller,
    curve: Curves.easeInOut,
  );

  @override
  void initState() {
    super.initState();
    if (widget.animate) _controller.forward();
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return SizeTransition(sizeFactor: _animation, child: widget.child);
  }
}
