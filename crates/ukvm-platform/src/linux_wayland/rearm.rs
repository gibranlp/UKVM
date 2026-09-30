//! Emergency escape hysteresis, using observed local logical coordinates.
use super::{Edge, OuterSegment};
use crate::Point;

pub const EMERGENCY_REARM_DISTANCE: f64 = 32.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum EmergencyRearmGuard {
    #[default]
    Ready,
    AwaitingInward {
        boundary: Option<OuterSegment>,
    },
}

impl EmergencyRearmGuard {
    /// Latch before any asynchronous release/close can fail. Unknown geometry
    /// cannot be cleared by a position sample and remains safely disarmed.
    pub fn suppress(&mut self, boundary: Option<OuterSegment>) {
        *self = Self::AwaitingInward { boundary };
    }

    pub fn allows_arming(self) -> bool {
        self == Self::Ready
    }

    /// Tangential/outward movement and invalid samples cannot clear the latch.
    /// The release-position *suggestion* is never an observation.
    pub fn observe_local(&mut self, position: Point) -> bool {
        let Self::AwaitingInward {
            boundary: Some(boundary),
        } = *self
        else {
            return false;
        };
        if !position.x.is_finite() || !position.y.is_finite() {
            return false;
        }
        let inward = match boundary.edge {
            Edge::Left => position.x - f64::from(boundary.start.0),
            Edge::Right => f64::from(boundary.start.0) - position.x,
            Edge::Top => position.y - f64::from(boundary.start.1),
            Edge::Bottom => f64::from(boundary.start.1) - position.y,
        };
        if inward < EMERGENCY_REARM_DISTANCE {
            return false;
        }
        *self = Self::Ready;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emergency_stays_disarmed_until_inward_motion_on_all_edges() {
        for edge in Edge::ALL {
            let boundary = OuterSegment {
                zone: 0,
                edge,
                start: (-100, -200),
                end: (-100, -200),
            };
            let mut guard = EmergencyRearmGuard::Ready;
            guard.suppress(Some(boundary));
            let at_edge = Point {
                x: -100.0,
                y: -200.0,
            };
            for _ in 0..100 {
                assert!(!guard.observe_local(at_edge));
                assert!(!guard.allows_arming());
            }
            let inward = |distance| match edge {
                Edge::Left => Point {
                    x: at_edge.x + distance,
                    ..at_edge
                },
                Edge::Right => Point {
                    x: at_edge.x - distance,
                    ..at_edge
                },
                Edge::Top => Point {
                    y: at_edge.y + distance,
                    ..at_edge
                },
                Edge::Bottom => Point {
                    y: at_edge.y - distance,
                    ..at_edge
                },
            };
            assert!(!guard.observe_local(inward(-32.0)));
            assert!(!guard.observe_local(inward(31.99)));
            assert!(guard.observe_local(inward(32.0)));
            assert!(guard.allows_arming());
        }
    }

    #[test]
    fn tangential_invalid_and_unknown_positions_never_rearm() {
        let mut guard = EmergencyRearmGuard::Ready;
        guard.suppress(Some(OuterSegment {
            zone: 0,
            edge: Edge::Right,
            start: (0, 0),
            end: (0, 2159),
        }));
        for point in [
            Point { x: 0.0, y: 500.0 },
            Point {
                x: f64::NAN,
                y: 0.0,
            },
            Point {
                x: -100.0,
                y: f64::INFINITY,
            },
        ] {
            assert!(!guard.observe_local(point));
            assert!(!guard.allows_arming());
        }
        guard.suppress(None);
        assert!(!guard.observe_local(Point { x: -100.0, y: 0.0 }));
        assert!(!guard.allows_arming());
    }
}
