import SwiftUI

/// The animated kite of docs/design/motion.md: one shape per layer, moved with offsets, rotations
/// and opacities. The loops (sway, wobble, tail flutter) run in a timeline that draws at most 30
/// times a second, only while a loop runs and the app is in front.
///
/// The caller says what the link is doing with `target`; the view works out the event. A new
/// target starts from what is on screen, and nothing waits for an animation to end.
struct KiteView: View {
    let target: Motion.State
    /// Throughput in Mbit/s; the tail follows it.
    let mbit: Double
    let side: CGFloat

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase

    @State private var shown = Motion.State.disconnected
    @State private var pose = Motion.rest
    @State private var taut = 0.0
    @State private var slack = 1.0
    @State private var snapped = 0.0
    @State private var badge = 0.0
    @State private var badgeScale = Motion.badgeFrom
    @State private var art = 1.0
    @State private var loops = Loops()
    @State private var looping = false
    /// The parts of the current transition that start later; a new event cancels them.
    @State private var later: Task<Void, Never>?
    @State private var fades = 0

    private var visible: Bool { scenePhase == .active }

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30, paused: !looping || !visible || reduceMotion)) {
            context in
            let angles = loops.advance(to: context.date, rate: Motion.tailRate(mbit: mbit))
            drawing(angles)
        }
        .frame(width: side, height: side)
        // The status text beside it says the same, so a screen reader skips the picture.
        .accessibilityHidden(true)
        .onAppear { snap(target) }
        .onChange(of: target) { _, new in
            for event in Motion.events(from: shown, to: new) { go(event) }
        }
        .onChange(of: reduceMotion) { _, reduced in
            if reduced {
                loops.halt()
                looping = false
            } else {
                snap(shown)
            }
        }
        .onChange(of: visible) { _, now in
            // Hidden: nothing runs and the kite shows where it was going. Loops keep their phase.
            if !now { halt() }
        }
    }

    private func drawing(_ angles: Loops.Angles) -> some View {
        let unit = side / Motion.stage
        let line = StrokeStyle(lineWidth: 2 * unit, lineCap: .round)
        return ZStack {
            ZStack {
                Stage(art: Art.slack).stroke(Art.deep, style: line).opacity(slack)
                Stage(art: Art.snapped).stroke(Art.deep, style: line).opacity(snapped)
                ZStack {
                    Stage(art: Art.taut).stroke(Art.deep, style: line).opacity(taut)
                    ZStack {
                        Stage(art: Art.lightFacet).fill(Art.light)
                        Stage(art: Art.deepFacet).fill(Art.deep)
                        Stage(art: Art.tail).stroke(Art.deep, style: line)
                            .rotationEffect(.degrees(angles.tail), anchor: Motion.b)
                    }
                    .rotationEffect(.degrees(angles.rock), anchor: Motion.b)
                    .rotationEffect(.degrees(pose.rotation), anchor: Motion.b)
                    .offset(x: pose.x * unit, y: pose.y * unit)
                }
                .rotationEffect(.degrees(angles.rig), anchor: Motion.hand)
            }
            .opacity(art)
            ZStack {
                Stage(art: Art.badge).fill(Art.light)
                Stage(art: Art.mark).stroke(.white, style: line)
            }
            .scaleEffect(badgeScale, anchor: Motion.badgeCentre)
            .opacity(badge)
        }
    }

    // MARK: Events

    private func go(_ event: Motion.Event) {
        let from = shown
        guard let to = Motion.next(from, event) else { return }
        shown = to
        later?.cancel()
        later = nil
        guard visible else {
            snap(to)
            return
        }
        if reduceMotion {
            shown = settled(to)
            fadeThrough(shown)
            return
        }
        switch event {
        case .connect:
            withAnimation(Motion.out(Motion.lift)) { pose = Motion.connecting }
            stringTaut()
            if from == .error { hideBadge() }
            runLoops(to, delay: Motion.lift)
        case .ready:
            let settle = from == .connecting ? Motion.settleFromConnecting : Motion.settle
            withAnimation(Motion.out(settle)) { pose = Motion.connected }
            stringTaut()
            if from == .error { hideBadge() }
            runLoops(to, delay: settle)
        case .stop:
            withAnimation(Motion.sine(Motion.driftStop)) { pose = Motion.rest }
            stringSlack(delay: Motion.stringDelayStop)
            if from == .error { hideBadge() }
            runLoops(to, delay: 0)
        case .lost:
            fall(then: [(Motion.lost, { @MainActor in if shown == .dropped { shown = .disconnected } })])
            stringSlack(delay: Motion.stringDelayLost)
            runLoops(to, delay: 0)
        case .fail:
            if from == .disconnected {
                // A stop a moment ago may have been cut off half way.
                withoutAnimation { showLayers(.disconnected) }
                snapString()
                showBadge(delay: 0)
            } else {
                fall(then: [(Motion.snapDelay, { @MainActor in snapString() })])
                stringSlack(delay: Motion.stringDelayLost)
                showBadge(delay: Motion.badgeDelay)
            }
            runLoops(to, delay: 0)
        }
    }

    /// Lost and Fail: a quick dip, then a drift down to Rest. One transition: an event cancels both.
    private func fall(then steps: [(Double, @MainActor () -> Void)]) {
        withAnimation(Motion.out(Motion.lostDip)) { pose = Motion.dip }
        let drift: (Double, @MainActor () -> Void) = (Motion.lostDip, { @MainActor in
            withAnimation(Motion.sine(Motion.lost - Motion.lostDip)) { pose = Motion.rest }
        })
        later = Task {
            var at = 0.0
            for (time, step) in ([drift] + steps).sorted(by: { $0.0 < $1.0 }) {
                try? await Task.sleep(for: .seconds(time - at))
                if Task.isCancelled { return }
                at = time
                step()
            }
        }
    }

    private func stringTaut() {
        withAnimation(Motion.out(Motion.stringFade).delay(Motion.stringDelayConnect)) {
            taut = 1
            slack = 0
            snapped = 0
        }
    }

    private func stringSlack(delay: Double) {
        withAnimation(Motion.out(Motion.stringFade).delay(delay)) {
            taut = 0
            slack = 1
            snapped = 0
        }
    }

    private func snapString() {
        withAnimation(Motion.out(Motion.snapFade)) {
            slack = 0
            snapped = 1
        }
    }

    private func hideBadge() {
        withAnimation(Motion.out(Motion.badgeHide)) {
            badge = 0
            badgeScale = Motion.badgeFrom
        }
    }

    private func showBadge(delay: Double) {
        withAnimation(Motion.out(Motion.badgeShow).delay(delay)) {
            badge = 1
            badgeScale = 1
        }
    }

    /// Reduced motion: the art fades out, the new state is set while it is invisible, then it
    /// fades in. A new event during the fade starts a new one from the current opacity.
    private func fadeThrough(_ to: Motion.State) {
        fades += 1
        let fade = fades
        withAnimation(.linear(duration: Motion.fadeOut)) {
            art = 0
        } completion: {
            guard fade == fades else { return }
            withoutAnimation { showLayers(to) }
            withAnimation(Motion.out(Motion.fadeIn)) { art = 1 }
        }
    }

    /// Sets the end state of `to` at once.
    private func snap(_ to: Motion.State) {
        later?.cancel()
        later = nil
        shown = settled(to)
        withoutAnimation {
            showLayers(shown)
            art = 1
        }
        loops.halt()
        looping = false
        if visible && !reduceMotion { runLoops(shown, delay: 0) }
    }

    private func halt() {
        later?.cancel()
        later = nil
        if shown == .dropped { shown = .disconnected }
        withoutAnimation {
            showLayers(shown)
            art = 1
        }
    }

    private func showLayers(_ state: Motion.State) {
        let lifted = state == .connecting || state == .connected
        let failed = state == .error
        pose = switch state {
        case .connecting: Motion.connecting
        case .connected: Motion.connected
        default: Motion.rest
        }
        taut = lifted ? 1 : 0
        slack = lifted || failed ? 0 : 1
        snapped = failed ? 1 : 0
        badge = failed ? 1 : 0
        badgeScale = failed ? 1 : Motion.badgeFrom
    }

    /// A link that dropped is Disconnected for good where no fall is played.
    private func settled(_ state: Motion.State) -> Motion.State {
        state == .dropped ? .disconnected : state
    }

    /// Starts and stops the loops for `state`; one that already runs is left alone. The timeline
    /// keeps running for the 150 ms a stopping loop needs to ease out, then pauses.
    private func runLoops(_ state: Motion.State, delay: Double) {
        loops.set(state, delay: delay)
        if loops.wanted {
            looping = true
        } else if looping {
            Task {
                try? await Task.sleep(for: .seconds(Motion.loopEase + 0.05))
                if !loops.wanted { looping = false }
            }
        }
    }

    private func withoutAnimation(_ body: () -> Void) {
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction, body)
    }
}

/// The four loops and the clock that moves them. Advanced from the timeline, not observed.
@MainActor
final class Loops {
    struct Angles {
        var rig = 0.0
        var rock = 0.0
        var tail = 0.0
    }

    private var wobble = Loop(degrees: Motion.wobble.degrees, period: Motion.wobble.period)
    private var swayRig = Loop(degrees: Motion.swayRig.degrees, period: Motion.swayRig.period)
    private var swayRock = Loop(degrees: Motion.swayRock.degrees, period: Motion.swayRock.period)
    private var flutter = Loop(degrees: Motion.tail.degrees, period: Motion.tail.period)
    private var last: Date?
    private var rate = 1.0

    var wanted: Bool { wobble.on || swayRig.on || swayRock.on || flutter.on }

    func set(_ state: Motion.State, delay: Double) {
        wobble.set(state == .connecting, delay: delay)
        swayRig.set(state == .connected, delay: delay)
        swayRock.set(state == .connected, delay: delay + Motion.swayRock.delay)
        flutter.set(state == .connected, delay: delay)
    }

    func halt() {
        wobble.halt()
        swayRig.halt()
        swayRock.halt()
        flutter.halt()
    }

    /// Moves every loop to `now`. A gap longer than a few frames (the app was hidden) counts as
    /// one frame, so the loops carry on from where they were.
    func advance(to now: Date, rate goal: Double) -> Angles {
        let dt = last.map { min(max(now.timeIntervalSince($0), 0), 0.1) } ?? 0
        last = now
        rate += (goal - rate) * min(1, dt / Motion.rateEase)
        flutter.period = Motion.tail.period / rate
        if wobble.busy { wobble.step(dt) }
        if swayRig.busy { swayRig.step(dt) }
        if swayRock.busy { swayRock.step(dt) }
        if flutter.busy { flutter.step(dt) }
        return Angles(rig: swayRig.angle, rock: wobble.angle + swayRock.angle, tail: flutter.angle)
    }
}

/// A path drawn on the 64 by 64 stage, scaled to the frame it is given.
private struct Stage: Shape {
    let art: Path

    func path(in rect: CGRect) -> Path {
        art.applying(CGAffineTransform(scaleX: rect.width / Motion.stage, y: rect.height / Motion.stage))
    }
}

/// The drawing of assets/brand/kite.svg and the layers of docs/design/motion.md, in stage units.
private enum Art {
    static let light = Color(red: 0xE4 / 255, green: 0x57 / 255, blue: 0x2E / 255)
    static let deep = Color(red: 0xB6 / 255, green: 0x3E / 255, blue: 0x1C / 255)

    static let lightFacet = Path { p in
        p.move(to: CGPoint(x: 38, y: 1))
        p.addLine(to: CGPoint(x: 21, y: 17))
        p.addLine(to: CGPoint(x: 38, y: 41))
        p.closeSubpath()
    }
    static let deepFacet = Path { p in
        p.move(to: CGPoint(x: 38, y: 1))
        p.addLine(to: CGPoint(x: 55, y: 17))
        p.addLine(to: CGPoint(x: 38, y: 41))
        p.closeSubpath()
    }
    /// From the kite's bottom vertex B, the pivot of the flutter.
    static let tail = Path { p in
        p.move(to: CGPoint(x: 38, y: 41))
        p.addCurve(to: CGPoint(x: 41, y: 52), control1: CGPoint(x: 42, y: 44), control2: CGPoint(x: 36, y: 47))
    }
    /// From the hand H to B.
    static let taut = Path { p in
        p.move(to: CGPoint(x: 6, y: 62))
        p.addQuadCurve(to: CGPoint(x: 38, y: 41), control: CGPoint(x: 24, y: 55))
    }
    /// From the hand to where B lands in the Rest pose.
    static let slack = Path { p in
        p.move(to: CGPoint(x: 6, y: 62))
        p.addQuadCurve(to: CGPoint(x: 20, y: 50), control: CGPoint(x: 12, y: 67))
    }
    /// A short piece at the hand and a short piece at the resting kite.
    static let snapped = Path { p in
        p.move(to: CGPoint(x: 6, y: 62))
        p.addQuadCurve(to: CGPoint(x: 12, y: 62), control: CGPoint(x: 9, y: 63.5))
        p.move(to: CGPoint(x: 20, y: 50))
        p.addCurve(to: CGPoint(x: 15, y: 54), control1: CGPoint(x: 19, y: 53), control2: CGPoint(x: 17, y: 54))
    }
    static let badge = Path(ellipseIn: CGRect(x: 45, y: 3, width: 16, height: 16))
    static let mark = Path { p in
        p.move(to: CGPoint(x: 53, y: 7))
        p.addLine(to: CGPoint(x: 53, y: 12))
        p.move(to: CGPoint(x: 53, y: 14.8))
        p.addLine(to: CGPoint(x: 53, y: 14.9))
    }
}
