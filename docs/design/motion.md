# Kite motion

The kite shows what the link is doing. One state machine, one set of numbers, built natively on
every platform (Slint, Android, SwiftUI, website). Nothing here is a web-only idea: if a platform
cannot do something exactly, it keeps the intent and the timings.

Motion has one job: state indication. States change a few times per session, so the animation is
allowed to be visible, but it never blocks anything. The logical state changes at the event; the
kite only shows it. Motion is never the only signal: the pose, the string, the badge, the tray icon
and a text label all say the same thing.

## States

| State | Pose | String | Motion while in the state |
| --- | --- | --- | --- |
| Disconnected | resting low, lying on its side | slack | none |
| Connecting | lifted, leaning back | taut | small wobble |
| Connected | up, upright | taut | slow sway, tail flutter follows throughput |
| Link dropped | dips, then drifts down | goes slack | none; lasts 900 ms, then it is Disconnected |
| Error | resting low | snapped, badge | none |

Link dropped is only a visual transition. The app, the tray icon and the notification switch to
Disconnected at the moment the link is lost.

Events: `connect`, `ready`, `stop` (user disconnects), `lost` (link dropped), `fail`.

| From | `connect` | `ready` | `stop` | `lost` | `fail` |
| --- | --- | --- | --- | --- | --- |
| Disconnected | Connecting | Connected | | | Error |
| Connecting | | Connected | Disconnected | Link dropped | Error |
| Connected | | | Disconnected | Link dropped | Error |
| Link dropped | Connecting | Connected | | | Error |
| Error | Connecting | Connected | Disconnected | | |

An empty cell is ignored. Every other cell is accepted at any moment, including in the middle of
an animation.

## Rig

The kite is drawn on a 64 by 64 stage in its Connected pose (`assets/brand/kite.svg`). The string is
anchored at the kite's bottom vertex **B = (38, 41)** and at the hand **H = (6, 62)**. Layers, from
the outside in:

| Layer | Contains | Animated properties | Pivot |
| --- | --- | --- | --- |
| art | everything but the badge | opacity (reduced motion only) | none |
| slack, snapped | two baked string paths | opacity | none |
| rig | taut string and the kite | rotate (sway) | H |
| pose | kite and tail | translate, rotate | B |
| rock | kite and tail | rotate (wobble, sway) | B |
| tail | tail | rotate (flutter) | B |
| badge | error badge | opacity, scale | its centre |

Poses are a translation plus a rotation about B, in stage units:

| Pose | translate | rotate |
| --- | --- | --- |
| Rest | -18, 9 | 72° |
| Connecting | -1, 2 | -8° |
| Connected | 0, 0 | 0° |
| Dip (only inside Link dropped) | 0, 5 | 6° |

The string never changes shape while it moves. Slack, taut and snapped are three baked paths and
the app crossfades between them, so every property that animates is a transform or an opacity.
The crossfade starts late enough that the kite is already near its target, which hides the moment
when the string end and the kite are not exactly together.

## Curves and durations

One set of curves, copied into one constants file per platform.

| Name | Curve | Used for |
| --- | --- | --- |
| `out` | `cubic-bezier(0.23, 1, 0.32, 1)` | lifts, settles, fades in, calming a loop |
| `sine` | `cubic-bezier(0.37, 0, 0.63, 1)` | drifts down, loops (ease-in-out sine) |
| `linear` | linear | fade out in reduced motion |

Nothing uses `ease-in`. Durations are longer than a button press because this is a picture of a
state, not feedback to a tap. The button the user pressed answers in its own 100 to 160 ms.

## Transitions

Times are in milliseconds from the event. "Delay" is when that part starts. Pose, string and badge
start together unless a delay says otherwise.

**Connect** (Disconnected, Error, or Link dropped to Connecting)

| Part | Delay | Duration | Motion | Curve |
| --- | --- | --- | --- | --- |
| Pose to Connecting | 0 | 450 | lift | `out` |
| String slack or snapped to taut | 150 | 250 | opacity crossfade | `out` |
| Badge (from Error) | 0 | 120 | opacity to 0, scale to 0.9 | `out` |
| Wobble loop starts | 450 | | see Loops | |

**Ready** (to Connected)

| Part | Delay | Duration | Motion | Curve |
| --- | --- | --- | --- | --- |
| Pose to Connected | 0 | 350 from Connecting, 450 from anywhere else | settle | `out` |
| String to taut (if not already) | 150 | 250 | crossfade | `out` |
| Wobble loop exits | 0 | 150 | offset to 0 | `out` |
| Sway and tail loops start | end of the pose | | see Loops | |

**Stop** (to Disconnected)

| Part | Delay | Duration | Motion | Curve |
| --- | --- | --- | --- | --- |
| Pose to Rest | 0 | 600 | drift down | `sine` |
| String taut to slack | 250 | 250 | crossfade | `out` |
| Loops exit | 0 | 150 | offset to 0 | `out` |

**Lost** (Link dropped, then Disconnected)

One animation with two segments, 900 ms in total.

| Part | Delay | Duration | Motion | Curve |
| --- | --- | --- | --- | --- |
| Pose to Dip | 0 | 180 | quick dip | `out` |
| Pose from Dip to Rest | 180 | 720 | drift down | `sine` |
| String taut to slack | 300 | 250 | crossfade | `out` |
| Loops exit | 0 | 150 | offset to 0 | `out` |

**Fail** (to Error)

From a lifted pose it is the same fall as Lost, then the string snaps and the badge appears as the
kite lands. From a resting pose only the last two rows run, from delay 0.

| Part | Delay | Duration | Motion | Curve |
| --- | --- | --- | --- | --- |
| Fall | 0 | 900 | as Lost | |
| String taut to slack | 300 | 250 | crossfade | `out` |
| String slack to snapped | 800 | 120 | crossfade | `out` |
| Badge | 750 | 160 | opacity to 1, scale 0.9 to 1 | `out` |

Choreography, in short: the pose leads, the string follows once the kite is most of the way there,
the badge arrives last, and loops start only after the pose has landed so they never fight it. Exits
are simultaneous and shorter than entrances. Nothing is staggered for its own sake.

## Loops

Every loop is a sine on `rotate`, starting at its zero crossing, so it joins the pose with no jump.
A loop that is not a pure sine is a bug.

| Loop | Layer | Amplitude | Period | Starts |
| --- | --- | --- | --- | --- |
| Wobble (Connecting) | rock | 6° | 700 ms | when the lift ends |
| Sway (Connected) | rig | 1.6° | 4800 ms | when the settle ends |
| Sway (Connected) | rock | 3.5° | 4800 ms | 600 ms after the rig, a one-eighth cycle behind |
| Flutter (Connected) | tail | 10° | 1400 ms ÷ rate | when the settle ends |

**Flutter follows throughput.** Smooth the measured rate with a 1 s moving average, sample it at
most twice a second, and map it:

```
u    = clamp(log10(1 + mbit_per_s) / log10(101), 0, 1)
rate = 1 + 2.1 * u
period = 1400 / rate
```

That gives 1400 ms when idle, about 1060 ms at 1 Mbit/s, 670 ms at 10 and 452 ms from 100 Mbit/s up.
The clamp matters: faster than about 2.2 cycles a second reads as shaking. A change of rate must keep
the phase, never restart the cycle. On the web that is `updatePlaybackRate`. Elsewhere, accumulate
`phase += dt * rate` on a 30 fps tick.

## Interruption

1. Any event is accepted at any moment. There is no queue and no debounce. The last event wins. An
   event that matches the current target does nothing.
2. A new animation starts from what is on screen now, for every property. It never snaps to the
   nominal pose first and never restarts from zero.
3. Loops sit on top of the pose. When one ends it eases its current offset to zero in 150 ms
   (`out`) while the pose animation already runs. The offset is at most 10°, so it is absorbed.
4. A two-part animation (the dip and the drift) is one animation. An interrupt cancels both parts.
5. Rapid events, such as ten connects and stops in a second, never stack: each layer holds at most
   one pose animation and one loop.
6. Logic never waits for an animation to finish. Do not delay a state change, a notification or a
   tray icon to let the kite land.

## Reduced motion

Follow the OS setting, read at start and again when it changes. In the reduced variant:

- no loops: no wobble, no sway, no flutter;
- no travel: the kite does not lift, dip or drift;
- every state change is a fade through: the art fades out in 80 ms (`linear`), the new pose, string
  and badge are set while it is invisible, then it fades in over 120 ms (`out`). 200 ms in total;
- Link dropped skips the dip and shows Disconnected straight away;
- the badge appears at full opacity under the same fade.

The same event rules apply: a new event during the fade just starts a new fade from the current
opacity.

## Performance

These are requirements for every platform, not tuning notes.

- Animate transform (translate, rotate, scale) and opacity only. No path morphing, no size or
  layout changes, no filters or blur.
- Zero CPU when idle. In Disconnected, Error and every reduced-motion state nothing is scheduled:
  no timer, no frame callback, no running animator.
- Loops run only while the kite can be seen. Pause them when the window is minimised, hidden,
  covered, scrolled off screen, or the screen is off. Resume them without restarting the cycle.
- Cap loops at 30 fps on platforms where the app drives the frames. The compositor decides on the
  web.
- One vector source. No raster frames, no sprite sheets, no GIF or video, no Lottie. Precompute any
  lookup table once, and allocate nothing per frame.
- Tray and notification icons never animate. A state change swaps the icon once.
- Each platform PR records CPU while Connected with the window visible, and confirms nothing wakes
  up while the window is hidden.

## Static surfaces

The tray icons (`assets/brand/tray/`), the notification silhouette and the app icon do not move.
The four tray states differ by shape as well as colour: Disconnected is a grey outline lying low,
Connecting is half filled with a dashed string, Connected is solid and upright, Error is the grey
outline with a badge.

## Per platform

Same layers, same numbers. Put the curves and durations in one constants file or object.

**Slint (desktop).** Facets and strings are `Path` elements with the SVG path data from the brand
files. Poses are `states` with `animate` on `x`, `y`, `rotation-angle` and `opacity`, using
`easing: cubic-bezier(...)` from the table. Slint retargets a property from its current value. The
wobble and sway are `animate rotation-angle` with `iteration-count: -1` and `direction: alternate`
between the two amplitudes, using the `sine` curve. The tail needs a changing rate, so it is driven
by a 33 ms `Timer` that accumulates phase. Stop the timer and the loops when the window is hidden to
the tray.

**Android.** Each layer is a plain `VectorDrawable` in its own `ImageView`. Pose, string and badge
use `ViewPropertyAnimator` and `ObjectAnimator` (`translationX`, `translationY`, `rotation` with a
pivot, `alpha`), which start from the current value. Loops are `ObjectAnimator` with
`repeatCount = INFINITE`, `REVERSE` and a `PathInterpolator`. The tail rate uses a
`Choreographer.FrameCallback` that accumulates phase. `AnimatedVectorDrawable` has fixed start
values and cannot retarget, so it is only allowed for a loop, and only if profiling shows the
property animators costing main-thread frames. No Lottie. Pause loops in `onStop` and when the
screen turns off. Read `Settings.Global.ANIMATOR_DURATION_SCALE`: zero means reduced motion.

**SwiftUI (iPhone).** One `Shape` per layer, built from the same path data. Poses use `.offset`,
`.rotationEffect(_:anchor:)` and `.opacity` with
`Animation.timingCurve(0.23, 1, 0.32, 1, duration: 0.45)`. SwiftUI retargets from the presentation
value. The dip and the drift are a `keyframeAnimator`. Loops and the tail run inside a `TimelineView`
with `.animation(minimumInterval: 1/30, paused: !visible)`, where `visible` follows `scenePhase`.
Reduced motion is `@Environment(\.accessibilityReduceMotion)`.

**Website.** One inline SVG. Layers are `<g>` with `transform-box: view-box` and a `transform-origin`
at the pivot. Poses use CSS transitions on the individual `translate`, `rotate` and `opacity`
properties. Loops are CSS keyframes sampled from the sine, or Web Animations with an infinite
iteration count, so the tail can use `updatePlaybackRate`. Pause loops with `animation-play-state`
when the document is hidden or the SVG is off screen (an `IntersectionObserver`). Reduced motion is
`@media (prefers-reduced-motion: reduce)`: static Connected pose, no loops. Never drive a child's
transform from a CSS variable on its parent.

## Checking a build

- Play every state at a quarter of the speed and step through it frame by frame.
- Interrupt every transition at 25, 50 and 75 percent with every event. The kite must carry on from
  where it is, with no jump and no leftover loop.
- Turn reduced motion on in the middle of a loop.
- Hide the window while Connected and check that CPU use and wakeups drop to zero.
- Look at the 16 px tray icons and the notification silhouette on light and dark backgrounds.
