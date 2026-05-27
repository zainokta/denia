fn ceil_div(a: u64, b: u64) -> u64 {
    a.div_ceil(b.max(1))
}

/// Scale-up candidate: max of CPU-driven and memory-driven desired (memory only if a target is set),
/// never below the current count.
pub fn desired_up(
    current: u32,
    cpu_pct: u32,
    target_cpu: u8,
    mem_pct: Option<u32>,
    target_mem: Option<u8>,
) -> u32 {
    let c = ceil_div(current as u64 * cpu_pct as u64, target_cpu as u64) as u32;
    let m = match (mem_pct, target_mem) {
        (Some(mp), Some(tm)) => ceil_div(current as u64 * mp as u64, tm as u64) as u32,
        _ => 0,
    };
    c.max(m).max(current)
}

/// Scale-down candidate uses CPU only (memory is scale-up-only).
pub fn desired_down(current: u32, cpu_pct: u32, target_cpu: u8) -> u32 {
    ceil_div(current as u64 * cpu_pct as u64, target_cpu as u64) as u32
}

/// Loop-side clamp: floor is max(min,1) because the 1->0 transition is owned by the
/// activator/idle path, not the control loop.
pub fn clamp_loop(desired: u32, min: u32, max: u32) -> u32 {
    desired.clamp(min.max(1), max)
}

#[derive(Debug, Default, Clone)]
pub struct CooldownState {
    below_since: Option<u64>,
}

impl CooldownState {
    /// Call when the metric is at/above target: cancels any in-progress cooldown.
    pub fn note_above_target(&mut self, _now_s: u64) {
        self.below_since = None;
    }

    /// Call when the metric is below target and a scale-down is desired.
    /// Returns true once the metric has been continuously below target for `cooldown_s`.
    pub fn scale_down_allowed(&mut self, now_s: u64, cooldown_s: u64) -> bool {
        match self.below_since {
            None => {
                self.below_since = Some(now_s);
                false
            }
            Some(start) => now_s.saturating_sub(start) >= cooldown_s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_up_uses_max_of_cpu_and_mem() {
        // current=2, cpu 90/target80 => ceil(2*90/80)=3 ; mem 50/target75 => ceil(2*50/75)=2 => up=3
        assert_eq!(desired_up(2, 90, 80, Some(50), Some(75)), 3);
    }
    #[test]
    fn scale_up_ignores_mem_when_no_target() {
        // mem target None => mem term is 0 ; cpu 90/80 => 3
        assert_eq!(desired_up(2, 90, 80, Some(99), None), 3);
    }
    #[test]
    fn scale_down_ignores_memory() {
        // cpu 20/target80 => ceil(2*20/80)=1
        assert_eq!(desired_down(2, 20, 80), 1);
    }
    #[test]
    fn clamp_respects_bounds_never_zero_from_loop() {
        assert_eq!(clamp_loop(0, 1, 5), 1); // loop floor is max(min,1)=1
        assert_eq!(clamp_loop(0, 0, 5), 1); // even with min=0, loop floor is 1
        assert_eq!(clamp_loop(9, 1, 5), 5);
    }

    #[test]
    fn cooldown_gates_scale_down_only() {
        let mut st = CooldownState::default();
        // first observation below target at t=0 starts the window -> not yet allowed
        assert!(!st.scale_down_allowed(0, 300));
        // still within the window
        assert!(!st.scale_down_allowed(299, 300));
        // window elapsed -> allowed
        assert!(st.scale_down_allowed(300, 300));
        // a breach above target resets the window
        st.note_above_target(310);
        assert!(!st.scale_down_allowed(320, 300));
    }
}
