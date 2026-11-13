//! Kite for the desktop: a small window and a tray icon. Both are only a view of the daemon, which
//! owns the tunnel, so closing the window or quitting this program does not drop the connection.

// Not `forbid`: the code Slint generates for the UI has the unsafe parts of its item tree. None is
// written here, and `ui` is the only place it is allowed.
#![deny(unsafe_code)]

mod daemon;
mod present;
mod settings;
mod worker;

use kite_ipc::{Mode, State as Tunnel};
use present::Action;
use settings::Settings;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use worker::{format_rate, Cmd, Icon, Pair, View};

#[allow(unsafe_code)]
mod ui {
    slint::include_modules!();
}
use ui::{AppWindow, Kind, PhoneRow, Tray};

/// What the UI thread knows between events.
struct State {
    settings: Settings,
    settings_path: Option<PathBuf>,
    /// The phone the user clicked; the first ready one is used until then.
    picked: Option<String>,
    view: View,
    /// The rows in the window; kept, so a redraw does not replace the rows under a click.
    phones: Rc<VecModel<PhoneRow>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with_borrow_mut(|s| f(s.as_mut().expect("state is set before the loop starts")))
}

fn main() -> Result<(), slint::PlatformError> {
    let _lock = match single_instance() {
        Lock::Taken => {
            eprintln!("kite-desktop: already running");
            return Ok(());
        }
        Lock::Held(file) => Some(file),
        // Without a lock the app still runs; at worst there are two tray icons.
        Lock::Unavailable => None,
    };
    let hidden = std::env::args().any(|a| a == "--hidden");
    let settings_path = Settings::path();
    let phones = Rc::new(VecModel::default());
    STATE.set(Some(State {
        settings: settings_path
            .as_deref()
            .map(Settings::load)
            .unwrap_or_default(),
        settings_path,
        picked: None,
        view: View::default(),
        phones: phones.clone(),
    }));

    let ui = AppWindow::new()?;
    ui.set_phones(ModelRc::from(phones));
    let tray = Tray::new()?;
    let (cmds, rx) = unbounded_channel();
    let visible = Arc::new(AtomicBool::new(false));

    // The worker thread talks to the daemon; the UI thread only draws what it publishes.
    let (ui_weak, tray_weak) = (ui.as_weak(), tray.as_weak());
    let seen = visible.clone();
    std::thread::Builder::new()
        .name("kite-worker".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the tokio runtime starts");
            let publish = move |view: View| {
                let (ui, tray) = (ui_weak.clone(), tray_weak.clone());
                let _ = slint::invoke_from_event_loop(move || {
                    with_state(|s| s.view = view);
                    if let (Some(ui), Some(tray)) = (ui.upgrade(), tray.upgrade()) {
                        render(&ui, &tray);
                    }
                });
            };
            rt.block_on(worker::run(
                PathBuf::from(kite_ipc::DEFAULT_SOCKET),
                rx,
                seen,
                publish,
            ));
        })
        .map_err(|e| slint::PlatformError::Other(e.to_string()))?;

    wire(&ui, &tray, &cmds, &visible);
    if !hidden {
        show(&ui, &cmds, &visible)?;
    }
    render(&ui, &tray);
    slint::run_event_loop_until_quit()
}

fn show(
    ui: &AppWindow,
    cmds: &UnboundedSender<Cmd>,
    visible: &AtomicBool,
) -> Result<(), slint::PlatformError> {
    visible.store(true, Ordering::Relaxed);
    let _ = cmds.send(Cmd::Refresh);
    ui.show()
}

fn wire(ui: &AppWindow, tray: &Tray, cmds: &UnboundedSender<Cmd>, visible: &Arc<AtomicBool>) {
    // Closing the window hides it; the tray keeps the program alive.
    let v = visible.clone();
    ui.window().on_close_requested(move || {
        v.store(false, Ordering::Relaxed);
        slint::CloseRequestResponse::HideWindow
    });

    let handle = (ui.as_weak(), tray.as_weak());
    let redraw = move || {
        if let (Some(ui), Some(tray)) = (handle.0.upgrade(), handle.1.upgrade()) {
            render(&ui, &tray);
        }
    };

    let c = cmds.clone();
    ui.on_action(move || act(&c));
    let c = cmds.clone();
    tray.on_toggle(move || act(&c));

    let (r, ui_weak, c, v) = (redraw.clone(), ui.as_weak(), cmds.clone(), visible.clone());
    tray.on_open(move || {
        if let Some(ui) = ui_weak.upgrade() {
            let _ = show(&ui, &c, &v);
        }
        r();
    });
    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });

    let r = redraw.clone();
    ui.on_select_phone(move |i| {
        with_state(|s| {
            s.picked = usize::try_from(i)
                .ok()
                .and_then(|i| s.view.devices.get(i))
                .map(|d| d.serial.clone());
        });
        r();
    });
    let r = redraw.clone();
    ui.on_set_mode(move |i| {
        with_state(|s| {
            s.settings.mode = if i == 0 { Mode::Tun } else { Mode::Proxy };
            remember(s);
        });
        r();
    });
    let r = redraw.clone();
    ui.on_set_kill_switch(move |on| {
        with_state(|s| {
            s.settings.kill_switch = on;
            remember(s);
        });
        r();
    });

    let c = cmds.clone();
    ui.on_pair_start(move || {
        if let Some(serial) = with_state(|s| picked(s).map(String::from)) {
            let _ = c.send(Cmd::PairStart { serial });
        }
    });
    let c = cmds.clone();
    ui.on_pair_submit(move |code| {
        // The field takes digits only; anything else would be refused by the daemon anyway.
        let code: String = code.chars().filter(char::is_ascii_digit).collect();
        if code.len() == 6 {
            let _ = c.send(Cmd::Pair { code });
        }
    });
    let c = cmds.clone();
    ui.on_pair_cancel(move || {
        let _ = c.send(Cmd::PairCancel);
    });
}

fn picked(s: &State) -> Option<&str> {
    present::pick(&s.view.devices, s.picked.as_deref())
}

fn remember(s: &State) {
    // A setting that cannot be saved is only forgotten at the next start.
    if let Some(path) = &s.settings_path {
        let _ = s.settings.save(path);
    }
}

/// The big button and the tray's first item do the same.
fn act(cmds: &UnboundedSender<Cmd>) {
    let cmd =
        with_state(
            |s| match present::card(&s.view, s.settings.mode, picked(s).is_some()).action {
                Action::Disconnect => Some(Cmd::Disconnect),
                Action::Connect => picked(s).map(|serial| Cmd::Connect {
                    serial: serial.into(),
                    mode: s.settings.mode,
                    kill_switch: s.settings.kill_switch,
                }),
            },
        );
    if let Some(cmd) = cmd {
        let _ = cmds.send(cmd);
    }
}

fn render(ui: &AppWindow, tray: &Tray) {
    with_state(|s| {
        let v = &s.view;
        let phone = picked(s);
        let card = present::card(v, s.settings.mode, phone.is_some());
        let kind = match card.icon {
            Icon::Disconnected => Kind::Disconnected,
            Icon::Connecting => Kind::Connecting,
            Icon::Connected => Kind::Connected,
            Icon::Error => Kind::Error,
        };
        ui.set_blocker(v.unreachable.clone().unwrap_or_default().into());
        ui.set_kind(kind);
        ui.set_headline(card.headline.into());
        ui.set_detail(card.detail.clone().into());
        ui.set_action_label(card.label.into());
        ui.set_action_enabled(card.enabled);
        let rows: Vec<PhoneRow> = v
            .devices
            .iter()
            .take(present::MAX_PHONES)
            .map(|d| PhoneRow {
                serial: d.serial.clone().into(),
                note: present::phone_note(&d.state).into(),
                selected: phone == Some(d.serial.as_str()),
            })
            .collect();
        let same = rows.len() == s.phones.row_count()
            && rows
                .iter()
                .enumerate()
                .all(|(i, r)| s.phones.row_data(i).as_ref() == Some(r));
        if !same {
            s.phones.set_vec(rows);
        }
        ui.set_phones_note(present::phones_note(v).into());
        // While a tunnel runs the choices cannot change it, so the window shows what the daemon
        // runs, which may have been started from the command line.
        let running = v.status.as_ref().filter(|st| st.state != Tunnel::Idle);
        let mode = running.and_then(|st| st.mode).unwrap_or(s.settings.mode);
        let kill_switch = running.map_or(s.settings.kill_switch, |st| st.kill_switch);
        ui.set_locked(running.is_some());
        ui.set_mode(i32::from(mode == Mode::Proxy));
        ui.set_kill_switch(kill_switch);
        ui.set_show_speed(card.icon == Icon::Connected);
        ui.set_speed_up(format_rate(v.up).into());
        ui.set_speed_down(format_rate(v.down).into());
        let (step, fingerprint, note) = match &v.pair {
            Pair::Idle => (0, String::new(), String::new()),
            Pair::Waiting { fingerprint } => (1, fingerprint.clone(), String::new()),
            Pair::Done(key) => (2, String::new(), format!("Paired with phone {key}.")),
        };
        ui.set_pair_step(step);
        ui.set_fingerprint(fingerprint.into());
        ui.set_pair_note(note.into());
        ui.set_pair_enabled(phone.is_some());

        tray.set_kind(kind);
        let tip = if v.unreachable.is_some() {
            "Kite: the daemon is not available"
        } else {
            card.headline
        };
        tray.set_tip(SharedString::from(format!("Kite: {tip}")));
        tray.set_toggle_label(
            match card.action {
                Action::Connect => "Connect",
                Action::Disconnect => "Disconnect",
            }
            .into(),
        );
        tray.set_toggle_enabled(card.enabled);
    });
}

enum Lock {
    Held(std::fs::File),
    /// Another copy holds it.
    Taken,
    Unavailable,
}

/// A second copy would add a second tray icon. The lock goes away with the process, whatever
/// way it ends. It lives in the user's runtime directory, or else in their own config directory,
/// never in a directory that other users share.
fn single_instance() -> Lock {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| Settings::path()?.parent().map(PathBuf::from));
    let Some(dir) = dir else {
        return Lock::Unavailable;
    };
    let open = || {
        let _ = std::fs::create_dir_all(&dir);
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("kite-desktop.lock"))
    };
    match open().map(|f| (f.try_lock(), f)) {
        Ok((Ok(()), file)) => Lock::Held(file),
        Ok((Err(std::fs::TryLockError::WouldBlock), _)) => Lock::Taken,
        _ => Lock::Unavailable,
    }
}
