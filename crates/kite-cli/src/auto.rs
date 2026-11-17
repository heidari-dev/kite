#![cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    allow(dead_code)
)]
//! "Connect when the phone is plugged in": what the daemon remembers of the last USB connection,
//! and the rule for when to repeat it.

use kite_ipc::Mode;

/// The last USB connection that came up, which auto-connect repeats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Last {
    pub mode: Mode,
    pub kill_switch: bool,
    /// None: it was the only phone.
    pub serial: Option<String>,
}

impl Last {
    /// One line: `proxy|tun 0|1 SERIAL|-`.
    pub fn encode(&self) -> String {
        let mode = if self.mode == Mode::Tun {
            "tun"
        } else {
            "proxy"
        };
        let serial = self.serial.as_deref().unwrap_or("-");
        format!("{mode} {} {serial}\n", u8::from(self.kill_switch))
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let mode = match words.next()? {
            "tun" => Mode::Tun,
            "proxy" => Mode::Proxy,
            _ => return None,
        };
        let kill_switch = match words.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let serial = words.next()?;
        if words.next().is_some() {
            return None;
        }
        Some(Self {
            mode,
            kill_switch,
            serial: (serial != "-").then(|| serial.to_string()),
        })
    }
}

/// The phone to connect to on this poll, if any.
///
/// `phones` are the serials plugged in and ready. The remembered phone is the one to use; if it is
/// not there, the only phone is. `held` is set when the user disconnected: it keeps the phone that
/// was plugged in then from being connected again, and clears once no phone is there to use.
pub fn pick(
    on: bool,
    busy: bool,
    last: Option<&Last>,
    phones: &[String],
    held: &mut bool,
) -> Option<String> {
    let last = last?;
    let phone = last
        .serial
        .as_ref()
        .filter(|s| phones.contains(s))
        .or(match phones {
            [only] => Some(only),
            _ => None,
        });
    let Some(phone) = phone else {
        *held = false;
        return None;
    };
    (on && !busy && !*held).then(|| phone.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last(serial: Option<&str>) -> Last {
        Last {
            mode: Mode::Tun,
            kill_switch: true,
            serial: serial.map(Into::into),
        }
    }

    fn phones(serials: &[&str]) -> Vec<String> {
        serials.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn does_nothing_when_off() {
        let mut held = false;
        let p = phones(&["A"]);
        let l = last(Some("A"));
        assert_eq!(pick(false, false, Some(&l), &p, &mut held), None);
    }

    #[test]
    fn does_nothing_without_a_last_connection() {
        let mut held = false;
        assert_eq!(pick(true, false, None, &phones(&["A"]), &mut held), None);
    }

    #[test]
    fn connects_when_the_phone_is_there_and_not_when_it_is_not() {
        let mut held = false;
        let l = last(Some("A"));
        assert_eq!(pick(true, false, Some(&l), &[], &mut held), None);
        let a = phones(&["A"]);
        assert_eq!(pick(true, false, Some(&l), &a, &mut held), Some("A".into()));
        // Not shared yet: the next poll asks again.
        assert_eq!(pick(true, false, Some(&l), &a, &mut held), Some("A".into()));
    }

    #[test]
    fn waits_for_a_replug_after_the_user_disconnected() {
        let mut held = true;
        let l = last(Some("A"));
        let a = phones(&["A"]);
        assert_eq!(pick(true, false, Some(&l), &a, &mut held), None);
        assert_eq!(pick(true, false, Some(&l), &a, &mut held), None);
        assert_eq!(pick(true, false, Some(&l), &[], &mut held), None);
        assert!(!held);
        assert_eq!(pick(true, false, Some(&l), &a, &mut held), Some("A".into()));
    }

    #[test]
    fn leaves_a_running_tunnel_alone() {
        let mut held = false;
        let l = last(Some("A"));
        assert_eq!(pick(true, true, Some(&l), &phones(&["A"]), &mut held), None);
    }

    #[test]
    fn picks_by_serial_or_by_being_the_only_phone() {
        let mut held = false;
        let two = phones(&["B", "A"]);
        let (a, c, any) = (last(Some("A")), last(Some("C")), last(None));
        assert_eq!(
            pick(true, false, Some(&a), &two, &mut held),
            Some("A".into())
        );
        // No match among several: nothing.
        assert_eq!(pick(true, false, Some(&c), &two, &mut held), None);
        assert_eq!(pick(true, false, Some(&any), &two, &mut held), None);
        // No match, one phone: that one.
        let one = phones(&["B"]);
        assert_eq!(
            pick(true, false, Some(&c), &one, &mut held),
            Some("B".into())
        );
        assert_eq!(
            pick(true, false, Some(&any), &one, &mut held),
            Some("B".into())
        );
    }

    #[test]
    fn the_last_connection_round_trips_and_damage_is_refused() {
        let proxy = Last {
            mode: Mode::Proxy,
            kill_switch: false,
            serial: None,
        };
        for l in [last(Some("R5CT.1-2:3")), last(None), proxy] {
            assert_eq!(Last::decode(&l.encode()), Some(l));
        }
        for bad in ["", "tun", "tun 1", "tcp 1 A", "tun 2 A", "tun 1 A B"] {
            assert_eq!(Last::decode(bad), None, "{bad:?}");
        }
    }
}
