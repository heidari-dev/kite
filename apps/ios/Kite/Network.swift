import Darwin
import dnssd

/// Where computers can reach the phone.
enum Interfaces {
    /// The private IPv4 addresses of interfaces that are up and are WiFi, ethernet or the Personal
    /// Hotspot bridge, and never mobile data (`pdp_ip`) or a VPN (`utun`, `ipsec`). It is an
    /// allow-list by name: an interface it does not know is left out. The relay checks again.
    static func addresses() -> [String] {
        var head: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&head) == 0 else { return [] }
        defer { freeifaddrs(head) }
        var found = Set<String>()
        var next = head
        while let entry = next?.pointee {
            next = entry.ifa_next
            let flags = Int32(entry.ifa_flags)
            guard let address = entry.ifa_addr, address.pointee.sa_family == sa_family_t(AF_INET),
                  flags & IFF_UP != 0, flags & (IFF_LOOPBACK | IFF_POINTOPOINT) == 0,
                  shareable(interface: String(cString: entry.ifa_name))
            else { continue }
            let ip = address.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { $0.pointee.sin_addr.s_addr }
            if let text = privateIPv4(ip) { found.insert(text) }
        }
        return found.sorted()
    }

    /// `en` is WiFi or ethernet (a USB adapter), `bridge` the Personal Hotspot.
    static func shareable(interface name: String) -> Bool {
        name.hasPrefix("en") || name.hasPrefix("bridge")
    }

    /// `a.b.c.d` for an address in network byte order, if it is in 10/8, 172.16/12 or 192.168/16.
    static func privateIPv4(_ networkOrder: in_addr_t) -> String? {
        let ip = UInt32(bigEndian: networkOrder)
        let (a, b) = (ip >> 24, (ip >> 16) & 0xFF)
        guard a == 10 || (a == 172 && (16...31).contains(b)) || (a == 192 && b == 168) else { return nil }
        return "\(a).\(b).\((ip >> 8) & 0xFF).\(ip & 0xFF)"
    }
}

/// Announces `_kite._tcp` while the relay listens, so `kite connect --wifi` finds the phone. The
/// name is `Kite` and the TXT record holds a version and nothing else: a key or the phone's name
/// would let anyone on the network follow it from one network to the next. Best effort: a
/// computer can still be given the address by hand.
@MainActor
final class Advert {
    private var service: DNSServiceRef?
    private var port: UInt16?

    /// Announces `port`, or nothing when it is nil.
    func announce(port: UInt16?) {
        guard port != self.port else { return }
        stop()
        guard let port else { return }
        let txt: [UInt8] = [3] + Array("v=1".utf8)
        var service: DNSServiceRef?
        // No reply callback: a registration that fails changes nothing for the user.
        let error = DNSServiceRegister(
            &service, 0, 0, "Kite", "_kite._tcp", nil, nil, port.bigEndian, UInt16(txt.count), txt, nil, nil)
        if error == 0 {
            self.service = service
            self.port = port
        }
    }

    func stop() {
        if let service { DNSServiceRefDeallocate(service) }
        service = nil
        port = nil
    }
}
