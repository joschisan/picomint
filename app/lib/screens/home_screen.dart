import 'dart:async';

import 'package:app_links/app_links.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart' hide Notification;
import 'package:flutter/services.dart';
import 'package:phosphor_flutter/phosphor_flutter.dart';
import 'package:pico/bridge_generated.dart/client.dart';
import 'package:pico/bridge_generated.dart/events.dart';
import 'package:pico/bridge_generated.dart/factory.dart';
import 'package:pico/bridge_generated.dart/lib.dart';
import 'package:pico/bridge_generated.dart/lnurl.dart';
import 'package:pico/drawers/ecash_drawer.dart';
import 'package:pico/drawers/lightning_send_drawer.dart';
import 'package:pico/drawers/lnurl_drawer.dart';
import 'package:pico/drawers/invite_drawer.dart';
import 'package:pico/drawers/leave_federation_drawer.dart';
import 'package:pico/drawers/payment_details_drawer.dart';
import 'package:pico/drawers/remove_account_drawer.dart';
import 'package:pico/drawers/scanner_drawer.dart';
import 'package:pico/drawers/select_account_drawer.dart';
import 'package:pico/drawers/settings_drawer.dart';
import 'package:pico/screens/connection_status_screen.dart';
import 'package:pico/screens/display_contacts_screen.dart';
import 'package:pico/screens/ecash_amount_screen.dart';
import 'package:pico/screens/display_lnurl_screen.dart';
import 'package:pico/screens/display_recovery_phrase_screen.dart';
import 'package:pico/screens/lightning_address_entry_screen.dart';
import 'package:pico/screens/select_currency_screen.dart';
import 'package:pico/screens/wallet_v2_receive_screen.dart';
import 'package:pico/utils/account_utils.dart';
import 'package:pico/utils/auth_utils.dart';
import 'package:pico/utils/currency_utils.dart';
import 'package:pico/utils/notification_utils.dart';
import 'package:pico/utils/styles.dart';
import 'package:pico/widgets/amount_visibility.dart';
import 'package:pico/widgets/amount_headline_widget.dart';
import 'package:pico/widgets/animated_balance_widget.dart';
import 'package:pico/widgets/bordered_list_widget.dart';
import 'package:pico/widgets/recent_payments_widget.dart';
import 'package:pico/widgets/bleed_column_widget.dart';
import 'package:pico/widgets/scrollable_body_widget.dart';
import 'package:pico/widgets/circular_action_button_widget.dart';
import 'package:pico/utils/federation_utils.dart';
import 'package:pico/widgets/icon_chip_widget.dart';
import 'package:pico/screens/onchain_amount_screen.dart';

/// Identifies one entry in the factory's client map. A federation id alone
/// no longer does: a federation contributes one client per account, and the
/// pager swipes through all of them.
String _clientKey(PicoClient client) =>
    '${client.federationId()}/${client.accountName()}';

/// Multimint home: the balance and the row naming its account form one
/// swipeable page, so every balance is paged through rather than picked from
/// a list. Each mint has three accounts — the federation cannot tell them
/// apart; they exist only to keep money in separate piles — but only the ones
/// worth showing get pages, so a swipe crosses whichever accounts are in use
/// and then on to the next mint. The page you land on is the balance every
/// action spends from. The leading gear opens the settings drawer, which is
/// where the account picker and both removals live. The picomint
/// eventlog is daemon-wide so recent ops and notifications come from a single
/// factory-level stream — no per-client merging needed.
///
/// Always has a federation: it is only ever mounted with one — from startup
/// or from [OnboardingScreen] — and the last one can't be left, so nothing
/// here renders an empty state.
class HomeScreen extends StatefulWidget {
  final PicoClientFactory clientFactory;
  // The federations already joined at mount, so the first frame has one to
  // render instead of waiting on this screen's own first emission.
  final List<PicoClient> initialClients;

  const HomeScreen({
    super.key,
    required this.clientFactory,
    required this.initialClients,
  });

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  late final Stream<List<OperationSummary>> _recentStream;
  late final AppLinks _appLinks;
  StreamSubscription<Uri>? _linkSubscription;
  StreamSubscription<Notification>? _notificationSubscription;
  StreamSubscription<List<PicoClient>>? _clientsSubscription;

  // Never empty: seeded from the mount-time list and only ever replaced by a
  // non-empty emission. Every account of every joined federation, shown or
  // not — [_visible] is the subset with pages.
  late List<PicoClient> _clients = widget.initialClients;
  // The accounts the pager carries, in [_clients] order. Derived state, kept
  // in a field rather than recomputed in `build` because the pager indexes
  // into it: the selection is a position in this list, so it has to hold
  // still between the frame that changes it and the jump that follows.
  late List<PicoClient> _visible = _computeVisible();
  // Accounts opened from the picker this session. Deliberately not persisted:
  // an empty account you opened once would otherwise keep its page for good,
  // and anything you actually put money into keeps its page on its own.
  final Set<String> _opened = {};
  // Federations seen in the last emission, so the next one can tell an
  // arrival from a list that merely re-rendered.
  late Set<String> _knownFederationIds =
      _clients.map((c) => c.federationId()).toSet();
  // Holds the selection, not just the scroll: the account in view is the one
  // every action routes through, so this controller is the only place it is
  // recorded. Starts on the first federation's first account.
  final PageController _pageController = PageController();
  // One session per account, mirroring the factory's
  // `BTreeMap<(FederationId, Account), PicoClient>`: this is where a client's
  // two streams are listened to, so everything below is a pure function of
  // the values they carry. Keyed by [_clientKey] and kept in step with
  // [_clients] by [_syncSessions], so a client in that list always has an
  // entry here.
  final Map<String, _FederationSession> _sessions = {};
  // Single cycling control over how balances read: sats → fiat → hidden.
  BalanceDisplay _balanceDisplay = BalanceDisplay.sats;

  // Whether a cached exchange rate exists, so the fiat step is reachable.
  // The rate is global, so any client answers; `satsToFiat` is a cache-only
  // sync read returning null when no fresh rate is stored.
  bool get _fiatAvailable =>
      _clients.first.satsToFiat(
        amountSats: 0,
        currencyCode: _clients.first.currencyCode(),
      ) !=
      null;

  void _cycleBalanceDisplay() {
    setState(() {
      // Skip the fiat step entirely when no rate is cached.
      _balanceDisplay = switch (_balanceDisplay) {
        BalanceDisplay.sats =>
          _fiatAvailable ? BalanceDisplay.fiat : BalanceDisplay.hidden,
        BalanceDisplay.fiat => BalanceDisplay.hidden,
        BalanceDisplay.hidden => BalanceDisplay.sats,
      };
    });
  }

  @override
  void initState() {
    super.initState();
    _syncSessions(_clients);
    _recentStream = widget.clientFactory.subscribeRecentOperations();
    _notificationSubscription = widget.clientFactory
        .subscribeNotifications()
        .listen(_handleNotification);
    _clientsSubscription = widget.clientFactory.subscribeClients().listen((
      clients,
    ) {
      if (!mounted) return;
      // Unreachable while the last federation can't be left, and kept so a
      // future path to an empty wallet degrades to a stale row rather than a
      // crash on `_clients.first`.
      if (clients.isEmpty) return;
      setState(() {
        _clients = clients;
        final ids = clients.map((c) => c.federationId()).toSet();
        // A federation that wasn't in the previous list was just joined, so
        // page to it — the user acted on it, and a join that restored funds
        // arrives with them already on its balances. Its first page is its
        // first account, since the factory orders by federation then account.
        // A federation that left needs no counterpart: the pager clamps onto
        // a page that still exists, and whatever it lands on is the
        // selection.
        final arrived = ids.difference(_knownFederationIds);
        _knownFederationIds = ids;

        _syncSessions(clients);
        // A federation that left takes its accounts with it, so a re-join
        // later in the same session starts from a fresh primary.
        final live = clients.map(_clientKey).toSet();
        _opened.removeWhere((key) => !live.contains(key));
        _visible = _computeVisible();

        if (arrived.isNotEmpty) {
          _pageTo(
            _visible.indexWhere((c) => arrived.contains(c.federationId())),
          );
        }
      });
      // Warm each federation's exchange-rate cache so the fiat balance toggle
      // and the send/receive fiat rows render from cache without blocking.
      // Repaint once the rates land so a balance shown in fiat updates from
      // its sats fallback without waiting for the next balance change.
      Future.wait(clients.map((c) => c.prefetchExchangeRates())).then((_) {
        if (mounted) setState(() {});
      });
    });
    _initDeepLinks();
  }

  @override
  void dispose() {
    _linkSubscription?.cancel();
    _notificationSubscription?.cancel();
    _clientsSubscription?.cancel();
    _pageController.dispose();
    for (final session in _sessions.values) {
      session.dispose();
    }
    super.dispose();
  }

  void _handleNotification(Notification notification) {
    if (!mounted) return;
    switch (notification) {
      // Arriving funds already announce themselves: the payment row grows in
      // and the balance counts up. A haptic is the only extra signal — no
      // toast for an outcome the screen is showing.
      case Notification_LightningReceived():
      case Notification_OnchainReceived():
        HapticFeedback.heavyImpact();
      case Notification_LightningRefunding():
        HapticFeedback.heavyImpact();
        NotificationUtils.showError(context, 'Lightning payment refunded');
      case Notification_TransactionRejected():
        HapticFeedback.heavyImpact();
        NotificationUtils.showError(context, 'Transaction rejected');
    }
  }

  void _initDeepLinks() {
    _appLinks = AppLinks();
    _linkSubscription = _appLinks.uriLinkStream.listen(_handleDeepLink);
    _appLinks.getInitialLink().then((uri) {
      if (uri != null) _handleDeepLink(uri);
    });
  }

  void _handleDeepLink(Uri uri) => _handleInput(uri.toString());

  /// Routes a pasted or deep-linked string to the matching flow, the same job
  /// the scanner does for a camera frame. Returns whether anything recognised
  /// it, so the caller can say so when nothing did.
  bool _handleInput(String input) {
    final client = _selectedClient();

    final parsers = [
      (
        // Invite codes route to the join drawer, which owns the whole
        // lifecycle — pasting an invite is a first-class way to join.
        parseInviteCode(invite: input),
        (dynamic result) => InviteDrawer.show(
          context,
          invite: result,
          clientFactory: widget.clientFactory,
        ),
      ),
      (
        parseBolt11Invoice(invoice: input),
        (dynamic result) =>
            LightningSendDrawer.show(context, client: client, invoice: result),
      ),
      (
        parseEcash(ecash: input),
        (dynamic result) => EcashDrawer.show(
          context,
          selected: client,
          clientFactory: widget.clientFactory,
          ecash: result,
        ),
      ),
      (
        parseBitcoinAddress(address: input),
        // An address carries no amount, so there is nothing to confirm before
        // asking for one — go straight to the amount entry.
        (dynamic result) => Navigator.of(context).push(
          MaterialPageRoute(
            builder:
                (_) => OnchainAmountScreen(
                  client: client,
                  clientFactory: widget.clientFactory,
                  address: result,
                ),
          ),
        ),
      ),
      (
        parseLnurl(request: input),
        (dynamic result) => LnurlDrawer.show(
          context,
          client: client,
          clientFactory: widget.clientFactory,
          lnurl: result,
        ),
      ),
    ];

    for (final (result, showDrawer) in parsers) {
      if (result != null) {
        showDrawer(result);
        return true;
      }
    }

    return false;
  }

  /// The app-bar counterpart to the scanner: same inputs, same flows, without
  /// opening the camera for a code that is already on the clipboard.
  Future<void> _handlePaste() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);

    if (!mounted) return;

    final input = data?.text?.trim();

    if (input == null || input.isEmpty) {
      NotificationUtils.showError(context, 'Clipboard is empty');
      return;
    }

    if (!_handleInput(input)) {
      NotificationUtils.showError(context, 'Unrecognized clipboard content');
    }
  }

  /// The account in view — a federation and one of its three balances — and
  /// so the client every action routes through.
  ///
  /// Read from the pager rather than mirrored into state. A selection kept in
  /// both places has to be reconciled every time either moves — and the pager
  /// moves on its own, clamping itself when the list shrinks under it, so the
  /// mirror is the copy that goes stale. Nothing needs the selection at build
  /// time except the dots, which watch the same controller, so there is
  /// nothing to rebuild for and no `setState` on a swipe.
  ///
  /// Rounds mid-drag to the page the swipe is committing to, which is the one
  /// the user is looking at. Before the first layout there is no position to
  /// read and the pager sits on its initial page; the clamp covers the frame
  /// between a federation leaving — taking three pages with it — and the
  /// pager laying out over the shorter list.
  PicoClient _selectedClient() {
    final page = _pageController.hasClients ? _pageController.page : null;
    final index = (page ?? _pageController.initialPage.toDouble()).round();

    return _visible[index.clamp(0, _visible.length - 1)];
  }

  /// Moves the pager onto [index], deferred a frame so it has rebuilt with
  /// the new page count first. A jump rather than an animation: the list
  /// changed while the user was on another screen, so there is no swipe to
  /// finish.
  void _pageTo(int index) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || !_pageController.hasClients) return;
      _pageController.jumpToPage(index);
    });
  }

  /// Brings [_sessions] into step with the client set: an account that
  /// arrived gets its streams opened, one that went gets them closed. A join
  /// brings three at once and a leave takes three, since a federation's
  /// accounts arrive and depart together. Called inside the same `setState`
  /// that swaps [_clients], so the two never disagree by the time anything
  /// builds.
  void _syncSessions(List<PicoClient> clients) {
    for (final client in clients) {
      final key = _clientKey(client);
      if (_sessions.containsKey(key)) continue;

      final session = _FederationSession(client);
      // An account holding money always has a page, so its balance is an
      // input to [_visible] and not only to what the page renders. The
      // listener goes with the session when it is disposed.
      session.balance.addListener(_refreshVisible);
      _sessions[key] = session;
    }

    final keys = clients.map(_clientKey).toSet();
    for (final key in _sessions.keys.toList()) {
      if (!keys.contains(key)) _sessions.remove(key)!.dispose();
    }
  }

  /// Whether [client]'s account gets a page.
  ///
  /// Three accounts each is more pages than a wallet with two mints wants to
  /// swipe through when it is using one of them, so an account earns its page
  /// rather than being given one. Primary always has one — it is where a join
  /// lands and where a removal transfers to. The rest are there because they
  /// were opened from the picker, or because they hold money: a balance is
  /// never hidden, which is what makes a restore that finds funds in an
  /// account nobody ever opened surface them instead of swallowing them.
  bool _isVisible(PicoClient client) {
    if (client.accountName() == primaryAccount) return true;

    if (_opened.contains(_clientKey(client))) return true;

    return (_sessions[_clientKey(client)]?.balance.value ?? 0) > 0;
  }

  List<PicoClient> _computeVisible() {
    final visible = _clients.where(_isVisible).toList();

    // Unreachable — every federation contributes a primary — and kept so a
    // pager with no pages can never be built.
    return visible.isEmpty ? _clients : visible;
  }

  /// Recomputes which accounts have pages, rebuilding only when the answer
  /// changed. Called whenever one of its three inputs moves: the client set,
  /// the stored set, and any account's balance.
  ///
  /// Holds the selection across the change. The pager indexes into
  /// [_visible], so a page appearing ahead of the current one would otherwise
  /// slide the selection sideways under a user who did nothing — and the
  /// selection is what every action spends from.
  void _refreshVisible() {
    final next = _computeVisible();
    final keys = next.map(_clientKey).toList();

    if (listEquals(keys, _visible.map(_clientKey).toList())) return;

    final selected = _clientKey(_selectedClient());

    setState(() => _visible = next);

    final index = keys.indexOf(selected);

    // The page in view is the one that just went, which only happens to the
    // account being removed. Its balance is now primary's, so that is where
    // the pager belongs.
    _pageTo(index >= 0 ? index : _primaryIndexOf(selected, next));
  }

  /// Where [key]'s federation keeps its primary in [clients].
  int _primaryIndexOf(String key, List<PicoClient> clients) {
    final federationId = key.split('/').first;

    final index = clients.indexWhere(
      (c) =>
          c.federationId() == federationId && c.accountName() == primaryAccount,
    );

    return index < 0 ? 0 : index;
  }

  /// Receive Lightning leads with the reusable code. The lnurl is read from
  /// the mint's locally mirrored gateway set, so this needs no round trip.
  void _onReceiveLightning() {
    final client = _selectedClient();

    Navigator.of(context).push(
      MaterialPageRoute(
        builder:
            (_) => DisplayLnurlScreen(
              client: client,
              clientFactory: widget.clientFactory,
              lnurl: client.lnurl(),
              currencyCode: client.currencyCode(),
            ),
      ),
    );
  }

  void _onSendEcash() {
    final client = _selectedClient();
    Navigator.of(context).push(
      MaterialPageRoute(
        builder:
            (_) => EcashAmountScreen(
              client: client,
              clientFactory: widget.clientFactory,
            ),
      ),
    );
  }

  void _onReceiveBitcoin() {
    final client = _selectedClient();
    Navigator.of(context).push(
      MaterialPageRoute(
        builder:
            (_) => WalletV2ReceiveScreen(
              client: client,
              clientFactory: widget.clientFactory,
            ),
      ),
    );
  }

  void _onLightningAddress() {
    final client = _selectedClient();
    Navigator.of(context).push(
      MaterialPageRoute(
        builder:
            (_) => LightningAddressEntryScreen(
              client: client,
              clientFactory: widget.clientFactory,
            ),
      ),
    );
  }

  void _onContacts() {
    final client = _selectedClient();
    Navigator.of(context).push(
      MaterialPageRoute(
        builder:
            (_) => DisplayContactsScreen(
              client: client,
              clientFactory: widget.clientFactory,
            ),
      ),
    );
  }

  void _onScan() {
    ScannerDrawer.show(
      context,
      client: _selectedClient(),
      clientFactory: widget.clientFactory,
    );
  }

  void _onSettings() {
    SettingsDrawer.show(
      context,
      client: _selectedClient(),
      clientFactory: widget.clientFactory,
      onSelectRecoveryPhrase: _openRecoveryPhrase,
      onSelectCurrency: _openCurrency,
      onSelectAccount: _openSelectAccount,
      onSelectConnectivity: _openConnectivity,
      // Primary is where a removal moves the balance, so it has nowhere to go
      // and the row is left out on it.
      onSelectRemoveAccount:
          _selectedClient().accountName() == primaryAccount
              ? null
              : _openRemoveAccount,
      // Leaving the last federation would strand the wallet on onboarding, so
      // the row only appears once there is another to fall back to.
      onSelectLeave: _knownFederationIds.length > 1 ? _openLeave : null,
    );
  }

  /// Lists the selected federation's accounts, including the ones without
  /// pages — this is the only place they can be reached from.
  void _openSelectAccount() {
    final federationId = _selectedClient().federationId();

    SelectAccountDrawer.show(
      context,
      accounts: [
        for (final client in _clients)
          if (client.federationId() == federationId)
            (client: client, balance: _sessions[_clientKey(client)]!.balance),
      ],
      onSelect: _selectAccount,
    );
  }

  /// Gives [account] a page if it hasn't got one and swipes to it. Choosing
  /// an account is how an empty one is brought out — it holds its page for
  /// as long as the app is up, and past that only if it has a balance.
  void _selectAccount(PicoClient account) {
    _opened.add(_clientKey(account));

    _refreshVisible();

    final index = _visible.indexWhere(
      (c) => _clientKey(c) == _clientKey(account),
    );

    if (index >= 0) _pageTo(index);
  }

  void _openRemoveAccount() {
    final account = _selectedClient();

    RemoveAccountDrawer.show(
      context,
      account: account,
      balance: _sessions[_clientKey(account)]!.balance,
      // The transfer leaves the account empty, so forgetting it was opened is
      // all that is left — and the balance falling to zero would have taken
      // its page anyway had it never been opened.
      onSuccess: () => _hideAccount(account),
    );
  }

  void _hideAccount(PicoClient account) {
    _opened.remove(_clientKey(account));

    _refreshVisible();
  }

  Future<void> _openRecoveryPhrase() async {
    try {
      await requireBiometricAuth(context);

      if (!mounted) return;

      final seedPhrase = await widget.clientFactory.seedPhrase();

      if (!mounted) return;

      Navigator.of(context).push(
        MaterialPageRoute(
          builder: (_) => DisplayRecoveryPhraseScreen(seedPhrase: seedPhrase),
        ),
      );
    } catch (e) {
      if (mounted) NotificationUtils.showError(context, e.toString());
    }
  }

  Future<void> _openCurrency() async {
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder:
            (_) => SelectCurrencyScreen(clientFactory: widget.clientFactory),
      ),
    );
    // The currency may have changed; the rate map already covers every
    // currency, so a plain repaint reprices the balance/rows off the cache —
    // no refetch needed.
    if (mounted) setState(() {});
  }

  void _openConnectivity() {
    Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => ConnectionStatusScreen(client: _selectedClient()),
      ),
    );
  }

  void _openLeave() {
    LeaveFederationDrawer.show(
      context,
      client: _selectedClient(),
      clientFactory: widget.clientFactory,
      // Nothing to route: the factory's client stream drives the selection, so
      // the row and every action follow the remaining federations on their own.
      onSuccess: () {},
    );
  }

  void _showEventDetails(OperationSummary event) {
    PaymentDetailsDrawer.show(
      context,
      clientFactory: widget.clientFactory,
      event: event,
      display: _balanceDisplay,
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        // Settings leads, so the actions on the right are all things to do
        // with a payment: paste one, address one, pick who it's for.
        leading: IconButton(
          icon: const Icon(PhosphorIconsRegular.gearSix, size: smallIconSize),
          onPressed: _onSettings,
        ),
        // The balance display is cycled by tapping the balance itself.
        actions: [
          IconButton(
            icon: const Icon(
              PhosphorIconsRegular.clipboardText,
              size: smallIconSize,
            ),
            onPressed: _handlePaste,
          ),
          IconButton(
            icon: const Icon(PhosphorIconsRegular.at, size: smallIconSize),
            onPressed: _onLightningAddress,
          ),
          IconButton(
            icon: const Icon(PhosphorIconsRegular.users, size: smallIconSize),
            onPressed: _onContacts,
          ),
        ],
      ),
      body: AmountDisplay(
        display: _balanceDisplay,
        child: ScrollableBody(
          child: Padding(
            padding: const EdgeInsets.symmetric(vertical: 16),
            child: BleedColumn(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                // Leads the block: the only affordance that it swipes, and
                // above the balance it reads as a header for what follows
                // rather than a footnote under the account row. Left out with
                // a single account on a single mint, where it would promise a
                // swipe that goes nowhere.
                if (_visible.length > 1)
                  _PageDots(
                    count: _visible.length,
                    controller: _pageController,
                  ),
                _FederationPager(
                  clients: _visible,
                  sessions: _sessions,
                  controller: _pageController,
                  display: _balanceDisplay,
                  onBalanceTap: _cycleBalanceDisplay,
                ),
                const SizedBox(height: 16),
                Container(
                  padding: const EdgeInsets.symmetric(vertical: 16),
                  decoration: BoxDecoration(
                    color: Theme.of(
                      context,
                    ).colorScheme.primary.withValues(alpha: 0.05),
                    borderRadius: cornerRadius,
                  ),
                  // Each button gets an equal share of the row, so the
                  // four actions can never overflow the screen width.
                  child: Row(
                    children: [
                      Expanded(
                        child: CircularActionButton(
                          icon: PhosphorIconsRegular.lightning,
                          label: 'Lightning',
                          onTap: _onReceiveLightning,
                        ),
                      ),
                      Expanded(
                        child: CircularActionButton(
                          icon: PhosphorIconsRegular.link,
                          label: 'Onchain',
                          onTap: _onReceiveBitcoin,
                        ),
                      ),
                      Expanded(
                        child: CircularActionButton(
                          icon: PhosphorIconsRegular.coinVertical,
                          label: 'eCash',
                          onTap: _onSendEcash,
                        ),
                      ),
                      Expanded(
                        child: CircularActionButton(
                          icon: PhosphorIconsRegular.qrCode,
                          label: 'Scan',
                          onTap: _onScan,
                        ),
                      ),
                    ],
                  ),
                ),
                const SizedBox(height: 16),
                RecentPayments(
                  clientFactory: widget.clientFactory,
                  stream: _recentStream,
                  onTransactionTap: _showEventDetails,
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// The UI's half of one client: its two streams, listened to once and held
/// open for as long as that account's federation is joined, plus the name
/// they belong to. Held by [_HomeScreenState._sessions], one per entry in the
/// factory's client map — so three per federation, each carrying its own
/// account's balance and all three carrying the same connectivity.
///
/// The bridge hands back a single-subscription stream per call, so a stream
/// passed down the tree would throw the second time a widget listened to it.
/// Listening here instead turns each into a value that any number of widgets
/// can read, rebuild from, and stop reading without consequence — which is
/// what lets every page below be stateless.
class _FederationSession {
  // Null until the first value lands: an unresolved balance is not a zero
  // one, and no status yet is not "offline".
  final ValueNotifier<int?> balance = ValueNotifier(null);
  // Each entry is `(name, rttMs)`: a non-null RTT means that guardian is
  // connected. The stream replays its current snapshot, so this fills in on
  // the first frame rather than after a round trip.
  final ValueNotifier<List<(String, double?)>?> connection = ValueNotifier(
    null,
  );
  final ValueNotifier<String?> name = ValueNotifier(null);

  late final StreamSubscription<int> _balanceSubscription;
  late final StreamSubscription<List<(String, double?)>>
  _connectionSubscription;
  bool _disposed = false;

  _FederationSession(PicoClient client) {
    _balanceSubscription = client.subscribeBalance().listen(
      (sats) => balance.value = sats,
    );
    _connectionSubscription = client.subscribeConnectionStatus().listen(
      (statuses) => connection.value = statuses,
    );
    // The one value that resolves rather than streaming. Guarded because a
    // federation can be left before its name comes back.
    client.federationName().then((value) {
      if (!_disposed) name.value = value;
    });
  }

  void dispose() {
    _disposed = true;
    _balanceSubscription.cancel();
    _connectionSubscription.cancel();
    balance.dispose();
    connection.dispose();
    name.dispose();
  }
}

/// One account's balance, shown above the row naming it: masked when hidden,
/// fiat when toggled (falling back to sats until a rate is cached), otherwise
/// the animated sats amount.
class _BalanceHero extends StatelessWidget {
  final ValueListenable<int?> balance;
  final BalanceDisplay display;
  final PicoClient rateClient;

  const _BalanceHero({
    required this.balance,
    required this.display,
    required this.rateClient,
  });

  @override
  Widget build(BuildContext context) {
    final hidden = display == BalanceDisplay.hidden;
    return Padding(
      padding: const EdgeInsets.only(bottom: 32),
      child: ValueListenableBuilder<int?>(
        valueListenable: balance,
        builder: (context, sats, _) {
          // Fiat when toggled and a rate is cached; otherwise the sats
          // display (and a "Bitcoin" unit label to match).
          final fiat =
              (!hidden && display == BalanceDisplay.fiat)
                  ? cachedFiat(rateClient, sats ?? 0)
                  : null;

          return AmountHeadline(
            unit: fiat?.currency.name ?? 'Bitcoin',
            figure: switch ((hidden, fiat)) {
              // Mask the amount in place, keeping the " sat" suffix.
              (true, _) => const SatsFigure(maskedAmount),
              (false, final fiat?) => AnimatedBalance(
                sats: sats,
                style: amountStyle,
                textAlign: TextAlign.center,
                // Convert each tweened sats value to fiat so it counts up on
                // the same tween as the sats view.
                formatter: (s) {
                  final tweened = cachedFiat(rateClient, s);
                  return tweened == null
                      ? ''
                      : formatFiat(fiat.currency, tweened.value);
                },
              ),
              (false, null) => AnimatedBalance(
                sats: sats,
                style: amountStyle,
                unitStyle: amountUnitStyle,
                textAlign: TextAlign.center,
              ),
            },
          );
        },
      ),
    );
  }
}

/// Names the account its page belongs to: a wallet chip, the federation name
/// as the header and the account beneath. The balance lives in the hero above.
///
/// The account reads here because it is the half of the selection the
/// federation name doesn't state — swiping within a federation changes only
/// this line, and it is the only thing distinguishing three pages that
/// otherwise look alike.
///
/// Connectivity keeps the chip: it tints amber while too few guardians are
/// reachable to sign, so a degraded federation is still flagged on the row it
/// belongs to rather than from an app-bar icon. The detail behind it is a tap
/// away on the connection screen.
///
/// Inert: the pager selects, so there is nothing here to tap.
class _FederationRow extends StatelessWidget {
  final ValueListenable<String?> name;
  final String account;
  final ValueListenable<List<(String, double?)>?> connection;

  const _FederationRow({
    required this.name,
    required this.account,
    required this.connection,
  });

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;

    return ValueListenableBuilder<List<(String, double?)>?>(
      valueListenable: connection,
      builder: (context, statuses, _) {
        final operational =
            statuses != null &&
            federationOperational(
              online: statuses.where((s) => s.$2 != null).length,
              total: statuses.length,
            );

        return ListTile(
          contentPadding: listTilePadding,
          leading: IconChip(
            icon: PhosphorIconsRegular.stack,
            // Untinted until the first status lands, so amber only ever means
            // "too few guardians to sign".
            color:
                statuses == null ? null : (operational ? null : Colors.amber),
          ),
          // Both texts go in the title slot so ListTile sees a single-line
          // tile (56dp min) instead of the 72dp two-line tile a populated
          // `subtitle` would force. Keeps the row height consistent with
          // the other rows in the app.
          title: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              ValueListenableBuilder<String?>(
                valueListenable: name,
                builder:
                    (_, value, _) => Text(
                      value ?? '\u2026',
                      style: mediumStyle,
                      overflow: TextOverflow.ellipsis,
                    ),
              ),
              Text(
                // Fixed at construction, so unlike the name and the chip
                // above it there is nothing to wait for.
                account,
                style: smallStyle.copyWith(color: scheme.onSurfaceVariant),
              ),
            ],
          ),
        );
      },
    );
  }
}

/// One account's page: its balance over the row naming it, both read from
/// that account's [_FederationSession]. Stateless, so the pager is free to
/// build and drop pages as they scroll.
class _FederationPage extends StatelessWidget {
  final PicoClient client;
  final _FederationSession session;
  final BalanceDisplay display;
  final VoidCallback onBalanceTap;

  const _FederationPage({
    required this.client,
    required this.session,
    required this.display,
    required this.onBalanceTap,
  });

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // The pager bleeds to the screen edges so pages slide edge to edge,
        // which leaves the hero to keep the inset a [BleedColumn] would have
        // given it. The row below bleeds, as it does there.
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16),
          // Tapping the balance cycles the display, the same control as the
          // app-bar switcher.
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: onBalanceTap,
            child: _BalanceHero(
              balance: session.balance,
              display: display,
              rateClient: client,
            ),
          ),
        ),
        BorderedList.column(
          children: [
            _FederationRow(
              name: session.name,
              account: client.accountName(),
              connection: session.connection,
            ),
          ],
        ),
      ],
    );
  }
}

/// One page per shown account — its balance over the row naming it — swiped
/// through to choose the account every action routes through. Ordered as the
/// factory's map is, so a federation's accounts sit together and swiping runs
/// through one federation before reaching the next.
///
/// Replaces a picker: at the two or three mints a wallet actually holds, a
/// swipe is cheaper than a list, and the balance you are swiping to is the
/// answer to the question the list was being opened to ask. Accounts ride the
/// same gesture rather than a second control, because choosing one is the
/// same act as choosing a federation — it names the balance being spent.
class _FederationPager extends StatelessWidget implements Bleeds {
  final List<PicoClient> clients;
  // Keyed by [_clientKey], one entry per client in [clients].
  final Map<String, _FederationSession> sessions;
  final PageController controller;
  final BalanceDisplay display;
  final VoidCallback onBalanceTap;

  const _FederationPager({
    required this.clients,
    required this.sessions,
    required this.controller,
    required this.display,
    required this.onBalanceTap,
  });

  Widget _page(PicoClient client) => _FederationPage(
    client: client,
    session: sessions[_clientKey(client)]!,
    display: display,
    onBalanceTap: onBalanceTap,
  );

  @override
  Widget build(BuildContext context) {
    return Stack(
      children: [
        // Where the pager's height comes from. A viewport has no intrinsic
        // size, so a PageView is only ever as tall as its constraints make
        // it — and inside a column there are none to inherit. This lays a
        // page out to answer the question and never paints it, which beats
        // picking a number that has to be re-picked whenever the block
        // changes. Any page will do: [AmountHeadline] reserves its figure's
        // full-size line, so a page's height doesn't depend on its balance.
        TickerMode(
          enabled: false,
          child: IgnorePointer(
            child: Opacity(opacity: 0, child: _page(clients.first)),
          ),
        ),
        Positioned.fill(
          child: PageView.builder(
            controller: controller,
            itemCount: clients.length,
            // Settling on a page is the selection being made — the same
            // commitment a tap in a picker would have been, so it gets the
            // same haptic. Nothing to report upwards: the controller already
            // holds the selection.
            onPageChanged: (_) => HapticFeedback.selectionClick(),
            itemBuilder: (context, index) => _page(clients[index]),
          ),
        ),
      ],
    );
  }
}

/// Where the pager is and how many pages there are to swipe through.
///
/// Watches the controller directly, so the pill slides with the finger rather
/// than snapping when a page settles — and so a swipe rebuilds the dots
/// instead of the screen.
class _PageDots extends StatelessWidget {
  final int count;
  final PageController controller;

  const _PageDots({required this.count, required this.controller});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;

    return Padding(
      padding: const EdgeInsets.only(bottom: 16),
      child: AnimatedBuilder(
        animation: controller,
        builder: (context, _) {
          // No position to read until the pager has laid out, where it sits
          // on its initial page.
          final page =
              (controller.hasClients ? controller.page : null) ??
              controller.initialPage.toDouble();

          return Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              for (var i = 0; i < count; i++)
                _dot(scheme, (1 - (page - i).abs()).clamp(0.0, 1.0)),
            ],
          );
        },
      ),
    );
  }

  /// One dot, [t] of the way from dormant to current: it widens into a pill
  /// as it takes over, so the page reads at a glance without relying on the
  /// colour alone.
  Widget _dot(ColorScheme scheme, double t) => Container(
    margin: const EdgeInsets.symmetric(horizontal: 3),
    width: 6 + 12 * t,
    height: 6,
    decoration: BoxDecoration(
      color:
          Color.lerp(
            scheme.onSurfaceVariant.withValues(alpha: 0.3),
            scheme.primary,
            t,
          )!,
      borderRadius: BorderRadius.circular(3),
    ),
  );
}
