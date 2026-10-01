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

/// One normalized orthogonal coordinate space for all external segments in a
/// cardinal direction. Gaps are retained spatially, never made into barriers.
#[derive(Clone, Copy, Debug)]
pub struct EdgeSpan {
    first: f64,
    last: f64,
}
impl EdgeSpan {
    pub fn new(segments: &[OuterSegment], edge: Edge) -> Result<Self, String> {
        let segments: Vec<_> = segments
            .iter()
            .filter(|segment| segment.edge == edge)
            .collect();
        if segments.is_empty() {
            return Err("direction has no reachable external boundary".into());
        }
        let first = segments
            .iter()
            .map(|segment| segment.orthogonal().0)
            .fold(f64::INFINITY, f64::min);
        let last = segments
            .iter()
            .map(|segment| segment.orthogonal().1)
            .fold(f64::NEG_INFINITY, f64::max);
        Ok(Self { first, last })
    }
    pub fn fraction(self, coordinate: f64) -> f32 {
        ((coordinate - self.first) / (self.last - self.first).max(1.0)).clamp(0.0, 1.0) as f32
    }
    pub fn coordinate(self, fraction: f32) -> f64 {
        self.first + f64::from(fraction.clamp(0.0, 1.0)) * (self.last - self.first)
    }
    pub fn cursor_fraction(self, segment: OuterSegment, cursor: (f32, f32)) -> f32 {
        self.fraction(f64::from(match segment.edge {
            Edge::Left | Edge::Right => cursor.1,
            Edge::Top | Edge::Bottom => cursor.0,
        }))
    }
    /// Return to the actual launching segment, clamping a position in a gap or
    /// another output to its closest reachable point, not a synthetic edge.
    pub fn release_position(self, segment: OuterSegment, fraction: f32) -> (f64, f64) {
        let (lo, hi) = segment.orthogonal();
        let coordinate = self.coordinate(fraction).clamp(lo, hi);
        let local = ((coordinate - lo) / (hi - lo).max(1.0)) as f32;
        segment.release_position_at(local)
    }
}
impl OuterSegment {
    fn orthogonal(self) -> (f64, f64) {
        match self.edge {
            Edge::Left | Edge::Right => (f64::from(self.start.1), f64::from(self.end.1)),
            Edge::Top | Edge::Bottom => (f64::from(self.start.0), f64::from(self.end.0)),
        }
    }
    /// Crossing must originate in the output owning this external segment.
    /// A shared seam, or a distant segment at the same y/x, cannot trigger it.
    pub fn crossed(self, zones: &[Zone], from: (f64, f64), to: (f64, f64)) -> bool {
        let Some(zone) = zones.get(self.zone) else {
            return false;
        };
        let Ok(bounds) = zone.bounds() else {
            return false;
        };
        if from.0 < bounds.left as f64
            || from.0 >= bounds.right as f64
            || from.1 < bounds.top as f64
            || from.1 >= bounds.bottom as f64
        {
            return false;
        }
        let (outward, orthogonal) = match self.edge {
            Edge::Left => (to.0 < f64::from(self.start.0) && to.0 < from.0, to.1),
            Edge::Right => (to.0 >= f64::from(self.start.0) && to.0 > from.0, to.1),
            Edge::Top => (to.1 < f64::from(self.start.1) && to.1 < from.1, to.0),
            Edge::Bottom => (to.1 >= f64::from(self.start.1) && to.1 > from.1, to.0),
        };
        let (lo, hi) = self.orthogonal();
        outward && (lo..=hi).contains(&orthogonal)
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

#[cfg(test)]
mod desktop_tests {
    use super::*;
    fn z(x: i32, y: i32, width: u32, height: u32) -> Zone {
        Zone {
            x,
            y,
            width,
            height,
        }
    }
    fn direction(zones: &[Zone], edge: Edge) -> Vec<OuterSegment> {
        all_outer_segments(zones)
            .unwrap()
            .into_iter()
            .filter(|s| s.edge == edge)
            .collect()
    }
    #[test]
    fn stacked_different_resolutions_and_logical_scale_rectangles_keep_external_lefts() {
        // Logical rectangles: a 3840x2160 output at scale 2 is 1920x1080;
        // offsets/resolutions are already transformed by the portal provider.
        let zones = [z(-1920, -1080, 1920, 1080), z(-1280, 0, 1280, 720)];
        let left = direction(&zones, Edge::Left);
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].start, (-1920, -1080));
        assert_eq!(left[1].start, (-1280, 0));
        assert!(outer_segments(&zones, 1, Edge::Top).unwrap().is_empty());
        let bottom = outer_segments(&zones, 0, Edge::Bottom).unwrap();
        assert_eq!(bottom[0].start, (-1920, 0));
        assert_eq!(bottom[0].end, (-1281, 0));
        let span = EdgeSpan::new(&left, Edge::Left).unwrap();
        let fraction = span.cursor_fraction(left[1], (-1280.0, 360.0));
        assert!((span.coordinate(fraction) - 360.0).abs() < 0.001);
        let restored = span.release_position(left[1], fraction);
        assert!((restored.0 + 1279.5).abs() < 0.001);
        assert!((restored.1 - 360.0).abs() < 0.001);
    }
    #[test]
    fn offset_side_by_side_segments_exclude_seams_and_normalize_across_direction() {
        let zones = [z(0, 0, 1920, 1080), z(-2560, 200, 2560, 1440)];
        let left = direction(&zones, Edge::Left);
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].start, (0, 0));
        assert_eq!(left[0].end, (0, 199));
        assert_eq!(left[1].start, (-2560, 200));
        assert_eq!(left[1].end, (-2560, 1639));
        assert!(!left[0].crossed(&zones, (0.5, 500.0), (-10.0, 500.0)));
        assert!(left[0].crossed(&zones, (0.5, 100.0), (-10.0, 100.0)));
        assert!(left[1].crossed(&zones, (-2559.5, 800.0), (-2570.0, 800.0)));
        let span = EdgeSpan::new(&left, Edge::Left).unwrap();
        let fraction = span.cursor_fraction(left[1], (-2560.0, 800.0));
        assert!((fraction - 800.0 / 1639.0).abs() < 0.0001);
        assert!((span.release_position(left[1], fraction).1 - 800.0).abs() < 0.001);
        // Return outside the launching segment clamps inside that segment.
        assert_eq!(span.release_position(left[0], 1.0), (0.5, 199.0));
    }
    #[test]
    fn separated_external_segments_do_not_create_gap_barriers() {
        let zones = [z(0, -200, 100, 100), z(0, 100, 100, 100)];
        let left = direction(&zones, Edge::Left);
        assert_eq!(left.len(), 2);
        assert!(left
            .iter()
            .all(|s| !s.crossed(&zones, (0.5, 0.0), (-1.0, 0.0))));
        assert!(EdgeSpan::new(&[], Edge::Left).is_err());
        assert!(EdgeSpan::new(&left, Edge::Top).is_err());
    }
    #[test]
    fn aligned_vertical_stack_only_outer_top_bottom_and_both_left_segments() {
        let zones = [z(0, 0, 100, 100), z(0, 100, 100, 100)];
        assert!(outer_segments(&zones, 0, Edge::Bottom).unwrap().is_empty());
        assert!(outer_segments(&zones, 1, Edge::Top).unwrap().is_empty());
        assert_eq!(direction(&zones, Edge::Left).len(), 2);
        assert_eq!(direction(&zones, Edge::Top).len(), 1);
        assert_eq!(direction(&zones, Edge::Bottom).len(), 1);
    }
}
