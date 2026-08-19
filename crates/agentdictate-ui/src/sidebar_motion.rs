use std::time::{Duration, Instant};

const OPEN_DURATION: Duration = Duration::from_millis(210);
const CLOSE_DURATION: Duration = Duration::from_millis(180);

/// Continuous motion shared by the docked sidebar and compact overlay.
pub(crate) struct SidebarMotion {
    panel: ScalarMotion,
    scrim: ScalarMotion,
    initialized: bool,
}

impl SidebarMotion {
    pub(crate) fn new() -> Self {
        Self {
            panel: ScalarMotion::new(),
            scrim: ScalarMotion::new(),
            initialized: false,
        }
    }

    pub(crate) fn update(
        &mut self,
        open: bool,
        compact: bool,
        first_layout: bool,
        now: Instant,
    ) -> SidebarFrame {
        let panel_target = f32::from(open);
        let scrim_target = f32::from(open && compact);
        if first_layout || !self.initialized {
            self.panel.snap(panel_target);
            self.scrim.snap(scrim_target);
            self.initialized = true;
        } else {
            self.panel.retarget(panel_target, now);
            self.scrim.retarget(scrim_target, now);
        }
        SidebarFrame {
            panel: self.panel.sample(now),
            scrim: self.scrim.sample(now),
            active: self.panel.active() || self.scrim.active(),
        }
    }
}

struct ScalarMotion {
    from: f32,
    target: f32,
    started_at: Option<Instant>,
    duration: Duration,
}

impl ScalarMotion {
    const fn new() -> Self {
        Self {
            from: 0.0,
            target: 0.0,
            started_at: None,
            duration: Duration::ZERO,
        }
    }

    fn snap(&mut self, value: f32) {
        self.from = value;
        self.target = value;
        self.started_at = None;
    }

    fn retarget(&mut self, target: f32, now: Instant) {
        if self.target == target {
            return;
        }
        let current = self.sample(now);
        let distance = (target - current).abs();
        self.from = current;
        self.target = target;
        self.duration = if target > current {
            OPEN_DURATION.mul_f32(distance)
        } else {
            CLOSE_DURATION.mul_f32(distance)
        };
        self.started_at = (distance > f32::EPSILON).then_some(now);
    }

    fn sample(&mut self, now: Instant) -> f32 {
        let Some(started_at) = self.started_at else {
            return self.target;
        };
        let elapsed = now.saturating_duration_since(started_at);
        if elapsed >= self.duration {
            self.snap(self.target);
            return self.target;
        }
        let linear = elapsed.as_secs_f32() / self.duration.as_secs_f32();
        let eased = 1.0 - (1.0 - linear).powi(3);
        self.from + (self.target - self.from) * eased
    }

    const fn active(&self) -> bool {
        self.started_at.is_some()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SidebarFrame {
    pub(crate) panel: f32,
    pub(crate) scrim: f32,
    pub(crate) active: bool,
}
