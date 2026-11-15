import SwiftUI

/// The numbers and rules of docs/design/motion.md. Times are seconds, angles degrees and lengths
/// stage units: the kite is drawn on a stage of 64 by 64 units.
enum Motion {
    enum State { case disconnected, connecting, connected, dropped, error }

    enum Event { case connect, ready, stop, lost, fail }

    /// A translation plus a rotation about the kite's bottom vertex B.
    struct Pose: Equatable {
        var x: Double
        var y: Double
        var rotation: Double
    }

    static let stage = 64.0
    static let b = UnitPoint(x: 38 / stage, y: 41 / stage)
    static let hand = UnitPoint(x: 6 / stage, y: 62 / stage)
    static let badgeCentre = UnitPoint(x: 53 / stage, y: 11 / stage)

    static let rest = Pose(x: -18, y: 9, rotation: 72)
    static let connecting = Pose(x: -1, y: 2, rotation: -8)
    static let connected = Pose(x: 0, y: 0, rotation: 0)
    static let dip = Pose(x: 0, y: 5, rotation: 6)

    static let lift = 0.45
    static let settleFromConnecting = 0.35
    static let settle = 0.45
    static let driftStop = 0.6
    static let stringFade = 0.25
    static let stringDelayConnect = 0.15
    static let stringDelayStop = 0.25
    static let stringDelayLost = 0.3
    static let badgeHide = 0.12
    static let lost = 0.9
    static let lostDip = 0.18
    static let snapDelay = 0.8
    static let snapFade = 0.12
    static let badgeDelay = 0.75
    static let badgeShow = 0.16
    static let badgeFrom = 0.9
    static let loopEase = 0.15
    static let fadeOut = 0.08
    static let fadeIn = 0.12

    static let wobble = (degrees: 6.0, period: 0.7)
    static let swayRig = (degrees: 1.6, period: 4.8)
    static let swayRock = (degrees: 3.5, period: 4.8, delay: 0.6)
    static let tail = (degrees: 10.0, period: 1.4)
    /// How fast the flutter follows a new throughput, so a jump in speed is not a jump in the tail.
    static let rateEase = 0.3

    static let outCurve = UnitCurve.bezier(
        startControlPoint: UnitPoint(x: 0.23, y: 1), endControlPoint: UnitPoint(x: 0.32, y: 1))

    static func out(_ duration: Double) -> Animation {
        .timingCurve(0.23, 1, 0.32, 1, duration: duration)
    }

    static func sine(_ duration: Double) -> Animation {
        .timingCurve(0.37, 0, 0.63, 1, duration: duration)
    }

    /// Flutter speed for a throughput in Mbit/s: 1x idle, 3.1x from 100 Mbit/s up.
    static func tailRate(mbit: Double) -> Double {
        let u = min(max(log10(1 + max(mbit, 0)) / log10(101), 0), 1)
        return 1 + 2.1 * u
    }

    /// The state machine's table; nil is an empty cell, and the event is ignored.
    static func next(_ from: State, _ event: Event) -> State? {
        switch event {
        case .connect: [.disconnected, .dropped, .error].contains(from) ? .connecting : nil
        case .ready: from != .connected ? .connected : nil
        case .stop: [.connecting, .connected, .error].contains(from) ? .disconnected : nil
        case .lost: [.connecting, .connected].contains(from) ? .dropped : nil
        case .fail: from != .error ? .error : nil
        }
    }

    /// The events that take the kite from what it shows to what the link is doing now.
    static func events(from shown: State, to target: State) -> [Event] {
        switch target {
        case .connected: [.ready]
        case .connecting: shown == .connected ? [.lost, .connect] : [.connect]
        case .disconnected: [.stop]
        case .error: [.fail]
        case .dropped: [.lost]
        }
    }
}

/// One sine on one layer's rotation. It keeps its own phase, so a change of rate never restarts
/// it, and eases its amplitude in and out so starting or stopping it never jumps.
struct Loop {
    let degrees: Double
    var period: Double
    private(set) var on = false
    private var gain = 0.0
    private var phase = 0.0
    private var wait = 0.0

    init(degrees: Double, period: Double) {
        self.degrees = degrees
        self.period = period
    }

    var busy: Bool { on || gain > 0 }

    var angle: Double { degrees * Motion.outCurve.value(at: gain) * sin(2 * .pi * phase) }

    mutating func set(_ want: Bool, delay: Double) {
        if !want {
            on = false
        } else if !on {
            on = true
            // Still easing out from a moment ago: pick up at once instead of holding its offset.
            wait = gain > 0 ? 0 : delay
            if gain == 0 { phase = 0 } // a fresh loop starts at its zero crossing
        }
    }

    mutating func halt() {
        on = false
        gain = 0
    }

    /// Moves the loop on by `dt` seconds.
    mutating func step(_ dt: Double) {
        if on && wait > 0 {
            wait -= dt
            return
        }
        gain = on ? min(1, gain + dt / Motion.loopEase) : max(0, gain - dt / Motion.loopEase)
        phase += dt / period
        phase -= phase.rounded(.down)
    }
}
