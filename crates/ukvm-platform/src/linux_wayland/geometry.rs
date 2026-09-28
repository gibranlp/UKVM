//! Logical zone geometry for XDG InputCapture pointer barriers.
//! Intervals are half-open internally; portal barrier endpoints are inclusive.

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Zone {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub struct ZoneSet {
    pub id: u32,
    pub zones: Vec<Zone>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OuterSegment {
    pub zone: usize,
    pub edge: Edge,
    /// Portal coordinates. The second endpoint includes the last pixel.
    pub start: (i32, i32),
    pub end: (i32, i32),
}

impl OuterSegment {
    /// A suggested return position at the center of a boundary pixel inside the source zone.
    /// The compositor may ignore this suggestion.
    pub fn release_position(self) -> (f64, f64) {
        self.release_position_at(0.5)
    }

    pub fn release_position_at(self, fraction: f32) -> (f64, f64) {
        let along = |a: i32, b: i32| {
            f64::from(a) + f64::from(fraction.clamp(0.0, 1.0)) * (f64::from(b) - f64::from(a))
        };
        match self.edge {
            Edge::Left => (
                f64::from(self.start.0) + 0.5,
                along(self.start.1, self.end.1),
            ),
            Edge::Right => (
                f64::from(self.start.0) - 0.5,
                along(self.start.1, self.end.1),
            ),
            Edge::Top => (
                along(self.start.0, self.end.0),
                f64::from(self.start.1) + 0.5,
            ),
            Edge::Bottom => (
                along(self.start.0, self.end.0),
                f64::from(self.start.1) - 0.5,
            ),
        }
    }

    pub fn fraction_for_cursor(self, cursor: (f32, f32)) -> f32 {
        let (value, start, end) = match self.edge {
            Edge::Left | Edge::Right => (cursor.1, self.start.1, self.end.1),
            Edge::Top | Edge::Bottom => (cursor.0, self.start.0, self.end.0),
        };
        ((value - start as f32) / (f64::from(end) - f64::from(start)).max(1.0) as f32)
            .clamp(0.0, 1.0)
    }
}

#[derive(Clone, Copy)]
struct Bounds {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl Zone {
    fn bounds(self) -> Result<Bounds, String> {
        let left = i64::from(self.x);
        let top = i64::from(self.y);
        let right = left + i64::from(self.width);
        let bottom = top + i64::from(self.height);
        if self.width == 0
            || self.height == 0
            || right > i64::from(i32::MAX)
            || bottom > i64::from(i32::MAX)
        {
            return Err("zone has zero size or exceeds portal coordinate range".into());
        }
        Ok(Bounds {
            left,
            top,
            right,
            bottom,
        })
    }
}

/// Returns every maximal external segment on one zone edge. Other zones may
/// cover only part of the edge, so one edge can yield zero or several segments.
pub fn outer_segments(
    zones: &[Zone],
    index: usize,
    edge: Edge,
) -> Result<Vec<OuterSegment>, String> {
    let zone = zones
        .get(index)
        .ok_or("zone index does not exist")?
        .bounds()?;
    let (boundary, start, end) = match edge {
        Edge::Left => (zone.left, zone.top, zone.bottom),
        Edge::Right => (zone.right, zone.top, zone.bottom),
        Edge::Top => (zone.top, zone.left, zone.right),
        Edge::Bottom => (zone.bottom, zone.left, zone.right),
    };
    let mut covered = Vec::<(i64, i64)>::new();
    for (other_index, other) in zones.iter().enumerate() {
        if other_index == index {
            continue;
        }
        let other = other.bounds()?;
        let extends_outward = match edge {
            Edge::Left => other.left < boundary && other.right >= boundary,
            Edge::Right => other.left <= boundary && other.right > boundary,
            Edge::Top => other.top < boundary && other.bottom >= boundary,
            Edge::Bottom => other.top <= boundary && other.bottom > boundary,
        };
        if !extends_outward {
            continue;
        }
        let (other_start, other_end) = match edge {
            Edge::Left | Edge::Right => (other.top, other.bottom),
            Edge::Top | Edge::Bottom => (other.left, other.right),
        };
        let clipped = (start.max(other_start), end.min(other_end));
        if clipped.0 < clipped.1 {
            covered.push(clipped);
        }
    }
    covered.sort_unstable();
    let mut visible = Vec::new();
    let mut cursor = start;
    for (cover_start, cover_end) in covered {
        if cover_start > cursor {
            visible.push((cursor, cover_start));
        }
        cursor = cursor.max(cover_end);
        if cursor >= end {
            break;
        }
    }
    if cursor < end {
        visible.push((cursor, end));
    }
    Ok(visible
        .into_iter()
        .map(|(first, after_last)| {
            let (start, end) = match edge {
                Edge::Left | Edge::Right => ((boundary, first), (boundary, after_last - 1)),
                Edge::Top | Edge::Bottom => ((first, boundary), (after_last - 1, boundary)),
            };
            OuterSegment {
                zone: index,
                edge,
                start: (start.0 as i32, start.1 as i32),
                end: (end.0 as i32, end.1 as i32),
            }
        })
        .collect())
}

pub fn all_outer_segments(zones: &[Zone]) -> Result<Vec<OuterSegment>, String> {
    let mut result = Vec::new();
    for index in 0..zones.len() {
        for edge in Edge::ALL {
            result.extend(outer_segments(zones, index, edge)?);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{all_outer_segments, outer_segments, Edge, OuterSegment, Zone};

    fn zone(x: i32, y: i32, width: u32, height: u32) -> Zone {
        Zone {
            x,
            y,
            width,
            height,
        }
    }

    fn endpoints(segments: &[OuterSegment]) -> Vec<((i32, i32), (i32, i32))> {
        segments.iter().map(|s| (s.start, s.end)).collect()
    }

    #[test]
    fn single_monitor_has_four_outer_edges() {
        let zones = [zone(0, 0, 1920, 1080)];
        assert_eq!(all_outer_segments(&zones).unwrap().len(), 4);
        assert_eq!(
            endpoints(&outer_segments(&zones, 0, Edge::Right).unwrap()),
            vec![((1920, 0), (1920, 1079))]
        );
        assert_eq!(
            endpoints(&outer_segments(&zones, 0, Edge::Bottom).unwrap()),
            vec![((0, 1080), (1919, 1080))]
        );
    }

    #[test]
    fn release_position_preserves_normalized_orthogonal_coordinate() {
        let zones = [zone(-3840, 0, 3840, 2160)];
        let segment = outer_segments(&zones, 0, Edge::Right).unwrap()[0];
        let cursor = segment.release_position_at(0.6);
        assert!((cursor.0 + 0.5).abs() < 0.01);
        assert!((cursor.1 / 2159.0 - 0.6).abs() < 0.001);
        assert!((segment.fraction_for_cursor((0.0, cursor.1 as f32)) - 0.6).abs() < 0.001);
    }

    #[test]
    fn aligned_monitors_hide_shared_edge() {
        let zones = [zone(0, 0, 1920, 1080), zone(1920, 0, 1920, 1080)];
        assert!(outer_segments(&zones, 0, Edge::Right).unwrap().is_empty());
        assert!(outer_segments(&zones, 1, Edge::Left).unwrap().is_empty());
        assert_eq!(
            endpoints(&outer_segments(&zones, 1, Edge::Right).unwrap()),
            vec![((3840, 0), (3840, 1079))]
        );
    }

    #[test]
    fn vertically_offset_monitors_expose_partial_edge() {
        let zones = [zone(0, 0, 100, 200), zone(-100, 0, 100, 100)];
        assert_eq!(
            endpoints(&outer_segments(&zones, 0, Edge::Left).unwrap()),
            vec![((0, 100), (0, 199))]
        );
        assert!(outer_segments(&zones, 1, Edge::Right).unwrap().is_empty());
    }

    #[test]
    fn horizontally_offset_monitors_expose_partial_top() {
        let zones = [zone(0, 0, 200, 100), zone(50, -100, 100, 100)];
        assert_eq!(
            endpoints(&outer_segments(&zones, 0, Edge::Top).unwrap()),
            vec![((0, 0), (49, 0)), ((150, 0), (199, 0))]
        );
    }

    #[test]
    fn overlapping_monitor_geometry_exposes_union_boundary() {
        let zones = [zone(0, 0, 100, 100), zone(50, 50, 100, 100)];
        assert_eq!(
            endpoints(&outer_segments(&zones, 0, Edge::Right).unwrap()),
            vec![((100, 0), (100, 49))]
        );
        assert_eq!(
            endpoints(&outer_segments(&zones, 1, Edge::Left).unwrap()),
            vec![((50, 100), (50, 149))]
        );
    }

    #[test]
    fn multiple_covering_zones_split_one_edge() {
        let zones = [
            zone(0, 0, 100, 300),
            zone(-100, 50, 100, 50),
            zone(-100, 200, 100, 50),
        ];
        assert_eq!(
            endpoints(&outer_segments(&zones, 0, Edge::Left).unwrap()),
            vec![
                ((0, 0), (0, 49)),
                ((0, 100), (0, 199)),
                ((0, 250), (0, 299))
            ]
        );
    }

    #[test]
    fn negative_origin_is_preserved() {
        let zones = [zone(-5760, 0, 1920, 1080)];
        assert_eq!(
            endpoints(&outer_segments(&zones, 0, Edge::Left).unwrap()),
            vec![((-5760, 0), (-5760, 1079))]
        );
    }

    #[test]
    fn rejects_invalid_dimensions() {
        assert!(outer_segments(&[zone(0, 0, 0, 100)], 0, Edge::Left).is_err());
        assert!(outer_segments(&[zone(i32::MAX, 0, 2, 100)], 0, Edge::Right).is_err());
    }
}
