//! The kite's motion: `docs/design/motion.md` as code. No UI types and no clock of its own, so
//! it is the same every run and can be tested: the caller says how much time passed.
//!
//! Slint's software renderer ignores rotation and scale, so the rig of the spec (rig, pose, rock,
//! tail) is applied here to the points of the drawing, and the window draws plain paths from them.
//! The numbers, curves and timings are the spec's; nothing here is tuned by eye.

use std::f32::consts::TAU;

/// The stage is 64 by 64; the kite is drawn in its Connected pose.
const T: P = P(38.0, 1.0);
const L: P = P(21.0, 17.0);
const R: P = P(55.0, 17.0);
/// Bottom vertex: the pivot of pose, rock and tail, and where the string is tied.
const B: P = P(38.0, 41.0);
/// The hand: the pivot of the rig.
const H: P = P(6.0, 62.0);
const TAUT_CONTROL: P = P(24.0, 55.0);
const TAIL: [P; 3] = [P(42.0, 44.0), P(36.0, 47.0), P(41.0, 52.0)];

#[derive(Clone, Copy, Debug, PartialEq)]
struct P(f32, f32);

#[derive(Clone, Copy)]
struct Curve(f32, f32, f32, f32);

const OUT: Curve = Curve(0.23, 1.0, 0.32, 1.0);
const SINE: Curve = Curve(0.37, 0.0, 0.63, 1.0);
const LINEAR: Curve = Curve(0.0, 0.0, 1.0, 1.0);

impl Curve {
    /// y for x on the cubic bezier from (0, 0) to (1, 1).
    fn at(self, x: f32) -> f32 {
        let Curve(x1, y1, x2, y2) = self;
        let bez = |a: f32, b: f32, t: f32| {
            let u = 1.0 - t;
            3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
        };
        // Find t with bezier_x(t) == x by bisection: 20 steps are far below a pixel.
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..20 {
            let mid = (lo + hi) / 2.0;
            if bez(x1, x2, mid) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        bez(y1, y2, (lo + hi) / 2.0)
    }
}

/// Part of a move: wait `hold` ms, then go to `to` over `dur` ms.
#[derive(Clone, Copy)]
struct Leg {
    hold: f32,
    dur: f32,
    to: f32,
    curve: Curve,
}

const NO_LEG: Leg = Leg {
    hold: 0.0,
    dur: 0.0,
    to: 0.0,
    curve: LINEAR,
};

fn leg(hold: f32, dur: f32, to: f32, curve: Curve) -> Leg {
    Leg {
        hold,
        dur,
        to,
        curve,
    }
}

/// A value on its way from where it is now through up to three legs. A new move starts from the
/// value on screen, so nothing jumps when an event arrives in the middle of another.
#[derive(Clone, Copy)]
struct Tween {
    from: f32,
    legs: [Leg; 3],
    n: usize,
    t: f32,
}

impl Tween {
    fn at(v: f32) -> Self {
        Self {
            from: v,
            legs: [NO_LEG; 3],
            n: 0,
            t: 0.0,
        }
    }

    fn value(&self) -> f32 {
        let (mut x, mut v) = (self.t, self.from);
        for leg in &self.legs[..self.n] {
            if x < leg.hold {
                return v;
            }
            x -= leg.hold;
            if x < leg.dur {
                return v + (leg.to - v) * leg.curve.at(x / leg.dur);
            }
            x -= leg.dur;
            v = leg.to;
        }
        v
    }

    fn end(&self) -> f32 {
        self.legs[..self.n].last().map_or(self.from, |l| l.to)
    }

    fn done(&self) -> bool {
        self.t
            >= self.legs[..self.n]
                .iter()
                .map(|l| l.hold + l.dur)
                .sum::<f32>()
    }

    /// Goes from the current value through `legs`.
    fn go(&mut self, legs: &[Leg]) {
        let mut next = Self::at(self.value());
        next.legs[..legs.len()].copy_from_slice(legs);
        next.n = legs.len();
        *self = next;
    }

    fn to(&mut self, to: f32, hold: f32, dur: f32, curve: Curve) {
        self.go(&[leg(hold, dur, to, curve)]);
    }

    fn set(&mut self, v: f32) {
        *self = Self::at(v);
    }
}

/// A pose: a translation and a rotation about the bottom vertex.
#[derive(Clone, Copy)]
struct Pose(f32, f32, f32);

const REST: Pose = Pose(-18.0, 9.0, 72.0);
const CONNECTING: Pose = Pose(-1.0, 2.0, -8.0);
const CONNECTED: Pose = Pose(0.0, 0.0, 0.0);
const DIP: Pose = Pose(0.0, 5.0, 6.0);

/// A sine on one angle. It starts at its zero crossing, so it joins the pose without a jump, and
/// it leaves by shrinking its size, so it never jumps either. The phase keeps running while it
/// leaves and while the window is hidden, so a loop resumes where it was.
#[derive(Clone, Copy)]
struct Loop {
    amp: Tween,
    degrees: f32,
    period: f32,
    /// Milliseconds into the cycle; negative while the start is still waiting.
    phase: f32,
}

impl Loop {
    fn new(degrees: f32, period: f32) -> Self {
        Self {
            amp: Tween::at(0.0),
            degrees,
            period,
            phase: 0.0,
        }
    }

    fn running(&self) -> bool {
        self.amp.value() > 0.0 || self.amp.end() > 0.0
    }

    fn start(&mut self, delay: f32) {
        if self.amp.end() > 0.0 {
            return;
        }
        if self.amp.value() > 0.0 {
            // Still leaving: grow back and keep the cycle.
            self.amp.to(1.0, delay, 150.0, OUT);
            return;
        }
        // The size jumps to full at the zero crossing, which is when the phase reaches 0.
        self.phase = -delay;
        self.amp.to(1.0, delay, 0.0, LINEAR);
    }

    fn stop(&mut self) {
        if self.amp.end() > 0.0 {
            self.amp.to(0.0, 0.0, 150.0, OUT);
        }
    }

    fn angle(&self) -> f32 {
        self.amp.value() * self.degrees * (TAU * self.phase.max(0.0) / self.period).sin()
    }
}

/// What the app tells the kite to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    Disconnected,
    Connecting,
    Connected,
    /// The link dropped and is being retried: the kite falls and rests.
    Dropped,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Connect,
    Ready,
    Stop,
    Lost,
    Fail,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Str {
    Taut,
    Slack,
    Snapped,
}

/// Everything the window draws, in stage units, already moved by the rig.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    /// Top, left, right and bottom of the diamond, then the tail's two controls and its end, then
    /// the taut string's control and its end (the other end is the hand).
    pub points: [(f32, f32); 9],
    pub taut: f32,
    pub slack: f32,
    pub snapped: f32,
    pub badge: f32,
    pub badge_scale: f32,
    /// Everything fades with this in the reduced-motion variant.
    pub art: f32,
}

pub struct Kite {
    state: Show,
    /// What was asked for last, so asking again for it does nothing.
    asked: Show,
    pose: [Tween; 3],
    taut: Tween,
    slack: Tween,
    snapped: Tween,
    badge: Tween,
    badge_scale: Tween,
    art: Tween,
    wobble: Loop,
    sway_rig: Loop,
    sway_rock: Loop,
    flutter: Loop,
    rate: f32,
    /// Time since the link dropped; the fall is Disconnected once it is over.
    dropped: f32,
    reduced: bool,
    /// Reduced motion: when the old picture has faded out, show this.
    swap: Option<(f32, Show)>,
}

/// How long the fall of a dropped link lasts.
const FALL: f32 = 900.0;

impl Default for Kite {
    fn default() -> Self {
        Self::new()
    }
}

impl Kite {
    pub fn new() -> Self {
        let mut k = Self {
            state: Show::Disconnected,
            asked: Show::Disconnected,
            pose: [Tween::at(0.0), Tween::at(0.0), Tween::at(0.0)],
            taut: Tween::at(0.0),
            slack: Tween::at(0.0),
            snapped: Tween::at(0.0),
            badge: Tween::at(0.0),
            badge_scale: Tween::at(0.9),
            art: Tween::at(1.0),
            wobble: Loop::new(6.0, 700.0),
            sway_rig: Loop::new(1.6, 4800.0),
            sway_rock: Loop::new(3.5, 4800.0),
            flutter: Loop::new(10.0, 1400.0),
            rate: 1.0,
            dropped: 0.0,
            reduced: false,
            swap: None,
        };
        k.rest(Show::Disconnected);
        k
    }

    /// Tells the kite what to show. The state changes at once; the picture follows.
    pub fn show(&mut self, want: Show) {
        if want == self.asked {
            return;
        }
        self.asked = want;
        let event = match (self.state, want) {
            (Show::Disconnected | Show::Dropped | Show::Error, Show::Connecting) => Event::Connect,
            (_, Show::Connected) => Event::Ready,
            (Show::Connecting | Show::Connected | Show::Error, Show::Disconnected) => Event::Stop,
            (Show::Connecting | Show::Connected, Show::Dropped) => Event::Lost,
            (_, Show::Error) => Event::Fail,
            _ => return,
        };
        self.apply(event);
    }

    /// Finishes whatever is moving at once, for a window that opens after the state changed.
    pub fn settle(&mut self) {
        for _ in 0..60 {
            self.tick(100.0);
        }
    }

    /// Throughput in Mbit/s, already averaged over a second by the caller; sets how fast the tail
    /// flutters.
    pub fn throughput(&mut self, mbit: f32) {
        let u = ((1.0 + mbit.max(0.0)).log10() / 101f32.log10()).clamp(0.0, 1.0);
        self.rate = 1.0 + 2.1 * u;
    }

    pub fn set_reduced(&mut self, reduced: bool) {
        if reduced == self.reduced {
            return;
        }
        self.reduced = reduced;
        self.swap = None;
        self.art.set(1.0);
        if reduced {
            self.rest(self.state);
        } else if self.state == Show::Connected {
            self.loops(0.0);
        } else if self.state == Show::Connecting {
            self.wobble.start(0.0);
        }
    }

    /// Whether the picture can still change by itself, so it needs frames. At rest it does not, and
    /// then nothing should be scheduled.
    pub fn animating(&self) -> bool {
        let tweens = [
            &self.pose[0],
            &self.pose[1],
            &self.pose[2],
            &self.taut,
            &self.slack,
            &self.snapped,
            &self.badge,
            &self.badge_scale,
            &self.art,
        ];
        self.swap.is_some()
            || self.state == Show::Dropped
            || tweens.iter().any(|t| !t.done())
            || [self.wobble, self.sway_rig, self.sway_rock, self.flutter]
                .iter()
                .any(Loop::running)
    }

    /// Moves time on by `dt` ms and returns what to draw.
    pub fn tick(&mut self, dt: f32) -> Frame {
        // A frame after a long pause (the window was hidden) must not leap.
        let dt = dt.clamp(0.0, 100.0);
        for t in self.tweens_mut() {
            t.t += dt;
        }
        // The tail's phase runs faster with the rate, so a change of rate never restarts it. It
        // does so only once the loop has started: before that the start would jump.
        let flutter_runs = self.flutter.phase >= 0.0;
        for l in [
            &mut self.wobble,
            &mut self.sway_rig,
            &mut self.sway_rock,
            &mut self.flutter,
        ] {
            l.amp.t += dt;
            l.phase += dt;
        }
        if flutter_runs {
            self.flutter.phase += dt * (self.rate - 1.0);
        }
        // A sine repeats, so the counter wraps and never loses precision over a long session.
        for l in [
            &mut self.wobble,
            &mut self.sway_rig,
            &mut self.sway_rock,
            &mut self.flutter,
        ] {
            if l.phase >= l.period {
                l.phase %= l.period;
            }
        }
        if self.state == Show::Dropped {
            self.dropped += dt;
            if self.dropped >= FALL {
                self.state = Show::Disconnected;
            }
        }
        if let Some((left, to)) = self.swap {
            if left <= dt {
                self.swap = None;
                self.rest(to);
                self.art.go(&[leg(0.0, 120.0, 1.0, OUT)]);
            } else {
                self.swap = Some((left - dt, to));
            }
        }
        self.frame()
    }

    fn tweens_mut(&mut self) -> [&mut Tween; 9] {
        let [x, y, r] = &mut self.pose;
        [
            x,
            y,
            r,
            &mut self.taut,
            &mut self.slack,
            &mut self.snapped,
            &mut self.badge,
            &mut self.badge_scale,
            &mut self.art,
        ]
    }

    pub fn frame(&self) -> Frame {
        let rock = self.wobble.angle() + self.sway_rock.angle();
        let rig = self.sway_rig.angle();
        let tail = self.flutter.angle();
        let (tx, ty, turn) = (
            self.pose[0].value(),
            self.pose[1].value(),
            self.pose[2].value(),
        );
        // Tail, rock and pose turn about the bottom vertex, the pose then moves, and the rig turns
        // about the hand.
        let kite = |p: P, extra: f32| {
            let p = turn_about(p, B, extra + rock + turn);
            turn_about(P(p.0 + tx, p.1 + ty), H, rig)
        };
        let string = |p: P| turn_about(p, H, rig);
        let pt = |p: P| (p.0, p.1);
        Frame {
            points: [
                pt(kite(T, 0.0)),
                pt(kite(L, 0.0)),
                pt(kite(R, 0.0)),
                pt(kite(B, 0.0)),
                pt(kite(TAIL[0], tail)),
                pt(kite(TAIL[1], tail)),
                pt(kite(TAIL[2], tail)),
                pt(string(TAUT_CONTROL)),
                pt(string(B)),
            ],
            taut: self.taut.value(),
            slack: self.slack.value(),
            snapped: self.snapped.value(),
            badge: self.badge.value(),
            badge_scale: self.badge_scale.value(),
            art: self.art.value(),
        }
    }

    fn apply(&mut self, e: Event) {
        let next = match (self.state, e) {
            (Show::Disconnected, Event::Connect) => Show::Connecting,
            (Show::Disconnected, Event::Ready) => Show::Connected,
            (Show::Disconnected, Event::Fail) => Show::Error,
            (Show::Connecting, Event::Ready) => Show::Connected,
            (Show::Connecting, Event::Stop) => Show::Disconnected,
            (Show::Connecting, Event::Lost) => Show::Dropped,
            (Show::Connecting, Event::Fail) => Show::Error,
            (Show::Connected, Event::Stop) => Show::Disconnected,
            (Show::Connected, Event::Lost) => Show::Dropped,
            (Show::Connected, Event::Fail) => Show::Error,
            (Show::Dropped, Event::Connect) => Show::Connecting,
            (Show::Dropped, Event::Ready) => Show::Connected,
            (Show::Dropped, Event::Fail) => Show::Error,
            (Show::Error, Event::Connect) => Show::Connecting,
            (Show::Error, Event::Ready) => Show::Connected,
            (Show::Error, Event::Stop) => Show::Disconnected,
            _ => return,
        };
        let from = self.state;
        self.state = next;
        if self.reduced {
            // Fade out, then show the new picture under a fade in. A new event starts from the
            // opacity on screen, and a dropped link shows Disconnected straight away.
            if next == Show::Dropped {
                self.state = Show::Disconnected;
            }
            self.art.go(&[leg(0.0, 80.0, 0.0, LINEAR)]);
            self.swap = Some((80.0, self.state));
            return;
        }
        match e {
            Event::Connect => self.connect(),
            Event::Ready => self.ready(from),
            Event::Stop => self.stop(),
            Event::Lost => self.lost(),
            Event::Fail => self.fail(from),
        }
    }

    fn move_pose(&mut self, to: Pose, dur: f32, curve: Curve) {
        for (t, v) in self.pose.iter_mut().zip([to.0, to.1, to.2]) {
            t.to(v, 0.0, dur, curve);
        }
    }

    fn loops(&mut self, pose_ms: f32) {
        self.sway_rig.start(pose_ms);
        // A one-eighth cycle behind the rig.
        self.sway_rock.start(pose_ms + 600.0);
        self.flutter.start(pose_ms);
    }

    fn loops_leave(&mut self) {
        // The next session starts its tail at the idle rate until the first reading arrives.
        self.rate = 1.0;
        for l in [
            &mut self.wobble,
            &mut self.sway_rig,
            &mut self.sway_rock,
            &mut self.flutter,
        ] {
            l.stop();
        }
    }

    /// The three strings crossfade to `to` after `hold` ms.
    fn strings(&mut self, to: Str, hold: f32) {
        for (t, which) in [
            (&mut self.taut, Str::Taut),
            (&mut self.slack, Str::Slack),
            (&mut self.snapped, Str::Snapped),
        ] {
            t.to(f32::from(which == to), hold, 250.0, OUT);
        }
    }

    fn hide_badge(&mut self) {
        self.badge.to(0.0, 0.0, 120.0, OUT);
        self.badge_scale.to(0.9, 0.0, 120.0, OUT);
    }

    fn connect(&mut self) {
        self.move_pose(CONNECTING, 450.0, OUT);
        self.strings(Str::Taut, 150.0);
        self.hide_badge();
        self.loops_leave();
        self.wobble.start(450.0);
    }

    fn ready(&mut self, from: Show) {
        let pose_ms = if from == Show::Connecting {
            350.0
        } else {
            450.0
        };
        self.move_pose(CONNECTED, pose_ms, OUT);
        self.strings(Str::Taut, 150.0);
        self.hide_badge();
        self.wobble.stop();
        self.loops(pose_ms);
    }

    fn stop(&mut self) {
        self.move_pose(REST, 600.0, SINE);
        self.strings(Str::Slack, 250.0);
        self.hide_badge();
        self.loops_leave();
    }

    /// The dip and the drift are one move: an interruption cancels both.
    fn fall(&mut self) {
        for (t, (dip, rest)) in
            self.pose
                .iter_mut()
                .zip([(DIP.0, REST.0), (DIP.1, REST.1), (DIP.2, REST.2)])
        {
            t.go(&[leg(0.0, 180.0, dip, OUT), leg(0.0, 720.0, rest, SINE)]);
        }
        self.loops_leave();
    }

    fn lost(&mut self) {
        self.fall();
        self.strings(Str::Slack, 300.0);
        self.dropped = 0.0;
    }

    fn fail(&mut self, from: Show) {
        if matches!(from, Show::Connecting | Show::Connected) {
            // The same fall, then the string snaps and the badge arrives as the kite lands.
            self.fall();
            self.taut.to(0.0, 300.0, 250.0, OUT);
            self.slack
                .go(&[leg(300.0, 250.0, 1.0, OUT), leg(250.0, 120.0, 0.0, OUT)]);
            self.snapped.to(1.0, 800.0, 120.0, OUT);
            self.badge.to(1.0, 750.0, 160.0, OUT);
            self.badge_scale.to(1.0, 750.0, 160.0, OUT);
        } else {
            // At rest only the last two steps run; from a fall already on its way they keep
            // their place in it.
            let into = if from == Show::Dropped {
                self.dropped
            } else {
                FALL
            };
            let wait = |at: f32| (at - into).max(0.0);
            if from == Show::Dropped {
                // The fall's own crossfade to the slack string carries on, then it snaps.
                let fade = wait(300.0);
                self.taut.to(0.0, fade, 250.0, OUT);
                self.slack.go(&[
                    leg(fade, 250.0, 1.0, OUT),
                    leg((wait(800.0) - fade - 250.0).max(0.0), 120.0, 0.0, OUT),
                ]);
            } else {
                self.slack.to(0.0, 0.0, 120.0, OUT);
                self.taut.to(0.0, 0.0, 120.0, OUT);
            }
            self.snapped.to(1.0, wait(800.0), 120.0, OUT);
            self.badge.to(1.0, wait(750.0), 160.0, OUT);
            self.badge_scale.to(1.0, wait(750.0), 160.0, OUT);
        }
    }

    /// Sets the resting picture of `s` at once, with no loops.
    fn rest(&mut self, s: Show) {
        let (pose, string, badge) = match s {
            Show::Connecting => (CONNECTING, Str::Taut, false),
            Show::Connected => (CONNECTED, Str::Taut, false),
            Show::Error => (REST, Str::Snapped, true),
            Show::Disconnected | Show::Dropped => (REST, Str::Slack, false),
        };
        for (t, v) in self.pose.iter_mut().zip([pose.0, pose.1, pose.2]) {
            t.set(v);
        }
        for (t, which) in [
            (&mut self.taut, Str::Taut),
            (&mut self.slack, Str::Slack),
            (&mut self.snapped, Str::Snapped),
        ] {
            t.set(f32::from(which == string));
        }
        self.badge.set(f32::from(badge));
        self.badge_scale.set(if badge { 1.0 } else { 0.9 });
        for l in [
            &mut self.wobble,
            &mut self.sway_rig,
            &mut self.sway_rock,
            &mut self.flutter,
        ] {
            l.amp.set(0.0);
        }
    }
}

fn turn_about(p: P, pivot: P, degrees: f32) -> P {
    if degrees == 0.0 {
        return p;
    }
    let (s, c) = degrees.to_radians().sin_cos();
    let (x, y) = (p.0 - pivot.0, p.1 - pivot.1);
    P(pivot.0 + x * c - y * s, pivot.1 + x * s + y * c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(k: &mut Kite, ms: f32) -> Frame {
        let mut f = k.frame();
        let mut t = 0.0;
        while t < ms {
            f = k.tick(10.0);
            t += 10.0;
        }
        f
    }

    fn near(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 0.05 && (a.1 - b.1).abs() < 0.05
    }

    #[test]
    fn the_curves_start_at_zero_end_at_one_and_never_dip() {
        for c in [OUT, SINE, LINEAR] {
            assert!(c.at(0.0).abs() < 1e-3 && (c.at(1.0) - 1.0).abs() < 1e-3);
            let mut last = 0.0;
            for i in 0..=100 {
                let y = c.at(i as f32 / 100.0);
                assert!(y >= last - 1e-3, "{y} < {last}");
                last = y;
            }
        }
        // The spec's out curve is mostly done by a third of the way.
        assert!(OUT.at(0.33) > 0.8);
    }

    #[test]
    fn at_rest_nothing_is_scheduled() {
        let k = Kite::new();
        assert!(!k.animating());
        let f = k.frame();
        assert_eq!((f.slack, f.taut, f.snapped, f.badge), (1.0, 0.0, 0.0, 0.0));
        // The resting pose is the spec's: a move of (-18, 9) and a turn of 72 degrees.
        assert!(near(f.points[3], (20.0, 50.0)), "{:?}", f.points[3]);
    }

    #[test]
    fn connected_is_the_brand_drawing() {
        let mut k = Kite::new();
        k.show(Show::Connected);
        // At the end of the settle the loops are at their zero crossing.
        let f = run(&mut k, 450.0);
        assert!(near(f.points[0], (38.0, 1.0)), "{:?}", f.points[0]);
        assert_eq!((f.taut, f.slack), (1.0, 0.0));
    }

    #[test]
    fn connecting_lifts_and_then_wobbles_from_its_zero_crossing() {
        let mut k = Kite::new();
        k.show(Show::Connecting);
        assert!(k.animating());
        let at = run(&mut k, 450.0);
        assert!(near(at.points[3], (37.0, 43.0)), "{:?}", at.points[3]);
        let before = run(&mut k, 10.0).points[0];
        let after = run(&mut k, 10.0).points[0];
        // No jump when the wobble starts, and it keeps moving while connecting.
        assert!((before.0 - after.0).abs() < 1.0 && (before.1 - after.1).abs() < 1.0);
        assert!(k.animating());
    }

    #[test]
    fn stop_comes_to_rest_and_then_asks_for_no_frames() {
        let mut k = Kite::new();
        k.show(Show::Connected);
        run(&mut k, 1500.0);
        k.show(Show::Disconnected);
        let f = run(&mut k, 800.0);
        assert!(near(f.points[3], (20.0, 50.0)), "{:?}", f.points[3]);
        assert_eq!((f.slack, f.taut), (1.0, 0.0));
        assert!(!k.animating());
    }

    #[test]
    fn an_interrupt_carries_on_from_where_the_kite_is() {
        let mut k = Kite::new();
        k.show(Show::Connecting);
        let mid = run(&mut k, 200.0);
        k.show(Show::Disconnected);
        let first = k.tick(10.0);
        // No snap to a nominal pose: a frame later the kite is still close to where it was.
        let d = ((first.points[3].0 - mid.points[3].0).powi(2)
            + (first.points[3].1 - mid.points[3].1).powi(2))
        .sqrt();
        assert!(d < 3.0, "{d}");
        // Ten connects and stops in a second never stack.
        for i in 0..10 {
            k.show(if i % 2 == 0 {
                Show::Connecting
            } else {
                Show::Disconnected
            });
            k.tick(50.0);
        }
        run(&mut k, 2000.0);
        assert!(!k.animating());
        assert!(near(k.frame().points[3], (20.0, 50.0)));
    }

    #[test]
    fn a_dropped_link_dips_then_rests_and_is_disconnected_after_900_ms() {
        let mut k = Kite::new();
        k.show(Show::Connected);
        run(&mut k, 1000.0);
        k.show(Show::Dropped);
        assert!(k.animating());
        run(&mut k, 190.0);
        // Inside the dip the bottom vertex is lower than when connected.
        assert!(k.frame().points[3].1 > 44.0);
        run(&mut k, 800.0);
        assert!(near(k.frame().points[3], (20.0, 50.0)));
        run(&mut k, 300.0);
        assert!(!k.animating());
        // A connect works from the end of the fall.
        k.show(Show::Connecting);
        assert!(k.animating());
    }

    #[test]
    fn a_failure_from_the_air_falls_snaps_and_shows_the_badge_last() {
        let mut k = Kite::new();
        k.show(Show::Connected);
        run(&mut k, 1000.0);
        k.show(Show::Error);
        let early = run(&mut k, 400.0);
        assert!(early.snapped < 0.5 && early.badge < 0.1);
        let late = run(&mut k, 700.0);
        assert!(late.snapped > 0.99 && late.badge > 0.99 && late.slack < 0.01);
        assert!(near(late.points[3], (20.0, 50.0)));
        assert_eq!(late.badge_scale, 1.0);
    }

    #[test]
    fn a_failure_at_rest_snaps_at_once_and_leaving_the_error_hides_the_badge() {
        let mut k = Kite::new();
        k.show(Show::Error);
        let f = run(&mut k, 200.0);
        assert!(f.snapped > 0.99 && f.badge > 0.99);
        k.show(Show::Connecting);
        let f = run(&mut k, 500.0);
        assert!(f.badge < 0.01 && f.taut > 0.99 && f.snapped < 0.01);
    }

    #[test]
    fn asking_for_what_is_shown_does_nothing() {
        let mut k = Kite::new();
        k.show(Show::Disconnected);
        assert!(!k.animating());
        k.show(Show::Connected);
        run(&mut k, 600.0);
        let a = k.frame();
        k.show(Show::Connected);
        assert_eq!(a, k.frame());
    }

    #[test]
    fn the_flutter_follows_throughput_and_keeps_its_phase() {
        let mut k = Kite::new();
        k.throughput(0.0);
        assert!((k.rate - 1.0).abs() < 1e-6);
        // 1 Mbit/s is about 1060 ms a cycle, 10 about 670 and 100 or more 452.
        for (mbit, period) in [(1.0, 1060.0), (10.0, 670.0), (100.0, 452.0), (900.0, 452.0)] {
            k.throughput(mbit);
            let got = 1400.0 / k.rate;
            assert!((got - period).abs() < 8.0, "{mbit}: {got}");
        }
        k.show(Show::Connected);
        run(&mut k, 1500.0);
        let before = k.flutter.phase;
        k.throughput(50.0);
        k.tick(10.0);
        // The phase moved on by a little; it did not restart.
        assert!(k.flutter.phase > before && k.flutter.phase - before < 40.0);
    }

    #[test]
    fn a_frame_after_a_pause_does_not_leap() {
        let mut k = Kite::new();
        k.show(Show::Connected);
        run(&mut k, 2000.0);
        let before = k.frame();
        let after = k.tick(60_000.0);
        let moved = ((after.points[0].0 - before.points[0].0).powi(2)
            + (after.points[0].1 - before.points[0].1).powi(2))
        .sqrt();
        assert!(moved < 2.0, "{moved}");
    }

    #[test]
    fn reduced_motion_fades_through_without_travel_or_loops() {
        let mut k = Kite::new();
        k.set_reduced(true);
        k.show(Show::Connected);
        // Fading out for 80 ms: the kite has not moved.
        let f = run(&mut k, 40.0);
        assert!(f.art < 1.0);
        assert!(near(f.points[3], (20.0, 50.0)));
        // Then it is the connected picture at once, fading in; 200 ms in all.
        let f = run(&mut k, 200.0);
        assert_eq!(f.art, 1.0);
        assert!(near(f.points[0], (38.0, 1.0)));
        assert_eq!((f.taut, f.slack), (1.0, 0.0));
        // Nothing is scheduled afterwards, and nothing loops.
        assert!(!k.animating());
        // A dropped link shows Disconnected straight away.
        k.show(Show::Dropped);
        run(&mut k, 300.0);
        assert!(near(k.frame().points[3], (20.0, 50.0)));
        assert!(!k.animating());
        // An error appears at full opacity under the same fade.
        k.show(Show::Error);
        let f = run(&mut k, 300.0);
        assert!(f.badge == 1.0 && f.art == 1.0);
    }

    #[test]
    fn the_tail_starts_without_a_jump_even_at_a_high_rate() {
        let mut k = Kite::new();
        k.show(Show::Connected);
        k.throughput(100.0);
        // The settle is over at 450 ms; look at the frames around the start of the flutter.
        run(&mut k, 430.0);
        let mut last = k.frame().points[6];
        let mut worst = 0.0f32;
        for _ in 0..8 {
            let p = k.tick(10.0).points[6];
            worst = worst.max(((p.0 - last.0).powi(2) + (p.1 - last.1).powi(2)).sqrt());
            last = p;
        }
        assert!(worst < 0.8, "{worst}");
    }

    #[test]
    fn a_failure_during_the_fall_keeps_the_strings_in_step() {
        let mut k = Kite::new();
        k.show(Show::Connected);
        run(&mut k, 1000.0);
        k.show(Show::Dropped);
        run(&mut k, 100.0);
        k.show(Show::Error);
        // The taut string goes slack with the fall; it does not wait for the snap.
        let f = run(&mut k, 450.0);
        assert!(f.taut < 0.1 && f.slack > 0.9, "{f:?}");
        let f = run(&mut k, 700.0);
        assert!(
            f.snapped > 0.99 && f.badge > 0.99 && f.slack < 0.01,
            "{f:?}"
        );
    }

    #[test]
    fn turning_reduced_motion_on_mid_loop_stops_the_motion() {
        let mut k = Kite::new();
        k.show(Show::Connected);
        run(&mut k, 2000.0);
        k.set_reduced(true);
        assert!(!k.animating());
        assert!(near(k.frame().points[0], (38.0, 1.0)));
    }
}
