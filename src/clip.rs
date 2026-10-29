//! Clips ways to a rectangle, like the reference Wahoo tiles do.
//!
//! Closed rings (first point equals last point) are treated as polygons and clipped with the
//! Sutherland-Hodgman algorithm; every other ring is a polyline and is clipped segment by
//! segment with the Liang-Barsky algorithm, which may split it into several pieces.

use crate::model::{Point, Way};
use crate::tile::BBox;

/// Margin around each tile that the reference tiles keep, in metres.
pub const ENLARGEMENT_METERS: f64 = 20.0;
const METERS_PER_DEGREE: f64 = 111_320.0;

/// Grows `bbox` by [`ENLARGEMENT_METERS`] on every side (longitude scaled by the centre latitude).
pub fn enlarge(bbox: &BBox) -> BBox {
    let dlat = (ENLARGEMENT_METERS / METERS_PER_DEGREE * 1e6).round() as i32;
    let centre = f64::from(bbox.min_lat / 2 + bbox.max_lat / 2) / 1e6;
    let dlon = (f64::from(dlat) / centre.to_radians().cos()).round() as i32;
    BBox {
        min_lat: bbox.min_lat.saturating_sub(dlat),
        min_lon: bbox.min_lon.saturating_sub(dlon),
        max_lat: bbox.max_lat.saturating_add(dlat),
        max_lon: bbox.max_lon.saturating_add(dlon),
    }
}

/// Returns the part of `way` inside `bbox`, or `None` if nothing remains.
pub fn clip_way(way: &Way, bbox: &BBox) -> Option<Way> {
    let rings: Vec<Vec<Point>> = way.rings.iter().flat_map(|ring| clip_ring(ring, bbox)).collect();
    if rings.is_empty() {
        return None;
    }
    Some(Way { rings, ..way.clone() })
}

fn clip_ring(ring: &[Point], bbox: &BBox) -> Vec<Vec<Point>> {
    if ring.len() >= 4 && ring.first() == ring.last() {
        clip_polygon(ring, bbox).into_iter().collect()
    } else {
        clip_line(ring, bbox)
    }
}

fn lerp(a: i32, b: i32, t: f64) -> i32 {
    (f64::from(a) + t * (f64::from(b) - f64::from(a))).round() as i32
}

/// Clips one segment; returns the visible end points.
fn clip_segment(a: Point, b: Point, r: &BBox) -> Option<(Point, Point)> {
    let (dlat, dlon) = (f64::from(b.lat) - f64::from(a.lat), f64::from(b.lon) - f64::from(a.lon));
    let checks = [
        (-dlat, f64::from(a.lat) - f64::from(r.min_lat)),
        (dlat, f64::from(r.max_lat) - f64::from(a.lat)),
        (-dlon, f64::from(a.lon) - f64::from(r.min_lon)),
        (dlon, f64::from(r.max_lon) - f64::from(a.lon)),
    ];
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in checks {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else if p < 0.0 {
            t0 = t0.max(q / p);
        } else {
            t1 = t1.min(q / p);
        }
    }
    if t0 > t1 {
        return None;
    }
    let at = |t: f64| {
        if t == 0.0 {
            a
        } else if t == 1.0 {
            b
        } else {
            Point { lat: lerp(a.lat, b.lat, t), lon: lerp(a.lon, b.lon, t) }
        }
    };
    Some((at(t0), at(t1)))
}

fn clip_line(line: &[Point], bbox: &BBox) -> Vec<Vec<Point>> {
    let mut pieces = Vec::new();
    let mut current: Vec<Point> = Vec::new();
    for pair in line.windows(2) {
        let Some((a, b)) = clip_segment(pair[0], pair[1], bbox) else {
            flush(&mut pieces, &mut current);
            continue;
        };
        if a == b {
            continue;
        }
        if current.last() != Some(&a) {
            flush(&mut pieces, &mut current);
            current.push(a);
        }
        current.push(b);
    }
    flush(&mut pieces, &mut current);
    pieces
}

fn flush(pieces: &mut Vec<Vec<Point>>, current: &mut Vec<Point>) {
    if current.len() >= 2 {
        pieces.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn cross_lat(a: Point, b: Point, lat: i32) -> Point {
    let t = (f64::from(lat) - f64::from(a.lat)) / (f64::from(b.lat) - f64::from(a.lat));
    Point { lat, lon: lerp(a.lon, b.lon, t) }
}

fn cross_lon(a: Point, b: Point, lon: i32) -> Point {
    let t = (f64::from(lon) - f64::from(a.lon)) / (f64::from(b.lon) - f64::from(a.lon));
    Point { lat: lerp(a.lat, b.lat, t), lon }
}

/// One Sutherland-Hodgman pass against a single half plane.
fn clip_half_plane(poly: &[Point], inside: impl Fn(Point) -> bool, cut: impl Fn(Point, Point) -> Point) -> Vec<Point> {
    let mut out = Vec::new();
    for (i, &cur) in poly.iter().enumerate() {
        let prev = poly[(i + poly.len() - 1) % poly.len()];
        match (inside(prev), inside(cur)) {
            (true, true) => out.push(cur),
            (true, false) => out.push(cut(prev, cur)),
            (false, true) => {
                out.push(cut(prev, cur));
                out.push(cur);
            }
            (false, false) => {}
        }
    }
    out
}

fn clip_polygon(ring: &[Point], r: &BBox) -> Option<Vec<Point>> {
    let mut poly = ring[..ring.len() - 1].to_vec();
    poly = clip_half_plane(&poly, |p| p.lat >= r.min_lat, |a, b| cross_lat(a, b, r.min_lat));
    poly = clip_half_plane(&poly, |p| p.lat <= r.max_lat, |a, b| cross_lat(a, b, r.max_lat));
    poly = clip_half_plane(&poly, |p| p.lon >= r.min_lon, |a, b| cross_lon(a, b, r.min_lon));
    poly = clip_half_plane(&poly, |p| p.lon <= r.max_lon, |a, b| cross_lon(a, b, r.max_lon));
    poly.dedup();
    if poly.len() < 3 {
        return None;
    }
    poly.push(poly[0]);
    Some(poly)
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: BBox = BBox { min_lat: 0, min_lon: 0, max_lat: 100, max_lon: 100 };

    fn pt(lat: i32, lon: i32) -> Point {
        Point { lat, lon }
    }

    fn way(ring: Vec<Point>) -> Way {
        Way { rings: vec![ring], tags: vec![3], zoom: 9, name: Some("n".into()), ..Way::default() }
    }

    #[test]
    fn enlarge_adds_about_twenty_metres() {
        let b = enlarge(&BBox { min_lat: 52_000_000, min_lon: 5_000_000, max_lat: 52_100_000, max_lon: 5_100_000 });
        assert_eq!(52_000_000 - b.min_lat, 180);
        assert_eq!(b.max_lat - 52_100_000, 180);
        assert!((290..=295).contains(&(5_000_000 - b.min_lon)));
        assert_eq!(b.max_lon - 5_100_000, 5_000_000 - b.min_lon);
        let edge = enlarge(&BBox { min_lat: i32::MIN, min_lon: i32::MIN, max_lat: i32::MAX, max_lon: i32::MAX });
        assert_eq!((edge.min_lat, edge.max_lon), (i32::MIN, i32::MAX));
    }

    #[test]
    fn line_inside_is_unchanged_and_keeps_attributes() {
        let w = way(vec![pt(10, 10), pt(20, 80), pt(90, 90)]);
        assert_eq!(clip_way(&w, &R), Some(w));
    }

    #[test]
    fn line_outside_is_dropped() {
        assert!(clip_way(&way(vec![pt(200, 200), pt(300, 300)]), &R).is_none());
        assert!(clip_way(&way(vec![pt(-5, -5), pt(-5, 50)]), &R).is_none());
        assert!(clip_way(&Way::default(), &R).is_none());
    }

    #[test]
    fn line_is_cut_at_the_border() {
        let c = clip_way(&way(vec![pt(50, -100), pt(50, 50), pt(50, 300)]), &R).unwrap();
        assert_eq!(c.rings, vec![vec![pt(50, 0), pt(50, 50), pt(50, 100)]]);
    }

    #[test]
    fn line_leaving_and_reentering_is_split() {
        let c = clip_way(&way(vec![pt(10, 10), pt(10, 200), pt(20, 200), pt(20, 10)]), &R).unwrap();
        assert_eq!(c.rings, vec![vec![pt(10, 10), pt(10, 100)], vec![pt(20, 100), pt(20, 10)]]);
    }

    #[test]
    fn parallel_segments_outside_or_touching_a_corner_vanish() {
        assert!(clip_way(&way(vec![pt(150, 10), pt(150, 90)]), &R).is_none());
        assert!(clip_way(&way(vec![pt(-10, 110), pt(0, 100)]), &R).is_none());
        assert!(clip_way(&way(vec![pt(5, 5), pt(5, 5)]), &R).is_none());
        // the segment misses the rectangle although its infinite line crosses it
        assert!(clip_way(&way(vec![pt(160, 50), pt(50, 160)]), &R).is_none());
    }

    #[test]
    fn polygon_inside_is_unchanged() {
        let ring = vec![pt(10, 10), pt(10, 50), pt(50, 50), pt(10, 10)];
        assert_eq!(clip_way(&way(ring.clone()), &R).unwrap().rings, vec![ring]);
    }

    #[test]
    fn polygon_is_clipped_and_stays_closed() {
        let ring = vec![pt(50, 50), pt(50, 200), pt(150, 200), pt(150, 50), pt(50, 50)];
        let c = clip_way(&way(ring), &R).unwrap();
        let out = &c.rings[0];
        assert_eq!(out.first(), out.last());
        assert!(out.iter().all(|p| (0..=100).contains(&p.lat) && (0..=100).contains(&p.lon)));
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn polygon_covering_the_rectangle_becomes_the_rectangle() {
        let ring = vec![pt(-50, -50), pt(-50, 150), pt(150, 150), pt(150, -50), pt(-50, -50)];
        let c = clip_way(&way(ring), &R).unwrap();
        assert_eq!(c.rings[0].len(), 5);
        assert!(c.rings[0].contains(&pt(0, 0)) && c.rings[0].contains(&pt(100, 100)));
    }

    #[test]
    fn polygon_outside_is_dropped() {
        let ring = vec![pt(200, 200), pt(200, 300), pt(300, 300), pt(200, 200)];
        assert!(clip_way(&way(ring), &R).is_none());
    }

    #[test]
    fn polygon_that_degenerates_is_dropped() {
        // touches the rectangle in a single corner point
        let ring = vec![pt(0, 0), pt(-50, 0), pt(0, -50), pt(0, 0)];
        assert!(clip_way(&way(ring), &R).is_none());
    }
}
