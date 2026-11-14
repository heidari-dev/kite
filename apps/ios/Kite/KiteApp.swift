import SwiftUI

@main
struct KiteApp: App {
    @State private var sharing = Sharing()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environment(sharing)
                .task {
                    #if DEBUG
                    // `-demo NAME` on the command line, for the screenshots in CI.
                    if let demo = UserDefaults.standard.string(forKey: "demo") {
                        sharing.showDemo(demo)
                        return
                    }
                    #endif
                    await sharing.load()
                }
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .background: sharing.suspend()
            case .active: sharing.resume()
            default: break
            }
        }
    }
}
