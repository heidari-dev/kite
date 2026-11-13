package io.github.heidaridev.kite

import android.animation.Animator
import android.animation.Keyframe
import android.animation.ObjectAnimator
import android.animation.PropertyValuesHolder
import android.animation.TimeInterpolator
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.database.ContentObserver
import android.graphics.Rect
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.provider.Settings
import android.util.AttributeSet
import android.view.Choreographer
import android.view.LayoutInflater
import android.view.View
import android.view.ViewTreeObserver
import android.widget.FrameLayout
import io.github.heidaridev.kite.KiteMotion as M
import kotlin.math.PI
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.min
import kotlin.math.sin

/**
 * The animated kite of docs/design/motion.md: one vector layer per ImageView, moved with
 * property animators; the loops (sway, wobble, tail flutter) run from one frame callback that
 * draws at most 30 times a second and runs only while the kite can be seen.
 *
 * The caller says what the link is doing with [setTarget]; the view works out the event. Nothing
 * here waits for an animation: a new target starts from what is on screen.
 */
class KiteView @JvmOverloads constructor(context: Context, attrs: AttributeSet? = null) :
    FrameLayout(context, attrs) {

    enum class State { DISCONNECTED, CONNECTING, CONNECTED, DROPPED, ERROR }

    private enum class Event { CONNECT, READY, STOP, LOST, FAIL }

    /** One sine on one layer's rotation, with its own phase so a change of rate never restarts it. */
    private class Loop(val view: View, val amp: Float, var periodMs: Float) {
        var on = false
        var gain = 0f
        var phase = 0f
        var delayLeft = 0f
        val busy get() = on || gain > 0f

        fun set(want: Boolean, delay: Float) {
            if (!want) {
                on = false
            } else if (!on) {
                on = true
                // Still easing out from a moment ago: pick up at once instead of holding its offset.
                delayLeft = if (gain > 0f) 0f else delay
                if (gain == 0f) phase = 0f // a fresh loop starts at its zero crossing
            }
        }

        fun halt() {
            on = false
            gain = 0f
            view.rotation = 0f
        }

        /** Moves the loop on by [dt] ms; true while it needs more frames. */
        fun step(dt: Float): Boolean {
            if (on && delayLeft > 0f) {
                delayLeft -= dt
                return true
            }
            gain = if (on) min(1f, gain + dt / M.LOOP_EASE) else max(0f, gain - dt / M.LOOP_EASE)
            phase += dt / periodMs
            phase -= floor(phase)
            view.rotation = amp * M.OUT.getInterpolation(gain) * sin(2f * PI.toFloat() * phase)
            return on || gain > 0f
        }
    }

    init {
        clipChildren = false
        // The status text beside it says the same, so a screen reader skips the picture.
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO_HIDE_DESCENDANTS
        LayoutInflater.from(context).inflate(R.layout.view_kite, this, true)
    }

    private val art: View = findViewById(R.id.art)
    private val slack: View = findViewById(R.id.slack)
    private val snapped: View = findViewById(R.id.snapped)
    private val rig: View = findViewById(R.id.rig)
    private val taut: View = findViewById(R.id.taut)
    private val pose: View = findViewById(R.id.pose)
    private val rock: View = findViewById(R.id.rock)
    private val tail: View = findViewById(R.id.tail)
    private val badge: View = findViewById(R.id.badge)

    private val wobble = Loop(rock, M.WOBBLE_DEG, M.WOBBLE_MS)
    private val swayRig = Loop(rig, M.SWAY_RIG_DEG, M.SWAY_MS)
    private val swayRock = Loop(rock, M.SWAY_ROCK_DEG, M.SWAY_MS)
    private val flutter = Loop(tail, M.TAIL_DEG, M.TAIL_MS)
    private val loops = arrayOf(wobble, swayRig, swayRock, flutter)

    var state = State.DISCONNECTED
        private set

    private var px = 0f
    private var started = false
    private var screenOn = true
    private var reduced = false
    private var poseAnim: Animator? = null
    private var rateNow = 1f
    private var rateGoal = 1f
    private var lastFrame = 0L
    private var posted = false
    private var fadeTo = State.DISCONNECTED
    private var fadeFrom = 1f
    private var fadeStart = 0L
    private var swapped = false
    private var covered = false
    private var scrolledAway = false
    private var wasActive = false
    private val rect = Rect()

    private val active get() = started && screenOn && isAttachedToWindow && !covered && !scrolledAway

    /** Link dropped is only a picture: 900 ms after it started the kite is Disconnected. */
    private val toDisconnected = Runnable { if (state == State.DROPPED) state = State.DISCONNECTED }

    /** The string snaps after the kite has fallen. */
    private val snapNow = Runnable {
        fade(slack, 0f, 0L, M.SNAP_FADE)
        fade(snapped, 1f, 0L, M.SNAP_FADE)
    }

    /**
     * Reduced motion: the art fades out, the new state is set behind it, then it fades in. Driven by
     * the clock and not by an animator, because animators finish at once when the animation scale is 0,
     * which is exactly when this runs.
     */
    private val fadeStep = object : Runnable {
        override fun run() {
            val t = SystemClock.uptimeMillis() - fadeStart
            if (t < M.FADE_OUT) {
                art.alpha = fadeFrom * (1f - t / M.FADE_OUT.toFloat())
                postOnAnimation(this)
                return
            }
            if (!swapped) {
                swapped = true
                showLayers(fadeTo)
            }
            val u = (t - M.FADE_OUT) / M.FADE_IN.toFloat()
            if (u < 1f) {
                art.alpha = M.OUT.getInterpolation(u)
                postOnAnimation(this)
            } else {
                art.alpha = 1f
            }
        }
    }

    /** The kite scrolled out of view or back in. */
    private val scrolled = ViewTreeObserver.OnScrollChangedListener {
        scrolledAway = !getLocalVisibleRect(rect)
        refreshVisible()
    }

    private val frame = Choreographer.FrameCallback { nanos ->
        posted = false
        val dt = if (lastFrame == 0L) M.FRAME_MS else (nanos - lastFrame) / 1_000_000f
        if (dt >= M.FRAME_MS - 3f) {
            lastFrame = nanos
            val ease = rateGoal - rateNow
            if (ease != 0f) {
                rateNow += ease * min(1f, dt / M.RATE_EASE_MS)
                flutter.periodMs = M.TAIL_MS / rateNow
            }
            var busy = false
            for (loop in loops) {
                if (loop.busy) busy = loop.step(min(dt, 100f)) || busy
            }
            if (busy) schedule() else lastFrame = 0L // the next burst of frames starts a new clock
        } else {
            schedule()
        }
    }

    private val screen = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            screenOn = intent.action == Intent.ACTION_SCREEN_ON
            if (screenOn) wake() else halt()
        }
    }

    private val observer = object : ContentObserver(Handler(Looper.getMainLooper())) {
        override fun onChange(selfChange: Boolean) {
            val now = reducedMotion()
            if (now == reduced) return
            reduced = now
            if (now) {
                for (loop in loops) loop.halt()
                halt()
            } else if (active) {
                snap(state)
            }
        }
    }

    /** The screen is in front (onStart). */
    fun resume() {
        started = true
        screenOn = context.getSystemService(PowerManager::class.java).isInteractive
        reduced = reducedMotion()
        wake()
    }

    /** The screen is gone (onStop): nothing runs, and the kite shows where it is going. */
    fun pause() {
        started = false
        halt()
    }

    /** Starts the loops again after the kite could not be seen; one already running keeps its phase. */
    private fun wake() {
        if (px > 0f && !reduced && active) runLoops(state, 0f) else schedule()
    }

    /** Loops run only while the kite can be seen: they stop when it is covered or scrolled away. */
    private fun refreshVisible() {
        val now = active
        if (now == wasActive) return
        wasActive = now
        if (now) wake() else halt()
    }

    override fun onVisibilityAggregated(isVisible: Boolean) {
        super.onVisibilityAggregated(isVisible)
        covered = !isVisible
        refreshVisible()
    }

    /** A link that dropped is Disconnected for good where no fall is played. */
    private fun settled(s: State) = if (s == State.DROPPED) State.DISCONNECTED else s

    /** Throughput in Mbit/s; the tail follows it. Cheap, call it as often as it is measured. */
    fun setThroughput(mbit: Float) {
        rateGoal = M.tailRate(mbit)
    }

    /** What the link is doing now. The last call wins; the same target twice does nothing. */
    fun setTarget(target: State) {
        when (target) {
            State.CONNECTED -> go(Event.READY)
            State.CONNECTING -> {
                if (state == State.CONNECTED) go(Event.LOST)
                go(Event.CONNECT)
            }
            State.DISCONNECTED -> go(Event.STOP)
            State.ERROR -> go(Event.FAIL)
            State.DROPPED -> go(Event.LOST)
        }
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        reduced = reducedMotion()
        context.contentResolver.registerContentObserver(
            Settings.Global.getUriFor(Settings.Global.ANIMATOR_DURATION_SCALE), false, observer,
        )
        context.registerReceiver(
            screen, IntentFilter(Intent.ACTION_SCREEN_ON).apply { addAction(Intent.ACTION_SCREEN_OFF) },
        )
        viewTreeObserver.addOnScrollChangedListener(scrolled)
    }

    override fun onDetachedFromWindow() {
        context.contentResolver.unregisterContentObserver(observer)
        context.unregisterReceiver(screen)
        if (viewTreeObserver.isAlive) viewTreeObserver.removeOnScrollChangedListener(scrolled)
        halt()
        super.onDetachedFromWindow()
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        px = w / M.STAGE
        for (layer in arrayOf(pose, rock, tail)) {
            layer.pivotX = M.B_X * px
            layer.pivotY = M.B_Y * px
        }
        rig.pivotX = M.H_X * px
        rig.pivotY = M.H_Y * px
        badge.pivotX = M.BADGE_X * px
        badge.pivotY = M.BADGE_Y * px
        snap(state)
    }

    private fun reducedMotion() =
        Settings.Global.getFloat(context.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f

    /** The state machine of docs/design/motion.md; null is an empty cell: the event is ignored. */
    private fun next(from: State, event: Event): State? = when (event) {
        Event.CONNECT ->
            if (from == State.DISCONNECTED || from == State.DROPPED || from == State.ERROR) State.CONNECTING else null
        Event.READY -> if (from != State.CONNECTED) State.CONNECTED else null
        Event.STOP ->
            if (from == State.CONNECTING || from == State.CONNECTED || from == State.ERROR) State.DISCONNECTED else null
        Event.LOST -> if (from == State.CONNECTING || from == State.CONNECTED) State.DROPPED else null
        Event.FAIL -> if (from != State.ERROR) State.ERROR else null
    }

    private fun go(event: Event) {
        val from = state
        val to = next(from, event) ?: return
        state = to
        if (px == 0f || !active) {
            snap(to)
            return
        }
        cancelAnimations()
        if (reduced) {
            state = settled(to)
            fadeThrough(state)
            return
        }
        when (event) {
            Event.CONNECT -> {
                movePose(M.CONNECTING, M.LIFT, M.OUT)
                stringTaut()
                if (from == State.ERROR) hideBadge()
                runLoops(to, M.LIFT.toFloat())
            }
            Event.READY -> {
                val settle = if (from == State.CONNECTING) M.SETTLE_FROM_CONNECTING else M.SETTLE
                movePose(M.CONNECTED, settle, M.OUT)
                stringTaut()
                if (from == State.ERROR) hideBadge()
                runLoops(to, settle.toFloat())
            }
            Event.STOP -> {
                movePose(M.REST, M.DRIFT_STOP, M.SINE)
                stringSlack(M.STRING_DELAY_STOP)
                if (from == State.ERROR) hideBadge()
                runLoops(to, 0f)
            }
            Event.LOST -> {
                fall()
                stringSlack(M.STRING_DELAY_LOST)
                postDelayed(toDisconnected, M.LOST)
                runLoops(to, 0f)
            }
            Event.FAIL -> {
                if (from == State.DISCONNECTED) {
                    showLayers(State.DISCONNECTED) // a stop a moment ago may have been cut off half way
                    snapNow.run()
                    showBadge(0L)
                } else {
                    fall()
                    stringSlack(M.STRING_DELAY_LOST)
                    postDelayed(snapNow, M.SNAP_DELAY)
                    showBadge(M.BADGE_DELAY)
                }
                runLoops(to, 0f)
            }
        }
    }

    /** Sets the end state of [to] at once, without animation. */
    private fun snap(to: State) {
        cancelAnimations()
        state = settled(to)
        showLayers(state)
        art.alpha = 1f
        for (loop in loops) loop.halt()
        if (active && !reduced) runLoops(state, 0f)
    }

    /** Stops everything that moves and shows the end state; loops keep their phase for [resume]. */
    private fun halt() {
        cancelAnimations()
        unschedule()
        if (state == State.DROPPED) state = State.DISCONNECTED
        if (px > 0f) {
            showLayers(state)
            art.alpha = 1f
        }
    }

    private fun cancelAnimations() {
        poseAnim?.cancel()
        poseAnim = null
        removeCallbacks(toDisconnected)
        removeCallbacks(snapNow)
        removeCallbacks(fadeStep)
        for (layer in arrayOf(art, slack, snapped, taut, badge)) layer.animate().cancel()
    }

    private fun showLayers(to: State) {
        val p = when (to) {
            State.CONNECTING -> M.CONNECTING
            State.CONNECTED -> M.CONNECTED
            else -> M.REST
        }
        pose.translationX = p.x * px
        pose.translationY = p.y * px
        pose.rotation = p.rotation
        val lifted = to == State.CONNECTING || to == State.CONNECTED
        val failed = to == State.ERROR
        taut.alpha = if (lifted) 1f else 0f
        slack.alpha = if (lifted || failed) 0f else 1f
        snapped.alpha = if (failed) 1f else 0f
        badge.alpha = if (failed) 1f else 0f
        badge.scaleX = if (failed) 1f else M.BADGE_FROM
        badge.scaleY = badge.scaleX
    }

    /** Reduced motion: the art fades out, the new state is set while it is invisible, then it fades in. */
    private fun fadeThrough(to: State) {
        fadeTo = to
        fadeFrom = art.alpha
        fadeStart = SystemClock.uptimeMillis()
        swapped = false
        postOnAnimation(fadeStep)
    }

    private fun movePose(p: M.Pose, duration: Long, curve: TimeInterpolator) {
        poseAnim = ObjectAnimator.ofPropertyValuesHolder(
            pose,
            PropertyValuesHolder.ofFloat(View.TRANSLATION_X, p.x * px),
            PropertyValuesHolder.ofFloat(View.TRANSLATION_Y, p.y * px),
            PropertyValuesHolder.ofFloat(View.ROTATION, p.rotation),
        ).apply {
            this.duration = duration
            interpolator = curve
            start()
        }
    }

    /** Lost and Fail: a quick dip, then a drift down to Rest, as one animation. */
    private fun fall() {
        val dip = M.LOST_DIP.toFloat() / M.LOST
        fun holder(name: String, from: Float, dipTo: Float, rest: Float): PropertyValuesHolder {
            val a = Keyframe.ofFloat(0f, from)
            val b = Keyframe.ofFloat(dip, dipTo).apply { interpolator = M.OUT }
            val c = Keyframe.ofFloat(1f, rest).apply { interpolator = M.SINE }
            return PropertyValuesHolder.ofKeyframe(name, a, b, c)
        }
        poseAnim = ObjectAnimator.ofPropertyValuesHolder(
            pose,
            holder("translationX", pose.translationX, M.DIP.x * px, M.REST.x * px),
            holder("translationY", pose.translationY, M.DIP.y * px, M.REST.y * px),
            holder("rotation", pose.rotation, M.DIP.rotation, M.REST.rotation),
        ).apply {
            duration = M.LOST
            interpolator = M.LINEAR // each segment has its own curve
            start()
        }
    }

    private fun stringTaut() {
        fade(taut, 1f, M.STRING_DELAY_CONNECT, M.STRING_FADE)
        fade(slack, 0f, M.STRING_DELAY_CONNECT, M.STRING_FADE)
        fade(snapped, 0f, M.STRING_DELAY_CONNECT, M.STRING_FADE)
    }

    private fun stringSlack(delay: Long) {
        fade(taut, 0f, delay, M.STRING_FADE)
        fade(slack, 1f, delay, M.STRING_FADE)
        fade(snapped, 0f, delay, M.STRING_FADE)
    }

    private fun hideBadge() {
        badge.animate().alpha(0f).scaleX(M.BADGE_FROM).scaleY(M.BADGE_FROM)
            .setStartDelay(0L).setDuration(M.BADGE_HIDE).setInterpolator(M.OUT).start()
    }

    private fun showBadge(delay: Long) {
        badge.animate().alpha(1f).scaleX(1f).scaleY(1f)
            .setStartDelay(delay).setDuration(M.BADGE_SHOW).setInterpolator(M.OUT).start()
    }

    private fun fade(v: View, to: Float, delay: Long, duration: Long, curve: TimeInterpolator = M.OUT) {
        v.animate().alpha(to).setStartDelay(delay).setDuration(duration).setInterpolator(curve).start()
    }

    /** Starts and stops the loops for [to]; one that is already running is left alone. */
    private fun runLoops(to: State, delay: Float) {
        wobble.set(to == State.CONNECTING, delay)
        swayRig.set(to == State.CONNECTED, delay)
        swayRock.set(to == State.CONNECTED, delay + M.SWAY_ROCK_DELAY)
        flutter.set(to == State.CONNECTED, delay)
        schedule()
    }

    private fun schedule() {
        if (posted || !active || reduced) return
        if (loops.none { it.busy }) return
        posted = true
        Choreographer.getInstance().postFrameCallback(frame)
    }

    private fun unschedule() {
        if (posted) Choreographer.getInstance().removeFrameCallback(frame)
        posted = false
        lastFrame = 0L
    }
}
