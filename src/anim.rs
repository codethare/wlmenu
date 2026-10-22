//! Expand/collapse height transition for the panel.

use std::time::{Duration, Instant};

/// Height transition length for expand/collapse (Spotlight-style). Long enough to
/// read as motion rather than a jump; short enough that a size change caused by a
/// keystroke still feels immediate.
const ANIM: Duration = Duration::from_millis(180);

/// Height transition: ease-in-out cubic from `from` to `to`, in buffer pixels.
/// Frame cost is ~2 ms even at 64 rows, so animating is affordable.
pub(crate) struct Anim {
    pub(crate) from: u32,
    pub(crate) to: u32,
    pub(crate) start: Instant,
}

impl Anim {
    /// Current height plus whether the transition is still running.
    pub(crate) fn height(&self, now: Instant) -> (u32, bool) {
        let t = now.saturating_duration_since(self.start).as_secs_f32() / ANIM.as_secs_f32();
        if t >= 1.0 {
            return (self.to, false);
        }
        // Ease in and out: both ends move at nearly zero speed, so the eye reads a
        // glide with no jolt. A pure ease-out (which this used to be) starts at
        // full speed; that was invisible only because the transition was short.
        let t = t.clamp(0.0, 1.0);
        let e = if t < 0.5 {
            4.0 * t * t * t
        } else {
            1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
        };
        let h = self.from as f32 + (self.to as f32 - self.from as f32) * e;
        (h.round().max(0.0) as u32, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_eases_to_the_target_and_stops() {
        let start = Instant::now();
        let grow = Anim {
            from: 24,
            to: 1560,
            start,
        };
        assert_eq!(grow.height(start), (24, true), "starts at the from height");
        let (early, _) = grow.height(start + ANIM / 10);
        assert!(
            early - 24 < (1560 - 24) / 20,
            "the curve eases in: {early} after a tenth of the way"
        );
        let (mid, running) = grow.height(start + ANIM / 2);
        assert!(
            running && mid > 24 && mid < 1560,
            "mid-flight is between the ends: {mid}"
        );
        assert_eq!(
            grow.height(start + ANIM),
            (1560, false),
            "ends exactly at the target"
        );
        assert_eq!(
            grow.height(start + ANIM + Duration::from_millis(50)).0,
            1560
        );

        // Collapsing runs the same way, monotonically (no bounce).
        let shrink = Anim {
            from: 1560,
            to: 24,
            start,
        };
        let mut prev = 1560;
        for ms in 0..=ANIM.as_millis() as u64 {
            let (h, _) = shrink.height(start + Duration::from_millis(ms));
            assert!(h <= prev, "collapse must not bounce: {h} > {prev}");
            prev = h;
        }
        assert_eq!(prev, 24, "collapse reaches the bar height");
    }
}
