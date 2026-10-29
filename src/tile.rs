//! Web-Mercator tile mathematics and the tile grid that mapsforge derives from a bounding box.

use std::f64::consts::PI;

/// Converts degrees to microdegrees (the unit used inside mapsforge files).
pub fn to_micro(degrees: f64) -> i32 {
    (degrees * 1e6).round() as i32
}

fn tiles_per_axis(zoom: u8) -> f64 {
    (1u64 << zoom) as f64
}

/// Fractional tile X for a longitude in degrees.
pub fn lon_to_x(lon: f64, zoom: u8) -> f64 {
    (lon + 180.0) / 360.0 * tiles_per_axis(zoom)
}

/// Fractional tile Y for a latitude in degrees.
pub fn lat_to_y(lat: f64, zoom: u8) -> f64 {
    let rad = lat.to_radians();
    (1.0 - (rad.tan() + 1.0 / rad.cos()).ln() / PI) / 2.0 * tiles_per_axis(zoom)
}

/// Longitude in degrees of a fractional tile X.
pub fn x_to_lon(x: f64, zoom: u8) -> f64 {
    x / tiles_per_axis(zoom) * 360.0 - 180.0
}

/// Latitude in degrees of a fractional tile Y.
pub fn y_to_lat(y: f64, zoom: u8) -> f64 {
    (PI * (1.0 - 2.0 * y / tiles_per_axis(zoom))).sinh().atan().to_degrees()
}

/// Integer tile column containing a microdegree longitude, clamped to the world.
pub fn tile_x_of(lon_micro: i32, zoom: u8) -> i64 {
    let max = (1i64 << zoom) - 1;
    (lon_to_x(f64::from(lon_micro) / 1e6, zoom).floor() as i64).clamp(0, max)
}

/// Integer tile row containing a microdegree latitude, clamped to the world.
pub fn tile_y_of(lat_micro: i32, zoom: u8) -> i64 {
    let max = (1i64 << zoom) - 1;
    (lat_to_y(f64::from(lat_micro) / 1e6, zoom).floor() as i64).clamp(0, max)
}

/// A bounding box in microdegrees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BBox {
    pub min_lat: i32,
    pub min_lon: i32,
    pub max_lat: i32,
    pub max_lon: i32,
}

impl BBox {
    /// True when the point lies inside or on the border.
    pub fn contains(&self, lat: i32, lon: i32) -> bool {
        lat >= self.min_lat && lat <= self.max_lat && lon >= self.min_lon && lon <= self.max_lon
    }

    /// Grows the box by `micro` microdegrees on every side.
    pub fn expanded(&self, micro: i32) -> BBox {
        BBox {
            min_lat: self.min_lat.saturating_sub(micro),
            min_lon: self.min_lon.saturating_sub(micro),
            max_lat: self.max_lat.saturating_add(micro),
            max_lon: self.max_lon.saturating_add(micro),
        }
    }

    /// True when the two boxes overlap or touch.
    pub fn intersects(&self, other: &BBox) -> bool {
        self.min_lat <= other.max_lat
            && self.max_lat >= other.min_lat
            && self.min_lon <= other.max_lon
            && self.max_lon >= other.min_lon
    }
}

/// An XYZ tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    pub zoom: u8,
    pub x: u32,
    pub y: u32,
}

impl Tile {
    /// North-west corner as `(lat, lon)` in microdegrees.
    pub fn origin(&self) -> (i32, i32) {
        (
            to_micro(y_to_lat(f64::from(self.y), self.zoom)),
            to_micro(x_to_lon(f64::from(self.x), self.zoom)),
        )
    }

    /// Bounding box of the tile in microdegrees.
    pub fn bbox(&self) -> BBox {
        let (max_lat, min_lon) = self.origin();
        let south_east = Tile { zoom: self.zoom, x: self.x + 1, y: self.y + 1 }.origin();
        BBox { min_lat: south_east.0, min_lon, max_lat, max_lon: south_east.1 }
    }

    /// Validates that the coordinates exist at the zoom level.
    pub fn is_valid(&self) -> bool {
        self.zoom <= 20 && u64::from(self.x) < (1u64 << self.zoom) && u64::from(self.y) < (1u64 << self.zoom)
    }
}

/// The rectangular block of tiles covering a bounding box at one zoom level.
///
/// Mapsforge readers compute this grid from the header bounding box, so writer and reader must agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub zoom: u8,
    pub min_x: u32,
    pub min_y: u32,
    pub cols: u32,
    pub rows: u32,
}

impl Grid {
    /// Derives the grid from a bounding box exactly like mapsforge does.
    pub fn from_bbox(bbox: &BBox, zoom: u8) -> Grid {
        let min_x = tile_x_of(bbox.min_lon, zoom);
        let max_x = tile_x_of(bbox.max_lon, zoom);
        let min_y = tile_y_of(bbox.max_lat, zoom);
        let max_y = tile_y_of(bbox.min_lat, zoom);
        Grid {
            zoom,
            min_x: min_x as u32,
            min_y: min_y as u32,
            cols: (max_x - min_x + 1) as u32,
            rows: (max_y - min_y + 1) as u32,
        }
    }

    /// Number of tiles in the grid.
    pub fn len(&self) -> usize {
        self.cols as usize * self.rows as usize
    }

    /// True when the grid has no tiles (never the case for grids built from a box).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Row-major index of a tile coordinate, if it lies in the grid.
    pub fn index_of(&self, x: i64, y: i64) -> Option<usize> {
        let dx = x - i64::from(self.min_x);
        let dy = y - i64::from(self.min_y);
        if dx < 0 || dy < 0 || dx >= i64::from(self.cols) || dy >= i64::from(self.rows) {
            return None;
        }
        Some(dy as usize * self.cols as usize + dx as usize)
    }

    /// Tile at a row-major index.
    pub fn tile_at(&self, index: usize) -> Tile {
        Tile {
            zoom: self.zoom,
            x: self.min_x + (index % self.cols as usize) as u32,
            y: self.min_y + (index / self.cols as usize) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_round_trip() {
        let x = lon_to_x(5.0, 10);
        let y = lat_to_y(52.0, 10);
        assert!((x_to_lon(x, 10) - 5.0).abs() < 1e-9);
        assert!((y_to_lat(y, 10) - 52.0).abs() < 1e-9);
    }

    #[test]
    fn reference_tile_bbox() {
        // Tile 8/131/84 is the sample tile referenced by the project.
        let bbox = Tile { zoom: 8, x: 131, y: 84 }.bbox();
        assert_eq!(bbox.min_lon, 4_218_750);
        assert_eq!(bbox.max_lon, 5_625_000);
        assert_eq!(bbox.min_lat, 51_618_017);
        assert_eq!(bbox.max_lat, 52_482_780);
    }

    #[test]
    fn tile_validity() {
        assert!(Tile { zoom: 8, x: 255, y: 0 }.is_valid());
        assert!(!Tile { zoom: 8, x: 256, y: 0 }.is_valid());
        assert!(!Tile { zoom: 8, x: 0, y: 256 }.is_valid());
        assert!(!Tile { zoom: 30, x: 0, y: 0 }.is_valid());
    }

    #[test]
    fn bbox_operations() {
        let b = BBox { min_lat: 0, min_lon: 0, max_lat: 10, max_lon: 10 };
        assert!(b.contains(5, 5));
        assert!(b.contains(10, 0));
        assert!(!b.contains(11, 5));
        assert!(!b.contains(5, -1));
        let e = b.expanded(5);
        assert_eq!((e.min_lat, e.max_lon), (-5, 15));
        assert!(b.intersects(&BBox { min_lat: 10, min_lon: 10, max_lat: 20, max_lon: 20 }));
        assert!(!b.intersects(&BBox { min_lat: 11, min_lon: 0, max_lat: 20, max_lon: 20 }));
        assert!(!b.intersects(&BBox { min_lat: 0, min_lon: 11, max_lat: 20, max_lon: 20 }));
        assert_eq!(BBox { min_lat: i32::MIN, min_lon: 0, max_lat: i32::MAX, max_lon: 0 }.expanded(1).max_lat, i32::MAX);
    }

    #[test]
    fn grid_indexing() {
        let grid = Grid { zoom: 10, min_x: 10, min_y: 20, cols: 3, rows: 2 };
        assert_eq!(grid.len(), 6);
        assert!(!grid.is_empty());
        assert_eq!(grid.index_of(10, 20), Some(0));
        assert_eq!(grid.index_of(12, 21), Some(5));
        assert_eq!(grid.index_of(13, 20), None);
        assert_eq!(grid.index_of(9, 20), None);
        assert_eq!(grid.index_of(10, 22), None);
        assert_eq!(grid.index_of(10, 19), None);
        assert_eq!(grid.tile_at(4), Tile { zoom: 10, x: 11, y: 21 });
    }

    #[test]
    fn grid_from_bbox_includes_boundary_tile() {
        // The east edge lies exactly on a tile border, which mapsforge counts as an extra column.
        let bbox = Tile { zoom: 8, x: 131, y: 84 }.bbox();
        let grid = Grid::from_bbox(&bbox, 10);
        assert_eq!((grid.min_x, grid.min_y), (524, 336));
        assert!(grid.cols >= 4 && grid.rows >= 4);
        assert_eq!(tile_x_of(-200_000_000, 3), 0);
        assert_eq!(tile_y_of(89_000_000, 3), 0);
    }
}
