package io.github.heidaridev.kite

import android.view.animation.LinearInterpolator
import android.view.animation.PathInterpolator
import kotlin.math.log10

/**
 * The numbers of docs/design/motion.md in one place. Times are milliseconds, angles degrees and
 * lengths stage units: the kite is drawn on a stage of 64 by 64 units.
 */
internal object KiteMotion {

    val OUT = PathInterpolator(0.23f, 1f, 0.32f, 1f)
    val SINE = PathInterpolator(0.37f, 0f, 0.63f, 1f)
    val LINEAR = LinearInterpolator()

    const val STAGE = 64f

    // Anchors on the stage: the kite's bottom vertex B, the hand H and the badge's centre.
    const val B_X = 38f
    const val B_Y = 41f
    const val H_X = 6f
    const val H_Y = 62f
    const val BADGE_X = 53f
    const val BADGE_Y = 11f

    /** A translation in stage units plus a rotation about B in degrees. */
    class Pose(val x: Float, val y: Float, val rotation: Float)

    val REST = Pose(-18f, 9f, 72f)
    val CONNECTING = Pose(-1f, 2f, -8f)
    val CONNECTED = Pose(0f, 0f, 0f)
    val DIP = Pose(0f, 5f, 6f)

    const val LIFT = 450L
    const val SETTLE_FROM_CONNECTING = 350L
    const val SETTLE = 450L
    const val DRIFT_STOP = 600L
    const val STRING_FADE = 250L
    const val STRING_DELAY_CONNECT = 150L
    const val STRING_DELAY_STOP = 250L
    const val STRING_DELAY_LOST = 300L
    const val BADGE_HIDE = 120L

    const val LOST = 900L
    const val LOST_DIP = 180L
    const val SNAP_DELAY = 800L
    const val SNAP_FADE = 120L
    const val BADGE_DELAY = 750L
    const val BADGE_SHOW = 160L
    const val BADGE_FROM = 0.9f

    const val LOOP_EASE = 150f
    const val FADE_OUT = 80L
    const val FADE_IN = 120L

    const val WOBBLE_DEG = 6f
    const val WOBBLE_MS = 700f
    const val SWAY_RIG_DEG = 1.6f
    const val SWAY_ROCK_DEG = 3.5f
    const val SWAY_MS = 4800f
    const val SWAY_ROCK_DELAY = 600f
    const val TAIL_DEG = 10f
    const val TAIL_MS = 1400f

    /** Loops are drawn at most this often. */
    const val FRAME_MS = 33f

    /** How fast the flutter follows a new throughput, so a jump in speed is not a jump in the tail. */
    const val RATE_EASE_MS = 300f

    /** Flutter speed for a throughput in Mbit/s: 1x idle, 3.1x from 100 Mbit/s up. */
    fun tailRate(mbit: Float): Float {
        val u = (log10(1f + mbit.coerceAtLeast(0f)) / log10(101f)).coerceIn(0f, 1f)
        return 1f + 2.1f * u
    }
}
