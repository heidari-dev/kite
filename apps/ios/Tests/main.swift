// Checks of the app's logic that need no phone. CI builds this file together with
// Kite/KiteMotion.swift and Kite/Network.swift as a macOS program and runs it.
import Darwin

var failures = 0

func check(_ ok: Bool, _ what: String, line: Int = #line) {
    if !ok {
        print("FAIL line \(line): \(what)")
        failures += 1
    }
}

func near(_ a: Double, _ b: Double, _ tolerance: Double = 0.01) -> Bool { abs(a - b) <= tolerance }

// The state machine is the table of docs/design/motion.md, cell by cell.
let table: [(Motion.State, [Motion.Event: Motion.State])] = [
    (.disconnected, [.connect: .connecting, .ready: .connected, .fail: .error]),
    (.connecting, [.ready: .connected, .stop: .disconnected, .lost: .dropped, .fail: .error]),
    (.connected, [.stop: .disconnected, .lost: .dropped, .fail: .error]),
    (.dropped, [.connect: .connecting, .ready: .connected, .fail: .error]),
    (.error, [.connect: .connecting, .ready: .connected, .stop: .disconnected]),
]
for (from, row) in table {
    for event in [Motion.Event.connect, .ready, .stop, .lost, .fail] {
        check(Motion.next(from, event) == row[event], "\(from) + \(event)")
    }
}
// A connected kite that goes back to waiting falls first, then lifts.
check(Motion.events(from: .connected, to: .connecting) == [.lost, .connect], "connected to connecting")
check(Motion.events(from: .disconnected, to: .connecting) == [.connect], "disconnected to connecting")

// Flutter rate: 1400 ms idle, about 1060 ms at 1 Mbit/s, 670 at 10, 452 from 100 up.
check(Motion.tailRate(mbit: 0) == 1, "idle rate")
check(Motion.tailRate(mbit: -5) == 1, "negative rate")
check(near(1.4 / Motion.tailRate(mbit: 1), 1.06), "1 Mbit/s")
check(near(1.4 / Motion.tailRate(mbit: 10), 0.67), "10 Mbit/s")
check(near(1.4 / Motion.tailRate(mbit: 100), 0.452), "100 Mbit/s")
check(Motion.tailRate(mbit: 10_000) == Motion.tailRate(mbit: 100), "clamped")

// A loop starts at its zero crossing after its delay, and eases out to zero in 150 ms.
var loop = Loop(degrees: 10, period: 1)
loop.set(true, delay: 0.5)
loop.step(0.4)
check(loop.angle == 0, "waits for its delay")
loop.step(0.1)
check(loop.angle == 0, "starts at zero")
loop.step(0.25)
check(near(loop.angle, 10), "a quarter period in, at full amplitude")
let before = loop.angle
loop.period = 0.5 // a change of rate keeps the phase
check(loop.angle == before, "no jump when the rate changes")
loop.set(false, delay: 0)
loop.step(0.16)
check(loop.angle == 0 && !loop.busy, "eased out")

// Only private IPv4 addresses on WiFi, ethernet or the hotspot bridge.
for (text, ok) in [
    ("10.0.0.1", true), ("172.16.0.1", true), ("172.31.255.254", true), ("192.168.1.2", true),
    ("172.32.0.1", false), ("8.8.8.8", false), ("169.254.1.1", false), ("100.64.0.1", false),
    ("127.0.0.1", false), ("0.0.0.0", false),
] {
    check((Interfaces.privateIPv4(inet_addr(text)) == text) == ok, text)
}
for (name, ok) in [
    ("en0", true), ("en2", true), ("bridge100", true),
    ("pdp_ip0", false), ("utun3", false), ("ipsec0", false), ("lo0", false), ("awdl0", false), ("llw0", false),
] {
    check(Interfaces.shareable(interface: name) == ok, name)
}
for address in Interfaces.addresses() {
    check(Interfaces.privateIPv4(inet_addr(address)) == address, "listed \(address)")
}

print(failures == 0 ? "all checks passed" : "\(failures) failed")
exit(failures == 0 ? 0 : 1)
