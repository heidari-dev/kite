import SwiftUI
import UIKit

/// One column, centred: the kite, what it is doing, one primary button. The column is capped at
/// 480 points and the whole screen scrolls, so large text and small phones never clip.
struct ContentView: View {
    @Environment(Sharing.self) private var sharing
    @Environment(\.scenePhase) private var scenePhase
    /// The screen is recorded or mirrored: the pairing code stays off it.
    @State private var captured = false

    var body: some View {
        GeometryReader { screen in
            let column = min(screen.size.width, 480)
            let side = max(0, min(column - 48, screen.size.height * 0.38, 300))
            ScrollView {
                VStack(spacing: 0) {
                    KiteView(target: kiteState, mbit: sharing.mbit, side: side)
                    content
                }
                .padding(.horizontal, 24)
                .padding(.top, 16)
                .padding(.bottom, 24)
                .frame(width: column)
                .frame(maxWidth: .infinity)
            }
        }
        .foregroundStyle(Palette.text)
        .background(Palette.background.ignoresSafeArea())
        // The app switcher keeps a picture of the screen; the pairing code must not be in it.
        .overlay {
            if scenePhase != .active { Palette.background.ignoresSafeArea() }
        }
        .alert("Allow this computer?", isPresented: asking, presenting: sharing.request) { fingerprint in
            Button("Allow") { sharing.answer(fingerprint, allow: true) }
            Button("Don't allow", role: .cancel) { sharing.answer(fingerprint, allow: false) }
        } message: { fingerprint in
            Text("Computer \(fingerprint) wants to use this phone's connection.\n\nAllow it only if you just ran kite pair and the computer printed the same fingerprint.")
        }
        .task(id: scenePhase == .active) {
            if scenePhase == .active { await sharing.poll() }
        }
        .onChange(of: sharing.serving, initial: true) { _, serving in
            // A locked phone would end sharing, so the screen stays on while it runs.
            UIApplication.shared.isIdleTimerDisabled = serving
        }
        .onChange(of: status) { _, now in
            AccessibilityNotification.Announcement(now).post()
        }
        .onAppear { captured = Self.isCaptured }
        .onReceive(NotificationCenter.default.publisher(for: UIScreen.capturedDidChangeNotification)) { _ in
            captured = Self.isCaptured
        }
    }

    @ViewBuilder private var content: some View {
        Text(status)
            .font(.title.bold())
            .multilineTextAlignment(.center)
            .padding(.top, 16)
        line(detail, color: sharing.error == nil ? Palette.secondary : Palette.error)
            .padding(.top, 8)
        line(speed, font: .title3.monospacedDigit(), color: Palette.text)
            .padding(.top, 8)

        Button(sharing.serving ? "Stop" : "Start sharing") {
            sharing.serving ? sharing.stop() : sharing.start()
        }
        .buttonStyle(KiteButton(filled: true))
        .padding(.top, 32)
        line(sharing.serving ? "Keep Kite open while sharing. The screen stays on, and sharing stops when you leave the app." : "", font: .footnote, color: Palette.secondary)
            .padding(.top, 12)

        if sharing.stats != nil {
            Button("Pair a computer") { sharing.openPairing() }
                .buttonStyle(KiteButton(filled: false))
                .padding(.top, 12)
        }
        line(pairing, color: Palette.secondary)
            .padding(.top, 16)
        if let code = sharing.code {
            Text(captured ? "Hidden while the screen is recorded or mirrored." : code.chunked)
                .font(captured ? .body : .largeTitle.monospaced().bold())
                .foregroundStyle(captured ? Palette.secondary : Palette.text)
                .multilineTextAlignment(.center)
                .padding(.top, 8)
                // A screen reader would read "482 915" as two numbers.
                .accessibilityLabel(captured ? "Pairing code hidden" : code.map(String.init).joined(separator: " "))
        }

        line(listeningLine, font: .callout.monospaced(), color: Palette.secondary)
            .textSelection(.enabled)
            .padding(.top, 24)

        if !sharing.paired.isEmpty {
            Text("Paired computers")
                .font(.callout.bold())
                .foregroundStyle(Palette.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, 32)
                .accessibilityAddTraits(.isHeader)
            ForEach(sharing.paired, id: \.self) { key in
                computer(key)
            }
        }
    }

    private func computer(_ key: Data) -> some View {
        let name = fingerprint(key: key) ?? "?"
        return HStack {
            Text(name)
                .font(.body.monospaced())
                .frame(maxWidth: .infinity, alignment: .leading)
            Button("Remove") { sharing.remove(key) }
                .font(.body.bold())
                .foregroundStyle(Palette.accent)
                .frame(minWidth: 44, minHeight: 48)
                .accessibilityLabel("Remove computer \(name)")
        }
        .frame(minHeight: 48)
    }

    /// A line of text that takes no room while it is empty.
    @ViewBuilder private func line(_ text: String, font: Font = .callout, color: Color) -> some View {
        if !text.isEmpty {
            Text(text)
                .font(font)
                .foregroundStyle(color)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var asking: Binding<Bool> {
        // Answering clears the request; a request that goes away closes the alert.
        Binding(get: { sharing.request != nil }, set: { _ in })
    }

    private var tunnels: Int { Int(sharing.stats?.tunnels ?? 0) }

    private var kiteState: Motion.State {
        if sharing.error != nil { return .error }
        if sharing.stats == nil { return sharing.serving ? .connecting : .disconnected }
        return tunnels > 0 ? .connected : .connecting
    }

    private var status: String {
        if sharing.error != nil { return "Could not start" }
        if sharing.stats == nil { return sharing.serving ? "Starting" : "Not sharing" }
        switch tunnels {
        case 0: return "Waiting for a computer"
        case 1: return "Sharing with 1 computer"
        default: return "Sharing with \(tunnels) computers"
        }
    }

    private var detail: String {
        if let error = sharing.error { return error }
        if sharing.stats == nil {
            return sharing.serving ? "" : "Start sharing, then run kite connect --usb or kite connect --wifi on the computer."
        }
        return tunnels == 0
            ? "Connect the iPhone to the computer by USB and tap Trust, or put the computer on this iPhone's Personal Hotspot or the same WiFi. Then run kite connect --usb or kite connect --wifi."
            : ""
    }

    private var speed: String {
        tunnels > 0 ? "Down \(Self.rate(sharing.down)), up \(Self.rate(sharing.up))" : ""
    }

    private var pairing: String {
        if sharing.code != nil { return "Type this code on the computer." }
        return sharing.pairingOpen
            ? "Pairing is open for two minutes. On the computer, run kite pair --usb or kite pair --wifi, then allow it here."
            : ""
    }

    private var listeningLine: String {
        guard sharing.stats != nil, sharing.synced else { return "" }
        if let error = sharing.usbError { return "Could not listen for USB: \(error)" }
        if let error = sharing.listenError { return "Could not listen: \(error)" }
        if sharing.listening.isEmpty {
            return "No WiFi or hotspot network found. Connecting by USB works without one."
        }
        return "Listening on \(sharing.listening.joined(separator: ", "))"
    }

    private static func rate(_ bytesPerSecond: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytesPerSecond, countStyle: .file) + "/s"
    }

    private static var isCaptured: Bool {
        UIApplication.shared.connectedScenes.contains { ($0 as? UIWindowScene)?.screen.isCaptured == true }
    }
}

private struct KiteButton: ButtonStyle {
    let filled: Bool

    func makeBody(configuration: Configuration) -> some View {
        let shape = RoundedRectangle(cornerRadius: 16, style: .continuous)
        configuration.label
            .font(.headline)
            .multilineTextAlignment(.center)
            .padding(.horizontal, 16)
            .frame(maxWidth: .infinity, minHeight: 56)
            .foregroundStyle(filled ? Palette.onAccent : Palette.accent)
            .background(shape.fill(filled ? Palette.accent : .clear))
            .overlay(shape.strokeBorder(Palette.accent, lineWidth: filled ? 0 : 2))
            .contentShape(shape)
            .opacity(configuration.isPressed ? 0.8 : 1)
            // The press shows at once; the release eases back.
            .animation(configuration.isPressed ? nil : Motion.out(0.16), value: configuration.isPressed)
    }
}

/// The colour tokens of the Android app (apps/android/.../values/colors.xml), light and dark.
/// Contrast: text 13.5:1 and 16:1, secondary 6.6:1 and 9.3:1, button text 5.7:1 and 4.9:1.
private enum Palette {
    static let background = dynamic(light: 0xF6F1E7, dark: 0x12161C)
    static let text = dynamic(light: 0x1F2630, dark: 0xF6F1E7)
    static let secondary = dynamic(light: 0x4F5662, dark: 0xB4BAC4)
    static let accent = dynamic(light: 0xB63E1C, dark: 0xE4572E)
    static let onAccent = dynamic(light: 0xFFFFFF, dark: 0x12161C)
    static let error = dynamic(light: 0xB63E1C, dark: 0xEF8A68)

    private static func dynamic(light: UInt32, dark: UInt32) -> Color {
        Color(UIColor { traits in
            let rgb = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(
                red: CGFloat((rgb >> 16) & 0xFF) / 255,
                green: CGFloat((rgb >> 8) & 0xFF) / 255,
                blue: CGFloat(rgb & 0xFF) / 255,
                alpha: 1)
        })
    }
}

private extension String {
    /// "482915" as "482 915".
    var chunked: String {
        count == 6 ? "\(prefix(3)) \(suffix(3))" : self
    }
}
