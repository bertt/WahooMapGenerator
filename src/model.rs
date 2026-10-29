//! Feature model shared by the extractor, the writer and the reader.

/// A coordinate in microdegrees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    pub lat: i32,
    pub lon: i32,
}

/// A point of interest. `tags` holds ids into the map's POI tag list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Poi {
    pub point: Point,
    pub layer: i8,
    pub tags: Vec<u32>,
    pub name: Option<String>,
    pub house_number: Option<String>,
    pub elevation: Option<i32>,
    /// Lowest zoom level at which the feature is shown.
    pub zoom: u8,
}

/// A way. `rings` holds one or more coordinate sequences; `tags` holds ids into the way tag list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Way {
    pub rings: Vec<Vec<Point>>,
    pub layer: i8,
    pub tags: Vec<u32>,
    pub name: Option<String>,
    pub house_number: Option<String>,
    pub reference: Option<String>,
    /// Lowest zoom level at which the feature is shown.
    pub zoom: u8,
}

impl Way {
    /// Bounding box over all rings as `(min_lat, min_lon, max_lat, max_lon)`, if there are points.
    pub fn bounds(&self) -> Option<crate::tile::BBox> {
        let mut points = self.rings.iter().flatten();
        let first = points.next()?;
        let mut b = crate::tile::BBox {
            min_lat: first.lat,
            min_lon: first.lon,
            max_lat: first.lat,
            max_lon: first.lon,
        };
        for p in points {
            b.min_lat = b.min_lat.min(p.lat);
            b.max_lat = b.max_lat.max(p.lat);
            b.min_lon = b.min_lon.min(p.lon);
            b.max_lon = b.max_lon.max(p.lon);
        }
        Some(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_cover_all_rings() {
        let way = Way {
            rings: vec![
                vec![Point { lat: 1, lon: 5 }, Point { lat: 3, lon: 2 }],
                vec![Point { lat: -4, lon: 9 }],
            ],
            ..Way::default()
        };
        let b = way.bounds().unwrap();
        assert_eq!((b.min_lat, b.min_lon, b.max_lat, b.max_lon), (-4, 2, 3, 9));
        assert!(Way::default().bounds().is_none());
    }
}
