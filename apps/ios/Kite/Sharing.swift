import Foundation
import Observation

/// Runs the relay and keeps what the screen shows: its counters, a pairing in progress, the paired
/// computers and the addresses it listens on. The relay only runs while the app is in front;
/// iOS suspends an app in the background, and a suspended relay would hold computers that get no
/// answer.
@MainActor
@Observable
final class Sharing {
    /// The user started sharing and has not stopped it; sharing comes back with the app.
    private(set) var wanted = false
    /// The relay was started and has not been stopped; it may still be starting.
    private(set) var serving = false
    /// Why the relay could not start, or nil.
    private(set) var error: String?
    /// The relay's counters, nil while it is not running.
    private(set) var stats: Stats?
    private(set) var code: String?
    /// A computer asking to pair, as a fingerprint of its key.
    private(set) var request: String?
    /// Pairing was opened here and has not ended yet; the hint shows while it is open.
    private(set) var pairingOpen = false
    private(set) var paired: [Data] = []
    /// The `address:port` pairs the relay listens on.
    private(set) var listening: [String] = []
    private(set) var listenError: String?
    /// False from a start until the relay has answered about its addresses, so no stale line shows.
    private(set) var synced = false
    /// Bytes per second the computers download and upload.
    private(set) var down: Int64 = 0
    private(set) var up: Int64 = 0

    @ObservationIgnored private var relay: Relay?
    /// Starts and stops wait for the relay thread, so they run here one after another.
    @ObservationIgnored private let queue = DispatchQueue(label: "io.github.heidaridev.kite.relay")
    @ObservationIgnored private let advert = Advert()
    /// Counts starts and stops, so a start that finishes after a stop is undone.
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var asked: [String]?
    /// The relay whose addresses are being set, so a sync of a stopped relay does not hold up the next.
    @ObservationIgnored private var syncing: Relay?
    /// The last start, so a new one waits until a start that was stopped has given its port back.
    @ObservationIgnored private var starting: Task<Void, Never>?
    @ObservationIgnored private var ticks = 0
    @ObservationIgnored private var openedAt: ContinuousClock.Instant?
    @ObservationIgnored private var sawCode = false
    @ObservationIgnored private var measured: (at: ContinuousClock.Instant, stats: Stats)?
    @ObservationIgnored private var demo = false

    /// Throughput in Mbit/s, for the kite's tail.
    var mbit: Double { Double(down + up) * 8 / 1_000_000 }

    /// Reads the stored computers once, so the list shows before the first start.
    func load() async {
        if case .success(let peers) = await run({ try Keychain.peers() }), relay == nil, !demo {
            paired = peers
        }
    }

    func start() {
        wanted = true
        guard !serving else { return }
        serving = true
        error = nil
        generation += 1
        let started = generation
        let previous = starting
        starting = Task {
            await previous?.value
            guard started == generation else { return }
            let result = await run { () throws -> Relay in
                var key = try Keychain.privateKey()
                defer { key.resetBytes(in: key.startIndex..<key.endIndex) }
                return try Relay.startTcp(key: key, peers: try Keychain.peers())
            }
            guard started == generation else {
                // Stopped while it was starting.
                if case .success(let relay) = result { queue.async { relay.stop() } }
                return
            }
            switch result {
            case .success(let relay):
                self.relay = relay
                paired = relay.peers()
                refresh()
                await syncAddresses()
            case .failure(let failure):
                error = Self.describe(failure)
                serving = false
                wanted = false
            }
        }
    }

    /// The user stops sharing.
    func stop() {
        wanted = false
        halt()
    }

    /// The app left the foreground: the relay stops, and starts again with the app if it ran.
    func suspend() {
        halt()
    }

    func resume() {
        if wanted && !serving { start() }
    }

    /// Looks at the relay every second while the screen is in front. Cancelled with the view.
    func poll() async {
        while !Task.isCancelled {
            refresh()
            try? await Task.sleep(for: .seconds(1))
        }
    }

    /// Lets one more computer pair for two minutes.
    func openPairing() {
        guard let relay else { return }
        relay.openPairing()
        openedAt = .now
        pairingOpen = true
    }

    /// The user's answer about the computer `fingerprint`; the relay ignores it if another one
    /// waits by now.
    func answer(_ fingerprint: String, allow: Bool) {
        relay?.answerPairing(fingerprint: fingerprint, allow: allow)
        request = nil
        refresh()
    }

    /// Unpairs a computer, running or not. A tunnel it has up keeps running until it closes.
    func remove(_ key: Data) {
        if let relay {
            _ = relay.removePeer(key: key)
            paired = relay.peers()
        } else {
            paired.removeAll { $0 == key }
        }
        save(paired)
    }

    private func halt() {
        guard serving else { return }
        serving = false
        generation += 1
        let running = relay
        relay = nil
        stats = nil
        code = nil
        request = nil
        pairingOpen = false
        openedAt = nil
        listening = []
        listenError = nil
        synced = false
        asked = nil
        measure(nil)
        advert.stop()
        let peers = running?.peers()
        queue.async {
            // shortcut: a Keychain that refuses the write is silent; when the app gets a log, report it there
            if let peers { try? Keychain.savePeers(peers) }
            running?.stop()
        }
    }

    private func refresh() {
        guard !demo else { return }
        guard let relay else {
            measure(nil)
            return
        }
        let stats = relay.stats()
        self.stats = stats
        code = relay.pairingCode()
        request = relay.pairingRequest()
        // The hint lasts as long as the relay keeps pairing open, or until the code came and went.
        if code != nil {
            sawCode = true
        } else if sawCode {
            sawCode = false
            openedAt = nil
        }
        if let at = openedAt, at.duration(to: .now) > .seconds(120) { openedAt = nil }
        pairingOpen = openedAt != nil
        // A pairing or a removal changed the list: it must reach the Keychain too.
        let peers = relay.peers()
        if peers != paired {
            paired = peers
            save(peers)
        }
        measure(stats)
        // Networks come and go: follow them, so a listener ends with the network it is on.
        ticks += 1
        if ticks % 4 == 0 { Task { await syncAddresses() } }
    }

    /// Makes the relay listen on the phone's current addresses. Cheap when nothing changed.
    private func syncAddresses() async {
        guard let relay, syncing !== relay else { return }
        let want = Interfaces.addresses()
        if want == asked && listenError == nil { return }
        syncing = relay
        defer { if syncing === relay { syncing = nil } }
        asked = want
        let shared = await run { try relay.shareWifi(addresses: want) }
        guard relay === self.relay else { return }
        switch shared {
        case .success(let up):
            listening = up
            listenError = nil
            // An address that did not bind (a hotspot not ready yet) is tried again at the next look.
            if up.count < want.count { asked = nil }
        case .failure(let failure):
            listening = []
            listenError = Self.describe(failure)
        }
        synced = true
        advert.announce(port: listening.first?.split(separator: ":").last.flatMap { UInt16($0) })
    }

    /// Bytes per second since the last look, from the relay's counters.
    private func measure(_ stats: Stats?) {
        guard let stats else {
            measured = nil
            down = 0
            up = 0
            return
        }
        let now = ContinuousClock.now
        if let last = measured {
            let seconds = last.at.duration(to: now) / .seconds(1)
            // A look a moment after the last one would divide a few packets by a tiny time.
            guard seconds >= 0.9 else { return }
            // The computer downloads what the relay sends it.
            down = Int64(Double(stats.bytesOut &- last.stats.bytesOut) / seconds)
            up = Int64(Double(stats.bytesIn &- last.stats.bytesIn) / seconds)
        }
        measured = (now, stats)
    }

    private func save(_ peers: [Data]) {
        queue.async { try? Keychain.savePeers(peers) }
    }

    /// Runs `job` on the relay queue.
    private func run<T: Sendable>(_ job: @escaping @Sendable () throws -> T) async -> Result<T, any Error> {
        await withCheckedContinuation { done in
            queue.async { done.resume(returning: Result(catching: job)) }
        }
    }

    private nonisolated static func describe(_ error: any Error) -> String {
        if let error = error as? RelayError, case .Io(let reason) = error { return reason }
        return String(describing: error)
    }

    #if DEBUG
    /// A fixed screen with no relay behind it, for the simulator screenshots in CI:
    /// `-demo stopped|waiting|pairing|allow|code|connected|error|worst`.
    func showDemo(_ name: String) {
        demo = true
        let keys = (1...4).map { Data(repeating: UInt8($0), count: 32) }
        paired = [keys[0]]
        serving = name != "stopped" && name != "error"
        wanted = serving
        if serving {
            stats = Stats(bytesIn: 0, bytesOut: 0, tunnels: 0)
            listening = ["172.20.10.1:47810"]
            synced = true
        }
        switch name {
        case "pairing":
            pairingOpen = true
        case "allow":
            pairingOpen = true
            request = "3F9A 71C2 0B4D E815"
        case "code":
            pairingOpen = true
            code = "482915"
        case "connected":
            stats = Stats(bytesIn: 0, bytesOut: 0, tunnels: 1)
            down = 2_400_000
            up = 310_000
        case "error":
            error = "Address already in use (os error 48)"
        case "worst":
            paired = keys
            stats = Stats(bytesIn: 0, bytesOut: 0, tunnels: 4)
            listening = ["192.168.100.200:47810", "172.20.10.1:47810", "10.255.255.254:47810"]
            down = 987_654_321_000
            up = 123_456_789_000
        default:
            break
        }
    }
    #endif
}
