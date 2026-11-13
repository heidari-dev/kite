//! Kite for the desktop: a small window and a tray icon. Both are only a view of the daemon, which
//! owns the tunnel, so closing the window or quitting this program does not drop the connection.

// Not `forbid`: the code Slint generates for the UI has the unsafe parts of its item tree. None is
// written here, and `ui` is the only place it is allowed.
#![deny(unsafe_code)]

mod daemon;
mod kite;
mod present;
mod settings;
mod worker;

use kite::Kite;
use kite_ipc::{Mode, State as Tunnel};
use present::Action;
use settings::Settings;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use worker::{format_rate, Cmd, Icon, Pair, View};

#[allow(unsafe_code)]
mod ui {
    slint::include_modules!();
}
use ui::{AppWindow, Kind, KiteFrame, PhoneRow, Theme, Tray};

/// What the UI thread knows between events.
struct State {
    settings: Settings,
    settings_path: Option<PathBuf>,
    /// The phone the user clicked; the first ready one is used until then.
    picked: Option<String>,
    view: View,
    /// The rows in the window; kept, so a redraw does not replace the rows under a click.
    phones: Rc<VecModel<PhoneRow>>,
    kite: Kite,
    /// The window is open; the kite only gets frames then.
    open: bool,
    /// When the last frame was drawn.
    last: Instant,
    /// `--reduced-motion`: the reduced variant whatever the system says.
    reduced: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static TICK: Timer = Timer::default();
    /// A look at the minimised window is already scheduled.
    static WATCHING: Cell<bool> = const { Cell::new(false) };
}

/// 30 frames a second is all the kite's loops need.
const FRAME: Duration = Duration::from_millis(33);

/// The kite's frames go to the window only while it is open and something moves; at rest and
/// with the window closed no timer runs and nothing wakes up.
fn pace(ui: &AppWindow) {
    let run = with_state(|s| s.open && s.kite.animating());
    if !run {
        TICK.with(Timer::stop);
        return;
    }
    // A minimised window cannot be seen: no frames, only a slow look at whether it came back.
    if ui.window().is_minimized() {
        TICK.with(Timer::stop);
        if !WATCHING.replace(true) {
            let ui = ui.as_weak();
            Timer::single_shot(Duration::from_millis(500), move || {
                WATCHING.set(false);
                if let Some(ui) = ui.upgrade() {
                    pace(&ui);
                }
            });
        }
        return;
    }
    TICK.with(|t| {
        if !t.running() {
            with_state(|s| s.last = Instant::now());
            let ui = ui.as_weak();
            t.start(TimerMode::Repeated, FRAME, move || {
                if let Some(ui) = ui.upgrade() {
                    if ui.window().is_minimized() {
                        pace(&ui);
                        return;
                    }
                    let more = with_state(|s| {
                        let now = Instant::now();
                        let ms = now.duration_since(s.last).as_secs_f32() * 1000.0;
                        s.last = now;
                        s.kite.tick(ms);
                        s.kite.animating()
                    });
                    draw_kite(&ui);
                    if !more {
                        TICK.with(Timer::stop);
                    }
                }
            });
        }
    });
}

fn draw_kite(ui: &AppWindow) {
    let f = with_state(|s| s.kite.frame());
    let p = f.points;
    ui.set_kite(KiteFrame {
        x0: p[0].0,
        y0: p[0].1,
        x1: p[1].0,
        y1: p[1].1,
        x2: p[2].0,
        y2: p[2].1,
        x3: p[3].0,
        y3: p[3].1,
        x4: p[4].0,
        y4: p[4].1,
        x5: p[5].0,
        y5: p[5].1,
        x6: p[6].0,
        y6: p[6].1,
        x7: p[7].0,
        y7: p[7].1,
        x8: p[8].0,
        y8: p[8].1,
        taut: f.taut,
        slack: f.slack,
        snapped: f.snapped,
        badge: f.badge,
        badge_scale: f.badge_scale,
        art: f.art,
    });
}

static READING: AtomicBool = AtomicBool::new(false);

/// Asks for the system's motion and text size choices each time the window opens, off the UI
/// thread: `gsettings` can hang when the session bus is gone, and the window must not wait for
/// it. One question at a time; the answer is applied when it comes.
fn read_system_prefs(ui: &AppWindow) {
    if READING.swap(true, Ordering::Relaxed) {
        return;
    }
    let ui = ui.as_weak();
    let spawned = std::thread::Builder::new()
        .name("kite-prefs".into())
        .spawn(move || {
            let (reduced, scale) = system_prefs();
            READING.store(false, Ordering::Relaxed);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui.upgrade() {
                    let reduced = with_state(|s| {
                        let reduced = s.reduced || reduced;
                        s.kite.set_reduced(reduced);
                        reduced
                    });
                    let theme = ui.global::<Theme>();
                    theme.set_reduced_motion(reduced);
                    theme.set_text_scale(scale);
                    pace(&ui);
                    draw_kite(&ui);
                }
            });
        });
    if spawned.is_err() {
        READING.store(false, Ordering::Relaxed);
    }
}

fn gsettings(key: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/gsettings")
        .args(["get", "org.gnome.desktop.interface", key])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).ok())?
}

/// Whether to reduce motion, and by what factor to scale text (never below the designed size).
/// shortcut: GNOME's settings only; read KDE's and the desktop portal's when they are asked for.
fn system_prefs() -> (bool, f32) {
    if !cfg!(target_os = "linux") {
        return (false, 1.0);
    }
    let reduced = gsettings("enable-animations").is_some_and(|v| v.trim() == "false");
    let scale = gsettings("text-scaling-factor")
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|s| s.is_finite())
        .map_or(1.0, |s| s.clamp(1.0, 3.0));
    (reduced, scale)
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
    let reduced = std::env::args().any(|a| a == "--reduced-motion");
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
        kite: Kite::new(),
        open: false,
        last: Instant::now(),
        reduced,
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
            // A debug build can talk to the fake daemon of the examples; a release build never
            // takes the socket from the environment.
            let socket = PathBuf::from(kite_ipc::DEFAULT_SOCKET);
            #[cfg(debug_assertions)]
            let socket = std::env::var_os("KITE_SOCKET").map_or(socket, PathBuf::from);
            rt.block_on(worker::run(socket, rx, seen, publish));
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
    // Opening a window that is already open changes nothing. One that was closed opens at rest in
    // the state it is in: what changed meanwhile is not replayed, and the loops start from there.
    let was_open = with_state(|s| std::mem::replace(&mut s.open, true));
    if !was_open {
        with_state(|s| {
            s.kite.set_reduced(s.reduced);
            s.kite.settle();
        });
        ui.global::<Theme>()
            .set_reduced_motion(with_state(|s| s.reduced));
        draw_kite(ui);
        pace(ui);
        read_system_prefs(ui);
    }
    let shown = ui.show();
    if shown.is_err() {
        with_state(|s| s.open = false);
        TICK.with(Timer::stop);
    }
    shown
}

fn wire(ui: &AppWindow, tray: &Tray, cmds: &UnboundedSender<Cmd>, visible: &Arc<AtomicBool>) {
    // Closing the window hides it; the tray keeps the program alive.
    let v = visible.clone();
    ui.window().on_close_requested(move || {
        v.store(false, Ordering::Relaxed);
        with_state(|s| s.open = false);
        TICK.with(Timer::stop);
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

        s.kite.show(present::kite_state(v));
        if card.icon == Icon::Connected {
            s.kite
                .throughput(v.up.saturating_add(v.down) as f32 * 8.0 / 1e6);
        }
    });
    pace(ui);
    // At rest no frames come, so the picture is set here.
    if !TICK.with(Timer::running) {
        draw_kite(ui);
    }
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
