//! Open/close and expand/collapse transition for the panel: height plus opacity.

use std::time::{Duration, Instant};

/// Transition length (Spotlight-style). Long enough to read as motion rather than
/// a jump; short enough that a size change caused by a keystroke still feels
/// immediate.
const ANIM: Duration = Duration::from_millis(180);

/// Height and opacity transition: ease-in-out cubic from `from_*` to `to_*`.
/// Frame cost is ~2 ms even at 64 rows, so animating is affordable.
pub(crate) struct Anim {
    pub(crate) from_h: u32,
    pub(crate) to_h: u32,
    pub(crate) from_a: f32,
    pub(crate) to_a: f32,
    pub(crate) start: Instant,
}

impl Anim {
    /// Height in buffer px, opacity 0..=1, and whether the transition is running.
    pub(crate) fn at(&self, now: Instant) -> (u32, f32, bool) {
        let t = now.saturating_duration_since(self.start).as_secs_f32() / ANIM.as_secs_f32();
        if t >= 1.0 {
            return (self.to_h, self.to_a, false);
        }
        // Ease in and out: both ends move at nearly zero speed, so the eye reads a
        // glide with no jolt. A pure ease-out starts at full speed; that was
        // invisible only because the transition used to be short.
        let t = t.clamp(0.0, 1.0);
        let e = if t < 0.5 {
            4.0 * t * t * t
        } else {
            1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
        };
        let h = self.from_h as f32 + (self.to_h as f32 - self.from_h as f32) * e;
        let a = self.from_a + (self.to_a - self.from_a) * e;
        (h.round().max(0.0) as u32, a.clamp(0.0, 1.0), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grow(from: u32, to: u32) -> Anim {
        Anim {
            from_h: from,
            to_h: to,
            from_a: 1.0,
            to_a: 1.0,
            start: Instant::now(),
        }
    }

    #[test]
    fn animation_eases_to_the_target_and_stops() {
        let open = grow(24, 1560);
        let start = open.start;
        assert_eq!(open.at(start).0, 24, "starts at the from height");
        let (early, _, running) = open.at(start + ANIM / 10);
        assert!(running, "still running a tenth of the way in");
        assert!(
            early - 24 < (1560 - 24) / 20,
            "the curve eases in: {early} after a tenth of the way"
        );
        let (mid, _, running) = open.at(start + ANIM / 2);
        assert!(
            running && mid > 24 && mid < 1560,
            "mid-flight is between the ends: {mid}"
        );
        assert_eq!(
            open.at(start + ANIM),
            (1560, 1.0, false),
            "ends exactly at the target"
        );
        assert_eq!(open.at(start + ANIM + Duration::from_millis(50)).0, 1560);

        // Collapsing runs the same way, monotonically (no bounce).
        let shrink = grow(1560, 24);
        let mut prev = 1560;
        for ms in 0..=ANIM.as_millis() as u64 {
            let (h, _, _) = shrink.at(start + Duration::from_millis(ms));
            assert!(h <= prev, "collapse must not bounce: {h} > {prev}");
            prev = h;
        }
        assert_eq!(prev, 24, "collapse reaches the bar height");
    }

    /// Opening fades in, closing fades out, and both are monotonic in opacity: a
    /// panel that dipped back towards transparent mid-flight would flicker.
    #[test]
    fn opacity_ramps_monotonically_to_its_target() {
        let start = Instant::now();
        for (from_a, to_a) in [(0.0, 1.0), (1.0, 0.0)] {
            let anim = Anim {
                from_h: 100,
                to_h: 0,
                from_a,
                to_a,
                start,
            };
            let mut prev = from_a;
            for ms in 0..=ANIM.as_millis() as u64 {
                let (_, a, _) = anim.at(start + Duration::from_millis(ms));
                assert!(
                    (a - prev).abs() < f32::EPSILON || (to_a > from_a) == (a > prev),
                    "opacity must move one way: {a} after {prev}"
                );
                prev = a;
            }
            assert_eq!(anim.at(start + ANIM).1, to_a);
        }
    }
}
