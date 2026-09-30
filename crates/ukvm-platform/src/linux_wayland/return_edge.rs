//! Virtual pointer placement for a permissioned EI absolute pointer.
//! The portal does not report compositor cursor coordinates. This model is
//! valid only while UKVM drives every remote motion with EI absolute positions.

use super::geometry::{outer_segments, Edge, OuterSegment, Zone};
use ukvm_protocol::{Edge as ProtocolEdge, EdgePosition};

const ENTRY_INSET: f64 = 24.0;
const ARM_DISTANCE: f64 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MotionResult {
    Position(f32, f32),
    Return(EdgePosition),
}

pub struct RemotePointer {
    zones: Vec<Zone>,
    return_edge: Edge,
    return_segments: Vec<OuterSegment>,
    span: (f64, f64),
    position: (f64, f64),
    entry_boundary: f64,
    armed: bool,
    exits: Vec<OuterSegment>,
}

fn geometry_edge(edge: ProtocolEdge) -> Edge {
    match edge {
        ProtocolEdge::Left => Edge::Left,
        ProtocolEdge::Right => Edge::Right,
        ProtocolEdge::Top => Edge::Top,
        ProtocolEdge::Bottom => Edge::Bottom,
    }
}

impl RemotePointer {
    #[cfg(test)]
    pub fn new(zones: Vec<Zone>, entry: EdgePosition) -> Result<Self, String> {
        Self::with_exits(zones, entry, Vec::new())
    }
    pub fn with_exits(
        zones: Vec<Zone>,
        entry: EdgePosition,
        exits: Vec<ProtocolEdge>,
    ) -> Result<Self, String> {
        entry.validate()?;
        if exits.len() > 4
            || exits
                .iter()
                .enumerate()
                .any(|(i, edge)| exits[..i].contains(edge))
        {
            return Err("invalid topology exits".into());
        }
        let mut exit_segments = Vec::new();
        for edge in exits {
            if edge != entry.edge {
                for index in 0..zones.len() {
                    exit_segments.extend(outer_segments(&zones, index, geometry_edge(edge))?);
                }
            }
        }
        let edge = geometry_edge(entry.edge);
        let mut segments = Vec::new();
        for index in 0..zones.len() {
            segments.extend(outer_segments(&zones, index, edge)?);
        }
        if segments.is_empty() {
            return Err("EI regions expose no outer segment on the requested entry edge".into());
        }
        let orth = |segment: &OuterSegment| match edge {
            Edge::Left | Edge::Right => (f64::from(segment.start.1), f64::from(segment.end.1)),
            Edge::Top | Edge::Bottom => (f64::from(segment.start.0), f64::from(segment.end.0)),
        };
        let span = (
            segments
                .iter()
                .map(|s| orth(s).0)
                .fold(f64::INFINITY, f64::min),
            segments
                .iter()
                .map(|s| orth(s).1)
                .fold(f64::NEG_INFINITY, f64::max),
        );
        let desired = span.0 + f64::from(entry.fraction) * (span.1 - span.0);
        let segment = segments
            .iter()
            .min_by(|a, b| {
                let distance = |s: &OuterSegment| {
                    let (lo, hi) = orth(s);
                    (desired - desired.clamp(lo, hi)).abs()
                };
                distance(a).total_cmp(&distance(b))
            })
            .ok_or("no entry segment")?;
        let (lo, hi) = orth(segment);
        let orthogonal = if hi - lo < 1.0 {
            lo + 0.5
        } else {
            desired.clamp(lo + 0.5, hi - 0.5)
        };
        let boundary = match edge {
            Edge::Left | Edge::Right => f64::from(segment.start.0),
            Edge::Top | Edge::Bottom => f64::from(segment.start.1),
        };
        let zone = zones[segment.zone];
        let inset = match edge {
            Edge::Left | Edge::Right => ENTRY_INSET.min(f64::from(zone.width) / 4.0),
            Edge::Top | Edge::Bottom => ENTRY_INSET.min(f64::from(zone.height) / 4.0),
        };
        let position = match edge {
            Edge::Left => (boundary + inset, orthogonal),
            Edge::Right => (boundary - inset, orthogonal),
            Edge::Top => (orthogonal, boundary + inset),
            Edge::Bottom => (orthogonal, boundary - inset),
        };
        Ok(Self {
            zones,
            return_edge: edge,
            return_segments: segments,
            span,
            position,
            entry_boundary: boundary,
            armed: false,
            exits: exit_segments,
        })
    }

    pub fn update_exits(&mut self, exits: Vec<ProtocolEdge>) -> Result<(), String> {
        if exits.len() > 4
            || exits
                .iter()
                .enumerate()
                .any(|(i, edge)| exits[..i].contains(edge))
        {
            return Err("invalid topology exits".into());
        }
        let mut segments = Vec::new();
        for edge in exits {
            if geometry_edge(edge) != self.return_edge {
                for index in 0..self.zones.len() {
                    segments.extend(outer_segments(&self.zones, index, geometry_edge(edge))?);
                }
            }
        }
        self.exits = segments;
        Ok(())
    }

    pub fn position(&self) -> (f32, f32) {
        (self.position.0 as f32, self.position.1 as f32)
    }

    pub fn motion(&mut self, dx: f64, dy: f64) -> MotionResult {
        let candidate = (self.position.0 + dx, self.position.1 + dy);
        let inward = match self.return_edge {
            Edge::Left => candidate.0 - self.entry_boundary,
            Edge::Right => self.entry_boundary - candidate.0,
            Edge::Top => candidate.1 - self.entry_boundary,
            Edge::Bottom => self.entry_boundary - candidate.1,
        };
        if !self.armed && inward >= ARM_DISTANCE {
            self.armed = true;
        }
        let outward = match self.return_edge {
            Edge::Left => dx < 0.0,
            Edge::Right => dx > 0.0,
            Edge::Top => dy < 0.0,
            Edge::Bottom => dy > 0.0,
        };
        let orth = match self.return_edge {
            Edge::Left | Edge::Right => candidate.1,
            Edge::Top | Edge::Bottom => candidate.0,
        };
        let on_segment = self.return_segments.iter().any(|s| {
            let (lo, hi) = match self.return_edge {
                Edge::Left | Edge::Right => (f64::from(s.start.1), f64::from(s.end.1)),
                Edge::Top | Edge::Bottom => (f64::from(s.start.0), f64::from(s.end.0)),
            };
            (lo..=hi).contains(&orth)
        });
        if self.armed && outward && inward < 0.0 && on_segment {
            let fraction =
                ((orth - self.span.0) / (self.span.1 - self.span.0).max(1.0)).clamp(0.0, 1.0);
            return MotionResult::Return(EdgePosition {
                edge: match self.return_edge {
                    Edge::Left => ProtocolEdge::Left,
                    Edge::Right => ProtocolEdge::Right,
                    Edge::Top => ProtocolEdge::Top,
                    Edge::Bottom => ProtocolEdge::Bottom,
                },
                fraction: fraction as f32,
            });
        }
        // Only explicit, ready topology exits may leave the receiver. Internal
        // monitor seams and unconfigured directions remain bounded locally.
        for segment in &self.exits {
            let (outward, crossed, orth, lo, hi) = match segment.edge {
                Edge::Left => (
                    dx < 0.0,
                    candidate.0 < f64::from(segment.start.0),
                    candidate.1,
                    segment.start.1,
                    segment.end.1,
                ),
                Edge::Right => (
                    dx > 0.0,
                    candidate.0 >= f64::from(segment.start.0),
                    candidate.1,
                    segment.start.1,
                    segment.end.1,
                ),
                Edge::Top => (
                    dy < 0.0,
                    candidate.1 < f64::from(segment.start.1),
                    candidate.0,
                    segment.start.0,
                    segment.end.0,
                ),
                Edge::Bottom => (
                    dy > 0.0,
                    candidate.1 >= f64::from(segment.start.1),
                    candidate.0,
                    segment.start.0,
                    segment.end.0,
                ),
            };
            if outward && crossed && (f64::from(lo)..=f64::from(hi)).contains(&orth) {
                let edge = match segment.edge {
                    Edge::Left => ProtocolEdge::Left,
                    Edge::Right => ProtocolEdge::Right,
                    Edge::Top => ProtocolEdge::Top,
                    Edge::Bottom => ProtocolEdge::Bottom,
                };
                return MotionResult::Return(EdgePosition {
                    edge,
                    fraction: ((orth - f64::from(lo)) / f64::from((hi - lo).max(1))).clamp(0.0, 1.0)
                        as f32,
                });
            }
        }
        self.position = self.nearest_valid(candidate);
        let (x, y) = self.position();
        MotionResult::Position(x, y)
    }

    fn nearest_valid(&self, point: (f64, f64)) -> (f64, f64) {
        self.zones
            .iter()
            .map(|zone| {
                let x = point.0.clamp(
                    f64::from(zone.x) + 0.5,
                    f64::from(zone.x) + f64::from(zone.width) - 0.5,
                );
                let y = point.1.clamp(
                    f64::from(zone.y) + 0.5,
                    f64::from(zone.y) + f64::from(zone.height) - 0.5,
                );
                let distance = (point.0 - x).powi(2) + (point.1 - y).powi(2);
                ((x, y), distance)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(point, _)| point)
            .unwrap_or(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single(edge: ProtocolEdge, fraction: f32) -> RemotePointer {
        RemotePointer::new(
            vec![Zone {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }],
            EdgePosition { edge, fraction },
        )
        .unwrap()
    }

    #[test]
    fn inverse_edges_and_orthogonal_position() {
        for edge in [
            ProtocolEdge::Left,
            ProtocolEdge::Right,
            ProtocolEdge::Top,
            ProtocolEdge::Bottom,
        ] {
            assert_eq!(edge.opposite().opposite(), edge);
            let pointer = single(edge, 0.6);
            let (x, y) = pointer.position();
            let measured = if matches!(edge, ProtocolEdge::Left | ProtocolEdge::Right) {
                f64::from(y) / 1079.0
            } else {
                f64::from(x) / 1919.0
            };
            assert!((measured - 0.6).abs() < 0.01);
        }
    }

    #[test]
    fn inward_motion_arms_and_outward_crossing_returns() {
        let mut pointer = single(ProtocolEdge::Left, 0.6);
        assert!(matches!(
            pointer.motion(-100.0, 0.0),
            MotionResult::Position(..)
        ));
        assert!(matches!(
            pointer.motion(100.0, 0.0),
            MotionResult::Position(..)
        ));
        assert!(matches!(
            pointer.motion(-200.0, 0.0),
            MotionResult::Return(EdgePosition {
                edge: ProtocolEdge::Left,
                ..
            })
        ));
    }

    #[test]
    fn outward_at_entry_does_not_return_or_oscillate() {
        let mut pointer = single(ProtocolEdge::Left, 0.5);
        for _ in 0..10 {
            assert!(matches!(
                pointer.motion(-20.0, 0.0),
                MotionResult::Position(..)
            ));
        }
        assert!(matches!(
            pointer.motion(100.0, 0.0),
            MotionResult::Position(..)
        ));
        assert!(matches!(
            pointer.motion(0.0, 0.0),
            MotionResult::Position(..)
        ));
    }

    #[test]
    fn all_four_edges_require_inward_then_directional_crossing() {
        for (edge, inward, outward) in [
            (ProtocolEdge::Left, (100.0, 0.0), (-200.0, 0.0)),
            (ProtocolEdge::Right, (-100.0, 0.0), (200.0, 0.0)),
            (ProtocolEdge::Top, (0.0, 100.0), (0.0, -200.0)),
            (ProtocolEdge::Bottom, (0.0, -100.0), (0.0, 200.0)),
        ] {
            let mut pointer = single(edge, 0.4);
            assert!(matches!(
                pointer.motion(outward.0, outward.1),
                MotionResult::Position(..)
            ));
            assert!(matches!(
                pointer.motion(inward.0, inward.1),
                MotionResult::Position(..)
            ));
            assert!(matches!(
                pointer.motion(0.0, 0.0),
                MotionResult::Position(..)
            ));
            let result = pointer.motion(outward.0, outward.1);
            assert!(
                matches!(result, MotionResult::Return(EdgePosition { edge: returned, .. }) if returned == edge)
            );
        }
    }

    #[test]
    fn partial_shared_edge_is_not_a_return_boundary() {
        let mut pointer = RemotePointer::new(
            vec![
                Zone {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 200,
                },
                Zone {
                    x: 100,
                    y: 0,
                    width: 100,
                    height: 100,
                },
            ],
            EdgePosition {
                edge: ProtocolEdge::Right,
                fraction: 0.75,
            },
        )
        .unwrap();
        // At 75% height the external right edge belongs to the first zone.
        assert!(matches!(
            pointer.motion(-80.0, 0.0),
            MotionResult::Position(..)
        ));
        assert!(matches!(
            pointer.motion(200.0, 0.0),
            MotionResult::Return(_)
        ));
    }
}

#[cfg(test)]
mod topology_tests {
    use super::*;
    fn pointer(entry: ProtocolEdge, exits: Vec<ProtocolEdge>) -> RemotePointer {
        RemotePointer::with_exits(
            vec![Zone {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }],
            EdgePosition {
                edge: entry,
                fraction: 0.5,
            },
            exits,
        )
        .unwrap()
    }
    #[test]
    fn every_cardinal_forward_exit_is_directional_and_entry_return_is_preserved() {
        for (entry, forward, inward, outward) in [
            (
                ProtocolEdge::Left,
                ProtocolEdge::Right,
                (100.0, 0.0),
                (3000.0, 0.0),
            ),
            (
                ProtocolEdge::Right,
                ProtocolEdge::Left,
                (-100.0, 0.0),
                (-3000.0, 0.0),
            ),
            (
                ProtocolEdge::Top,
                ProtocolEdge::Bottom,
                (0.0, 100.0),
                (0.0, 3000.0),
            ),
            (
                ProtocolEdge::Bottom,
                ProtocolEdge::Top,
                (0.0, -100.0),
                (0.0, -3000.0),
            ),
        ] {
            let mut routed = pointer(entry, vec![forward]);
            assert!(matches!(
                routed.motion(inward.0, inward.1),
                MotionResult::Position(..)
            ));
            assert!(
                matches!(routed.motion(outward.0,outward.1),MotionResult::Return(EdgePosition{edge,..}) if edge==forward)
            );
            let mut returning = pointer(entry, vec![forward]);
            returning.motion(inward.0, inward.1);
            assert!(
                matches!(returning.motion(-outward.0,-outward.1),MotionResult::Return(EdgePosition{edge,..}) if edge==entry)
            );
        }
    }
    #[test]
    fn unavailable_exits_clamp_and_recovery_refresh_allows_intentional_crossing() {
        let mut routed = pointer(ProtocolEdge::Left, Vec::new());
        assert!(matches!(
            routed.motion(3000.0, 0.0),
            MotionResult::Position(..)
        ));
        routed.update_exits(vec![ProtocolEdge::Right]).unwrap();
        assert!(matches!(
            routed.motion(10.0, 0.0),
            MotionResult::Return(EdgePosition {
                edge: ProtocolEdge::Right,
                ..
            })
        ));
        routed.update_exits(Vec::new()).unwrap();
        assert!(matches!(
            routed.motion(10.0, 0.0),
            MotionResult::Position(..)
        ));
        assert!(routed
            .update_exits(vec![ProtocolEdge::Left, ProtocolEdge::Left])
            .is_err());
        assert!(RemotePointer::with_exits(
            vec![Zone {
                x: 0,
                y: 0,
                width: 100,
                height: 100
            }],
            EdgePosition {
                edge: ProtocolEdge::Left,
                fraction: 0.5
            },
            vec![ProtocolEdge::Right; 5]
        )
        .is_err());
    }
}
