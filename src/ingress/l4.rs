use std::collections::BTreeSet;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

impl PortRange {
    pub const fn new(start: u16, end: u16) -> Self {
        Self { start, end }
    }

    pub fn parse(value: &str) -> Result<Self, PortRangeError> {
        let (start, end) = value
            .trim()
            .split_once('-')
            .ok_or_else(|| PortRangeError::Invalid(value.to_string()))?;
        let start = start
            .trim()
            .parse::<u16>()
            .map_err(|_| PortRangeError::Invalid(value.to_string()))?;
        let end = end
            .trim()
            .parse::<u16>()
            .map_err(|_| PortRangeError::Invalid(value.to_string()))?;
        let range = Self { start, end };
        range.validate()?;
        Ok(range)
    }

    pub fn validate(&self) -> Result<(), PortRangeError> {
        if self.start == 0 || self.end == 0 || self.start > self.end {
            return Err(PortRangeError::Invalid(format!(
                "{}-{}",
                self.start, self.end
            )));
        }
        Ok(())
    }

    pub fn contains(&self, port: u16) -> bool {
        self.start <= port && port <= self.end
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PortRangeError {
    #[error("invalid port range: {0}")]
    Invalid(String),
    #[error("no free port in range {0}-{1}")]
    Exhausted(u16, u16),
}

#[derive(Debug, Clone)]
pub struct PortAllocator {
    range: PortRange,
    occupied: BTreeSet<u16>,
}

impl PortAllocator {
    pub fn new(range: PortRange, occupied: impl IntoIterator<Item = u16>) -> Self {
        Self {
            range,
            occupied: occupied.into_iter().collect(),
        }
    }

    pub fn allocate(&mut self) -> Result<u16, PortRangeError> {
        for port in self.range.start..=self.range.end {
            if self.occupied.insert(port) {
                return Ok(port);
            }
        }
        Err(PortRangeError::Exhausted(self.range.start, self.range.end))
    }

    pub fn release(&mut self, port: u16) {
        if self.range.contains(port) {
            self.occupied.remove(&port);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_range_parses_inclusive_bounds() {
        let range = PortRange::parse("20000-20010").expect("valid range");
        assert_eq!(range, PortRange::new(20000, 20010));
        assert!(range.contains(20000));
        assert!(range.contains(20010));
        assert!(!range.contains(19999));
    }

    #[test]
    fn port_range_rejects_zero_reversed_and_malformed_values() {
        for value in ["0-10", "20-10", "abc", "10", "10:20"] {
            assert!(
                matches!(PortRange::parse(value), Err(PortRangeError::Invalid(_))),
                "{value} should be rejected"
            );
        }
    }

    #[test]
    fn allocator_returns_first_unoccupied_port_and_tracks_claims() {
        let mut allocator = PortAllocator::new(PortRange::new(30000, 30003), [30000, 30002]);

        assert_eq!(allocator.allocate().expect("first"), 30001);
        assert_eq!(allocator.allocate().expect("second"), 30003);
        assert!(matches!(
            allocator.allocate(),
            Err(PortRangeError::Exhausted(30000, 30003))
        ));
    }

    #[test]
    fn allocator_reuses_released_ports_inside_range_only() {
        let mut allocator = PortAllocator::new(PortRange::new(40000, 40001), []);

        assert_eq!(allocator.allocate().expect("first"), 40000);
        allocator.release(12345);
        assert_eq!(allocator.allocate().expect("second"), 40001);
        allocator.release(40000);
        assert_eq!(allocator.allocate().expect("reused"), 40000);
    }
}
