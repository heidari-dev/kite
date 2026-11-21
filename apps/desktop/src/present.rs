//! What the window and the tray say for a [`View`]: plain functions, no UI types.

use crate::kite::Show;
use crate::worker::{Icon, View};
use kite_ipc::{Device, Mode, State};

/// Phones listed at most; the daemon's answer is bounded but a window is not a log.
pub const MAX_PHONES: usize = 8;

/// What the big button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Connect,
    Disconnect,
}

/// How the phone is reached, and whether that choice can be used now: a ready phone on USB, a
/// usable address on WiFi (an empty one is usable: the daemon looks for the phone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Usb { phone: bool },
    Wifi { addr_ok: bool },
}

impl Link {
    pub fn ready(self) -> bool {
        match self {
            Self::Usb { phone: ok } | Self::Wifi { addr_ok: ok } => ok,
        }
    }
}

pub struct Card {
    pub icon: Icon,
    pub headline: &'static str,
    /// One plain sentence; the daemon's own words are in `details` when they say more.
    pub detail: String,
    /// The raw text behind `detail`, for the user to read or copy into a report.
    pub details: String,
    pub action: Action,
    pub label: &'static str,
    pub enabled: bool,
}

/// The phone to use: the one the user picked if it is still there and ready, else the first
/// ready one.
pub fn pick<'a>(devices: &'a [Device], picked: Option<&str>) -> Option<&'a str> {
    let ready = |d: &&Device| d.state == "device";
    devices
        .iter()
        .filter(ready)
        .find(|d| Some(d.serial.as_str()) == picked)
        .or_else(|| devices.iter().find(ready))
        .map(|d| d.serial.as_str())
}

/// Why a phone cannot be used yet; empty when it can.
pub fn phone_note(state: &str) -> String {
    match state {
        "device" => String::new(),
        "unauthorized" => "Allow USB debugging on the phone.".into(),
        "offline" => "Offline. Unplug the phone and plug it in again.".into(),
        other => format!("State: {other}"),
    }
}

pub fn phones_note(v: &View) -> String {
    if !v.devices.is_empty() {
        return String::new();
    }
    v.devices_error
        .clone()
        .unwrap_or_else(|| "No phone found. Plug it in with USB debugging turned on.".into())
}

/// What the kite shows: the same story as the icon, but a link that is being retried is the fall
/// of a dropped link.
pub fn kite_state(v: &View) -> Show {
    if v.unreachable.is_some() || v.failure().is_some() {
        return Show::Error;
    }
    match v.state() {
        Some(State::Connected) => Show::Connected,
        Some(State::Connecting) => Show::Connecting,
        Some(State::Reconnecting) => Show::Dropped,
        _ => Show::Disconnected,
    }
}

/// How long the user is told to look at the phone while connecting goes on.
pub const SLOW_HINT: &str = "Taking long? Open Kite on the phone and turn Sharing on.";

/// What to tell the user about an error text from the daemon or adb: a plain sentence, and the raw
/// text when that says more than the sentence.
pub fn explain(raw: &str) -> (String, String) {
    let low = raw.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| low.contains(w));
    let plain = if has(&["unauthorized"]) {
        "Allow USB debugging on the phone, then try again."
    } else if has(&["not paired", "no paired"]) {
        "This computer is not paired with the phone yet."
    } else if has(&["not found", "no device", "no phone"]) {
        "The phone was not found. Check the cable and that USB debugging is on."
    } else if has(&[
        "timed out",
        "timeout",
        "did not answer",
        "refused",
        "unreachable",
        "no route",
    ]) {
        "The phone did not answer. Open Kite on the phone and turn Sharing on."
    } else if raw.chars().count() <= 80 {
        // Already a short sentence of ours, in the daemon's lower case.
        let mut c = raw.chars();
        let first = c
            .next()
            .map(|f| f.to_uppercase().chain(c).collect::<String>());
        return (first.unwrap_or_default(), String::new());
    } else {
        "Something went wrong. Show details for the reason."
    };
    (plain.into(), raw.into())
}

pub fn card(v: &View, mode: Mode, link: Link) -> Card {
    let busy = |headline, detail: &str| {
        let (detail, details) = v
            .message
            .as_deref()
            .map_or_else(|| (detail.into(), String::new()), explain);
        Card {
            icon: v.icon(),
            headline,
            detail,
            details,
            action: Action::Disconnect,
            label: "Disconnect",
            enabled: true,
        }
    };
    match v.state() {
        // What runs is what the daemon says, which may have been started from the command line.
        Some(State::Connected) => busy(
            "Connected",
            match v.status.as_ref().and_then(|s| s.mode).unwrap_or(mode) {
                Mode::Tun => "All traffic goes through the phone.",
                Mode::Proxy => "A SOCKS5 proxy is running on this computer.",
            },
        ),
        Some(State::Connecting) => Card {
            label: "Cancel",
            ..busy("Connecting", if v.slow { SLOW_HINT } else { "" })
        },
        Some(State::Reconnecting) => busy(
            "Phone disconnected",
            "Reconnecting. Traffic stays blocked until the phone is back.",
        ),
        _ => {
            let (detail, details) = if let Some(raw) = v.message.as_deref().or(v.failure()) {
                explain(raw)
            } else if !link.ready() {
                let hint = match link {
                    Link::Usb { .. } => "Plug in a phone to start.",
                    Link::Wifi { .. } => "Fix the phone's address to start.",
                };
                (hint.into(), String::new())
            } else if v.paired == 0 {
                (
                    "Pair this computer with the phone first.".into(),
                    String::new(),
                )
            } else {
                (String::new(), String::new())
            };
            Card {
                icon: v.icon(),
                headline: if v.failure().is_some() {
                    "Not connected"
                } else {
                    "Disconnected"
                },
                detail,
                details,
                action: Action::Connect,
                label: "Connect",
                enabled: link.ready() && v.paired > 0,
            }
        }
    }
}

pub struct Step {
    pub title: &'static str,
    pub text: String,
    pub done: bool,
}

/// The three steps shown while something is missing before Connect can work.
pub struct Setup {
    pub steps: [Step; 3],
    /// The phone is there and only the pairing is left: the main button pairs.
    pub pair_next: bool,
}

impl Setup {
    /// Index of the step the user is on.
    pub fn current(&self) -> usize {
        self.steps.iter().position(|s| !s.done).unwrap_or(2)
    }

    pub fn detail(&self) -> &'static str {
        if self.steps.iter().all(|s| !s.done) {
            "Three steps and you are online."
        } else {
            "Two more steps and you are online."
        }
    }
}

/// The steps, or `None` when nothing is missing (or the daemon is not there to ask).
pub fn setup(v: &View, link: Link, serial: Option<&str>, wifi_error: &str) -> Option<Setup> {
    if v.unreachable.is_some() || v.state() != Some(State::Idle) || (link.ready() && v.paired > 0) {
        return None;
    }
    let phone = match (link, serial) {
        (Link::Usb { .. }, Some(serial)) if link.ready() => Step {
            title: "Phone found",
            text: format!("{serial}, USB debugging is on"),
            done: true,
        },
        (Link::Wifi { .. }, _) if link.ready() => Step {
            title: "Phone on WiFi",
            text: "Kite looks for it on this network.".into(),
            done: true,
        },
        (Link::Wifi { .. }, _) => Step {
            title: "Fix the phone's address",
            text: wifi_error.into(),
            done: false,
        },
        _ => Step {
            title: "Connect your phone",
            text: v.devices.first().map_or_else(
                || "Turn on USB debugging in the phone's Developer options, then plug it in with the cable.".into(),
                |d| phone_note(&d.state),
            ),
            done: false,
        },
    };
    let pair_next = phone.done;
    let paired = v.paired > 0;
    Some(Setup {
        steps: [
            phone,
            Step {
                title: if paired { "Paired" } else { "Pair this computer with the phone" },
                text: match (paired, cfg!(windows)) {
                    (true, _) => "This computer is paired with the phone.".into(),
                    (false, true) => "Windows asks for permission once. A window opens where you type the 6-digit code the phone shows.".into(),
                    (false, false) => "Type the 6-digit code the phone shows.".into(),
                },
                done: paired,
            },
            Step {
                title: "Connect",
                text: "One click, and later automatically when you plug in.".into(),
                done: false,
            },
        ],
        pair_next: pair_next && !paired,
    })
}

/// "USB" or "WiFi", and for a running link how long it has been up: "USB · 12:34".
pub fn link_meta(v: &View) -> String {
    let way = match v.status.as_ref().and_then(|s| s.transport.as_deref()) {
        Some("usb") => "USB",
        _ => "WiFi",
    };
    match v.uptime {
        0 => way.into(),
        t if t >= 3600 => format!("{way} \u{b7} {}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60),
        t => format!("{way} \u{b7} {}:{:02}", t / 60, t % 60),
    }
}

/// The phone a running link goes to: the serial for USB, else the address the daemon dialled.
pub fn link_name(v: &View, serial: Option<&str>) -> String {
    match v.status.as_ref().and_then(|s| s.transport.as_deref()) {
        Some("usb") => serial.unwrap_or("Phone").into(),
        Some(addr) => addr.into(),
        None => String::new(),
    }
}

/// The settings that apply to a running link, as short labels.
pub fn chips(v: &View, mode: Mode, kill_switch: bool) -> Vec<&'static str> {
    let mut c = vec![if mode == Mode::Tun {
        "Full"
    } else {
        "Proxy only"
    }];
    if kill_switch && mode == Mode::Tun {
        c.push("Kill switch on");
    }
    if v.status.as_ref().is_some_and(|s| s.auto_connect) {
        c.push("Auto-connect on");
    }
    c
}

/// The first line of the tray menu: the state, without opening the window.
pub fn tray_line(v: &View, link: Link, rate: Option<u64>) -> String {
    if v.unreachable.is_some() {
        return "Daemon not available".into();
    }
    match v.state() {
        Some(State::Connected) => rate.map_or_else(
            || "Connected".into(),
            |r| format!("Connected \u{b7} {}", crate::worker::format_rate(r)),
        ),
        Some(State::Connecting) => "Connecting".into(),
        Some(State::Reconnecting) => "Phone disconnected".into(),
        _ if v.failure().is_some() => "Not connected".into(),
        _ => match link {
            Link::Usb { phone: false } => "Phone not found".into(),
            Link::Wifi { addr_ok: false } => "Phone address needs fixing".into(),
            _ if v.paired == 0 => "Not paired".into(),
            _ => "Ready".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kite_ipc::Status;

    fn dev(serial: &str, state: &str) -> Device {
        Device {
            serial: serial.into(),
            state: state.into(),
        }
    }

    const USB: Link = Link::Usb { phone: true };
    const NO_PHONE: Link = Link::Usb { phone: false };

    fn view(state: Option<State>, error: Option<&str>) -> View {
        View {
            status: state.map(|state| Status {
                state,
                transport: Some("usb".into()),
                mode: Some(Mode::Tun),
                kill_switch: false,
                error: error.map(Into::into),
                pairing: false,
                auto_connect: false,
            }),
            paired: 1,
            ..View::default()
        }
    }

    #[test]
    fn picks_the_chosen_phone_or_the_first_ready_one() {
        let list = [
            dev("a", "unauthorized"),
            dev("b", "device"),
            dev("c", "device"),
        ];
        assert_eq!(pick(&list, None), Some("b"));
        assert_eq!(pick(&list, Some("c")), Some("c"));
        // Gone, or not ready: the choice falls back.
        assert_eq!(pick(&list, Some("zz")), Some("b"));
        assert_eq!(pick(&list, Some("a")), Some("b"));
        assert_eq!(pick(&[dev("a", "offline")], None), None);
        assert_eq!(pick(&[], None), None);
    }

    #[test]
    fn says_why_a_phone_is_not_ready() {
        assert_eq!(phone_note("device"), "");
        assert!(phone_note("unauthorized").contains("USB debugging"));
        assert!(phone_note("offline").contains("plug it in again"));
        assert_eq!(phone_note("bootloader"), "State: bootloader");
    }

    #[test]
    fn the_button_follows_the_state() {
        let c = card(&view(Some(State::Idle), None), Mode::Tun, USB);
        assert_eq!(
            (c.action, c.label, c.enabled),
            (Action::Connect, "Connect", true)
        );
        assert_eq!(c.icon, Icon::Disconnected);

        let c = card(&view(Some(State::Connecting), None), Mode::Tun, USB);
        assert_eq!((c.action, c.label), (Action::Disconnect, "Cancel"));

        // The daemon runs full mode (see `view`), whatever the window's choice says.
        let c = card(&view(Some(State::Connected), None), Mode::Proxy, USB);
        assert_eq!((c.headline, c.action), ("Connected", Action::Disconnect));
        assert!(c.detail.contains("All traffic"));

        let c = card(
            &view(Some(State::Reconnecting), Some("lost")),
            Mode::Tun,
            USB,
        );
        assert_eq!(c.headline, "Phone disconnected");
        assert_eq!(c.action, Action::Disconnect);
    }

    #[test]
    fn the_kite_shows_the_same_story_as_the_icon() {
        let show = |state, error| kite_state(&view(state, error));
        assert_eq!(kite_state(&View::default()), Show::Disconnected);
        assert_eq!(show(Some(State::Idle), None), Show::Disconnected);
        assert_eq!(show(Some(State::Connecting), None), Show::Connecting);
        assert_eq!(show(Some(State::Connected), None), Show::Connected);
        // A link being retried is the fall of a dropped link.
        assert_eq!(show(Some(State::Reconnecting), Some("lost")), Show::Dropped);
        assert_eq!(show(Some(State::Idle), Some("not paired")), Show::Error);
        let down = View {
            unreachable: Some("gone".into()),
            ..View::default()
        };
        assert_eq!(kite_state(&down), Show::Error);
    }

    #[test]
    fn connect_waits_for_a_phone_and_a_pairing() {
        let c = card(&view(Some(State::Idle), None), Mode::Tun, NO_PHONE);
        assert!(!c.enabled && c.detail.contains("Plug in"));
        let mut v = view(Some(State::Idle), None);
        v.paired = 0;
        let c = card(&v, Mode::Tun, USB);
        assert!(!c.enabled && c.detail.contains("Pair"));
    }

    #[test]
    fn wifi_connects_with_no_phone_on_usb() {
        let idle = view(Some(State::Idle), None);
        // The adb error belongs to USB; a WiFi card never shows it.
        let mut v = View {
            devices_error: Some("the adb server is not running".into()),
            ..idle
        };
        let c = card(&v, Mode::Tun, Link::Wifi { addr_ok: true });
        assert!(c.enabled);
        assert_eq!(c.detail, "");
        let c = card(&v, Mode::Tun, Link::Wifi { addr_ok: false });
        assert!(!c.enabled && c.detail.contains("address"));
        v.paired = 0;
        let c = card(&v, Mode::Tun, Link::Wifi { addr_ok: true });
        assert!(!c.enabled && c.detail.contains("Pair"));
        assert!(Link::Wifi { addr_ok: true }.ready() && !NO_PHONE.ready());
    }

    #[test]
    fn a_failed_attempt_and_a_refusal_show_their_words() {
        let c = card(
            &view(Some(State::Idle), Some("not paired with a phone yet")),
            Mode::Tun,
            USB,
        );
        assert_eq!(c.icon, Icon::Error);
        assert_eq!(c.headline, "Not connected");
        assert_eq!(c.detail, "This computer is not paired with the phone yet.");
        assert_eq!(c.details, "not paired with a phone yet");
        let mut v = view(Some(State::Idle), None);
        v.message = Some("already connected; disconnect first".into());
        let c = card(&v, Mode::Tun, USB);
        assert_eq!(c.detail, "Already connected; disconnect first");
        assert_eq!(c.details, "");
    }

    #[test]
    fn a_long_error_is_cut_to_a_sentence_and_kept_whole() {
        let raw = "could not open the phone: adb failed with a very long message from the platform tools that nobody can act on";
        let (plain, whole) = explain(raw);
        assert!(plain.len() < 60 && plain.contains("Show details"));
        assert_eq!(whole, raw);
        let (plain, whole) = explain("error: device 'R58M1234ABC' not found");
        assert!(plain.contains("USB debugging"));
        assert!(whole.contains("R58M1234ABC"));
        assert_eq!(explain("").0, "");
    }

    #[test]
    fn connecting_gets_a_hint_only_when_it_drags_on() {
        let mut v = view(Some(State::Connecting), None);
        assert_eq!(card(&v, Mode::Tun, USB).detail, "");
        v.slow = true;
        assert_eq!(card(&v, Mode::Tun, USB).detail, SLOW_HINT);
    }

    #[test]
    fn the_steps_follow_what_is_missing() {
        let idle = || view(Some(State::Idle), None);
        let step = |v: &View, link, wifi_error| setup(v, link, pick(&v.devices, None), wifi_error);

        // Nothing missing, a running tunnel and an absent daemon: no card.
        let mut v = idle();
        v.devices = vec![dev("R58M", "device")];
        assert!(step(&v, USB, "").is_none());
        assert!(step(&view(Some(State::Connected), None), USB, "").is_none());
        assert!(step(&View::default(), USB, "").is_none());

        // A phone and no pairing: the card is on step 2 and the main button pairs.
        v.paired = 0;
        let s = step(&v, USB, "").unwrap();
        assert!(s.steps[0].done && !s.steps[1].done && s.pair_next);
        assert_eq!(
            (s.current(), s.detail()),
            (1, "Two more steps and you are online.")
        );
        assert!(s.steps[0].text.contains("R58M"));

        // No phone yet: step 1, and the phone's own state says what to do.
        let mut v = idle();
        v.paired = 0;
        let s = step(&v, NO_PHONE, "").unwrap();
        assert!(!s.pair_next && s.current() == 0);
        assert_eq!(s.detail(), "Three steps and you are online.");
        assert!(s.steps[0].text.contains("Developer options"));
        v.devices = vec![dev("R58M", "unauthorized")];
        assert!(step(&v, NO_PHONE, "").unwrap().steps[0]
            .text
            .contains("Allow USB debugging"));

        // Paired but the phone is gone: step 1 again.
        let v = idle();
        let s = step(&v, NO_PHONE, "").unwrap();
        assert!(s.steps[1].done && s.current() == 0 && !s.pair_next);

        // A bad WiFi address is step 1's text.
        let s = step(&v, Link::Wifi { addr_ok: false }, "not an address").unwrap();
        assert_eq!(s.steps[0].text, "not an address");
    }

    #[test]
    fn a_running_link_says_which_phone_how_and_for_how_long() {
        let mut v = view(Some(State::Connected), None);
        assert_eq!(link_name(&v, Some("R58M")), "R58M");
        assert_eq!(link_meta(&v), "USB");
        v.uptime = 754;
        assert_eq!(link_meta(&v), "USB \u{b7} 12:34");
        v.uptime = 3 * 3600 + 5 * 60 + 9;
        assert_eq!(link_meta(&v), "USB \u{b7} 3:05:09");
        v.status.as_mut().unwrap().transport = Some("192.168.1.7:6000".into());
        assert_eq!(link_name(&v, Some("R58M")), "192.168.1.7:6000");
        assert!(link_meta(&v).starts_with("WiFi"));
        v.status.as_mut().unwrap().auto_connect = true;
        assert_eq!(
            chips(&v, Mode::Tun, true),
            ["Full", "Kill switch on", "Auto-connect on"]
        );
        // The kill switch is a full-mode setting.
        assert_eq!(
            chips(&v, Mode::Proxy, true),
            ["Proxy only", "Auto-connect on"]
        );
    }

    #[test]
    fn the_tray_line_says_the_state() {
        let v = view(Some(State::Connected), None);
        assert_eq!(tray_line(&v, USB, None), "Connected");
        assert_eq!(
            tray_line(&v, USB, Some(2_400_000)),
            "Connected \u{b7} 2.4 MB/s"
        );
        let idle = view(Some(State::Idle), None);
        assert_eq!(tray_line(&idle, USB, None), "Ready");
        assert_eq!(tray_line(&idle, NO_PHONE, None), "Phone not found");
        let mut v = idle;
        v.paired = 0;
        assert_eq!(tray_line(&v, USB, None), "Not paired");
        let down = View {
            unreachable: Some("gone".into()),
            ..View::default()
        };
        assert_eq!(tray_line(&down, USB, None), "Daemon not available");
    }

    #[test]
    fn no_phone_says_how_to_get_one() {
        let mut v = View::default();
        assert!(phones_note(&v).contains("USB debugging"));
        v.devices_error = Some("the adb server is not running".into());
        assert_eq!(phones_note(&v), "the adb server is not running");
        v.devices = vec![dev("a", "device")];
        assert_eq!(phones_note(&v), "");
    }
}
