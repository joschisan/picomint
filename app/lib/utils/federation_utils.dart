/// Number of guardians that must be reachable for the federation to sign.
///
/// picomint sizes federations as `3f + 1` and signs at `2f + 1`
/// (picomint-core `NumPeers`), so the threshold falls out of the peer count.
int signingThreshold(int guardians) => 2 * (guardians ~/ 3) + 1;

/// Whether enough guardians are reachable for the federation to operate.
/// Drives the green/amber split shown wherever connectivity surfaces — the
/// federation rows on home and the connection-status header — so the two can
/// never disagree about what "online" means.
bool federationOperational({required int online, required int total}) =>
    total > 0 && online >= signingThreshold(total);
