//! What the window and the tray say for a [`View`]: plain functions, no UI types.

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

pub struct Card {
    pub icon: Icon,
    pub headline: &'static str,
    pub detail: String,
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

/// `phone` says whether a ready phone is picked.
pub fn card(v: &View, mode: Mode, phone: bool) -> Card {
    let busy = |headline, detail: &str| Card {
        icon: v.icon(),
        headline,
        detail: v.message.clone().unwrap_or_else(|| detail.into()),
        action: Action::Disconnect,
        label: "Disconnect",
        enabled: true,
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
            ..busy("Connecting", "")
        },
        Some(State::Reconnecting) => busy(
            "Phone disconnected",
            "Reconnecting. Traffic stays blocked until the phone is back.",
        ),
        _ => {
            let hint = if let Some(failure) = v.failure() {
                failure.to_string()
            } else if !phone {
                "Plug in a phone to start.".into()
            } else if v.paired == 0 {
                "Pair this computer with the phone first.".into()
            } else {
                String::new()
            };
            Card {
                icon: v.icon(),
                headline: if v.failure().is_some() {
                    "Not connected"
                } else {
                    "Disconnected"
                },
                detail: v.message.clone().unwrap_or(hint),
                action: Action::Connect,
                label: "Connect",
                enabled: phone && v.paired > 0,
            }
        }
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

    fn view(state: Option<State>, error: Option<&str>) -> View {
        View {
            status: state.map(|state| Status {
                state,
                transport: Some("usb".into()),
                mode: Some(Mode::Tun),
                kill_switch: false,
                error: error.map(Into::into),
                pairing: false,
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
        let c = card(&view(Some(State::Idle), None), Mode::Tun, true);
        assert_eq!(
            (c.action, c.label, c.enabled),
            (Action::Connect, "Connect", true)
        );
        assert_eq!(c.icon, Icon::Disconnected);

        let c = card(&view(Some(State::Connecting), None), Mode::Tun, true);
        assert_eq!((c.action, c.label), (Action::Disconnect, "Cancel"));

        // The daemon runs full mode (see `view`), whatever the window's choice says.
        let c = card(&view(Some(State::Connected), None), Mode::Proxy, true);
        assert_eq!((c.headline, c.action), ("Connected", Action::Disconnect));
        assert!(c.detail.contains("All traffic"));

        let c = card(
            &view(Some(State::Reconnecting), Some("lost")),
            Mode::Tun,
            true,
        );
        assert_eq!(c.headline, "Phone disconnected");
        assert_eq!(c.action, Action::Disconnect);
    }

    #[test]
    fn connect_waits_for_a_phone_and_a_pairing() {
        let c = card(&view(Some(State::Idle), None), Mode::Tun, false);
        assert!(!c.enabled && c.detail.contains("Plug in"));
        let mut v = view(Some(State::Idle), None);
        v.paired = 0;
        let c = card(&v, Mode::Tun, true);
        assert!(!c.enabled && c.detail.contains("Pair"));
    }

    #[test]
    fn a_failed_attempt_and_a_refusal_show_their_words() {
        let c = card(
            &view(Some(State::Idle), Some("not paired with a phone yet")),
            Mode::Tun,
            true,
        );
        assert_eq!(c.icon, Icon::Error);
        assert_eq!(c.headline, "Not connected");
        assert_eq!(c.detail, "not paired with a phone yet");
        let mut v = view(Some(State::Idle), None);
        v.message = Some("already connected; disconnect first".into());
        assert_eq!(
            card(&v, Mode::Tun, true).detail,
            "already connected; disconnect first"
        );
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
