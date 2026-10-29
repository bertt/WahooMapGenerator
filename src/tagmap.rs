//! Mapping from OpenStreetMap tags to the Wahoo tag vocabulary stored in the map header.
//!
//! The rules live in `data/tag-mapping.csv` (generated from a reference Wahoo tile, see
//! `tools/gen_tag_mapping.py`). The order of first appearance of each Wahoo tag defines its id.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

const EMBEDDED: &str = include_str!("../data/tag-mapping.csv");
const EMBEDDED_MIN_SIZES: &str = include_str!("../data/area-min-size.csv");

/// Maximum number of tags per feature (the format stores the count in 4 bits).
pub const MAX_TAGS: usize = 15;

/// Kind of feature a rule applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Poi,
    Way,
}

#[derive(Clone, Debug)]
struct Rule {
    tag_id: u32,
    min_zoom: u8,
    primary: bool,
}

#[derive(Default)]
struct KindMapping {
    tags: Vec<String>,
    ids: HashMap<String, u32>,
    rules: HashMap<String, HashMap<String, Vec<Rule>>>,
}

/// The result of classifying an OSM element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classified {
    pub tags: Vec<u32>,
    pub zoom: u8,
}

/// Parses one `key=value,min_size` line.
fn parse_min_size(line: &str) -> Result<((String, String), i32)> {
    let (tag, size) = line.split_once(',').context("expected osm_tag,min_size")?;
    let (key, value) = tag.split_once('=').context("osm_tag is not key=value")?;
    let size: i32 = size.trim().parse().context("invalid min_size")?;
    Ok(((key.to_string(), value.to_string()), size))
}

/// All tag rules.
pub struct TagMapping {
    poi: KindMapping,
    way: KindMapping,
    min_sizes: HashMap<String, HashMap<String, i32>>,
}

impl TagMapping {
    /// The mapping compiled into the binary, including the minimum way sizes.
    pub fn embedded() -> Self {
        Self::parse(EMBEDDED)
            .expect("embedded tag mapping is valid")
            .with_min_sizes(EMBEDDED_MIN_SIZES)
            .expect("embedded area sizes are valid")
    }

    /// Adds `osm_tag,min_size` lines (`#` starts a comment line): ways smaller than `min_size`
    /// microdegrees lose that OSM tag before classification, see [`TagMapping::classify_way`].
    pub fn with_min_sizes(mut self, csv: &str) -> Result<Self> {
        for (n, line) in csv.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (tag, size) = parse_min_size(line).with_context(|| format!("area size line {}", n + 1))?;
            self.min_sizes.entry(tag.0).or_default().insert(tag.1, size);
        }
        Ok(self)
    }

    /// Like [`TagMapping::classify`] for ways, but ignores tags whose configured minimum size is
    /// larger than `size` (the largest bounding-box side of the way in microdegrees).
    pub fn classify_way(&self, osm_tags: &[(String, String)], size: i32) -> Option<Classified> {
        if self.min_sizes.is_empty() {
            return self.classify(Kind::Way, osm_tags);
        }
        let kept: Vec<(String, String)> = osm_tags
            .iter()
            .filter(|(k, v)| self.min_sizes.get(k).and_then(|m| m.get(v)).is_none_or(|min| size >= *min))
            .cloned()
            .collect();
        self.classify(Kind::Way, &kept)
    }

    /// Parses `kind,osm_tag,wahoo_tag,min_zoom,primary` lines. `#` starts a comment line.
    /// An empty `osm_tag` declares a header tag that no OSM tag maps to.
    pub fn parse(csv: &str) -> Result<Self> {
        let mut mapping = TagMapping { poi: KindMapping::default(), way: KindMapping::default(), min_sizes: HashMap::new() };
        for (n, line) in csv.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            mapping.parse_line(line).with_context(|| format!("tag mapping line {}", n + 1))?;
        }
        Ok(mapping)
    }

    fn parse_line(&mut self, line: &str) -> Result<()> {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() != 5 {
            bail!("expected 5 fields, found {}", fields.len());
        }
        let target = match fields[0] {
            "poi" => &mut self.poi,
            "way" => &mut self.way,
            other => bail!("unknown kind '{other}'"),
        };
        let min_zoom: u8 = fields[3].parse().context("invalid min_zoom")?;
        let primary = match fields[4] {
            "true" => true,
            "false" => false,
            other => bail!("invalid primary flag '{other}'"),
        };
        let wahoo = fields[2];
        let next_id = target.tags.len() as u32;
        let tag_id = *target.ids.entry(wahoo.to_string()).or_insert(next_id);
        if tag_id == next_id {
            target.tags.push(wahoo.to_string());
        }
        if let Some((key, value)) = fields[1].split_once('=') {
            target
                .rules
                .entry(key.to_string())
                .or_default()
                .entry(value.to_string())
                .or_default()
                .push(Rule { tag_id, min_zoom, primary });
        } else if !fields[1].is_empty() {
            bail!("osm_tag '{}' is not key=value", fields[1]);
        }
        Ok(())
    }

    /// The header tag list (`key=value` strings) for a kind, indexed by tag id.
    pub fn tags(&self, kind: Kind) -> &[String] {
        &self.of(kind).tags
    }

    fn of(&self, kind: Kind) -> &KindMapping {
        match kind {
            Kind::Poi => &self.poi,
            Kind::Way => &self.way,
        }
    }

    /// Translates OSM tags into Wahoo tag ids.
    ///
    /// Returns `None` unless at least one *primary* tag matches, so that elements which only carry
    /// modifiers (for example `surface=asphalt`) are not emitted. The zoom is the lowest zoom of the
    /// primary tags.
    pub fn classify(&self, kind: Kind, osm_tags: &[(String, String)]) -> Option<Classified> {
        let mapping = self.of(kind);
        let mut matched: Vec<(u32, bool)> = Vec::new();
        let mut zoom: Option<u8> = None;
        for (key, value) in osm_tags {
            let Some(rules) = mapping.rules.get(key).and_then(|values| values.get(value)) else {
                continue;
            };
            for rule in rules {
                if rule.primary {
                    zoom = Some(zoom.map_or(rule.min_zoom, |z| z.min(rule.min_zoom)));
                }
                match matched.iter_mut().find(|(id, _)| *id == rule.tag_id) {
                    Some(existing) => existing.1 |= rule.primary,
                    None => matched.push((rule.tag_id, rule.primary)),
                }
            }
        }
        let zoom = zoom?;
        matched.sort_by_key(|(_, primary)| !*primary);
        matched.truncate(MAX_TAGS);
        Some(Classified { tags: matched.into_iter().map(|(id, _)| id).collect(), zoom })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    const CSV: &str = "# comment\n\n\
        poi,amenity=drinking_water,amenity=drinking_water,10,true\n\
        poi,drinking_water=yes,drinking_water=yes,10,false\n\
        poi,,hike_node=rwn,10,true\n\
        way,highway=path,highway=path,10,true\n\
        way,surface=concrete,surface=smooth_paved,10,false\n\
        way,surface=sett,surface=smooth_paved,10,false\n\
        way,highway=track,highway=track,8,true\n";

    #[test]
    fn small_ways_lose_filtered_tags() {
        let csv = "# comment\n\nway,landuse=grass,landuse=grass,10,true\nway,leisure=park,leisure=park,10,true\n";
        let m = TagMapping::parse(csv).unwrap().with_min_sizes("# c\n\nlanduse=grass,1000\n").unwrap();
        let tags = [t("landuse", "grass"), t("leisure", "park"), t("name", "x")];
        assert_eq!(m.classify_way(&tags, 999).unwrap().tags, [1]);
        assert_eq!(m.classify_way(&tags, 1000).unwrap().tags.len(), 2);
        assert!(m.classify_way(&[t("landuse", "grass")], 5).is_none());
        let plain = TagMapping::parse(csv).unwrap();
        assert!(plain.classify_way(&[t("landuse", "grass")], 5).is_some());
    }

    #[test]
    fn invalid_min_sizes_are_errors() {
        for bad in ["nocomma", "novalue,5", "a=b,five"] {
            let err = TagMapping::parse(CSV).unwrap().with_min_sizes(bad).err().unwrap();
            assert!(err.to_string().starts_with("area size line 1"), "{bad}");
        }
    }

    #[test]
    fn parses_tags_in_first_seen_order() {
        let m = TagMapping::parse(CSV).unwrap();
        assert_eq!(m.tags(Kind::Poi), ["amenity=drinking_water", "drinking_water=yes", "hike_node=rwn"]);
        assert_eq!(m.tags(Kind::Way), ["highway=path", "surface=smooth_paved", "highway=track"]);
    }

    #[test]
    fn classifies_primary_first_and_dedupes() {
        let m = TagMapping::parse(CSV).unwrap();
        let c = m
            .classify(Kind::Poi, &[t("drinking_water", "yes"), t("amenity", "drinking_water"), t("x", "y")])
            .unwrap();
        assert_eq!(c, Classified { tags: vec![0, 1], zoom: 10 });
        let w = m
            .classify(Kind::Way, &[t("surface", "concrete"), t("surface", "sett"), t("highway", "track"), t("highway", "path")])
            .unwrap();
        assert_eq!(w.tags, vec![2, 0, 1]);
        assert_eq!(w.zoom, 8);
    }

    #[test]
    fn modifier_only_elements_are_skipped() {
        let m = TagMapping::parse(CSV).unwrap();
        assert!(m.classify(Kind::Way, &[t("surface", "sett")]).is_none());
        assert!(m.classify(Kind::Way, &[t("name", "x")]).is_none());
        assert!(m.classify(Kind::Poi, &[t("drinking_water", "yes")]).is_none());
    }

    #[test]
    fn primary_flag_is_merged_for_shared_tag_ids() {
        let m = TagMapping::parse("poi,a=b,x=y,5,false\npoi,c=d,x=y,7,true\n").unwrap();
        let c = m.classify(Kind::Poi, &[t("a", "b"), t("c", "d")]).unwrap();
        assert_eq!(c, Classified { tags: vec![0], zoom: 7 });
    }

    #[test]
    fn tag_count_is_limited() {
        let mut csv = String::new();
        let mut tags = Vec::new();
        for i in 0..20 {
            csv.push_str(&format!("poi,k{i}=v,k{i}=v,5,true\n"));
            tags.push(t(&format!("k{i}"), "v"));
        }
        let m = TagMapping::parse(&csv).unwrap();
        assert_eq!(m.classify(Kind::Poi, &tags).unwrap().tags.len(), MAX_TAGS);
    }

    #[test]
    fn invalid_lines_are_rejected() {
        for bad in [
            "poi,a=b,c=d,5",
            "node,a=b,c=d,5,true",
            "poi,a=b,c=d,x,true",
            "poi,a=b,c=d,5,maybe",
            "poi,ab,c=d,5,true",
        ] {
            assert!(TagMapping::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn embedded_mapping_matches_reference_vocabulary() {
        let m = TagMapping::embedded();
        assert_eq!(m.tags(Kind::Poi).len(), 118);
        assert_eq!(m.tags(Kind::Way).len(), 195);
        assert_eq!(m.tags(Kind::Poi)[0], "barrier=gate");
        let water = m.classify(Kind::Poi, &[t("amenity", "drinking_water")]).unwrap();
        assert_eq!(m.tags(Kind::Poi)[water.tags[0] as usize], "amenity=drinking_water");
        let way = m.classify(Kind::Way, &[t("highway", "cycleway"), t("bicycle", "yes"), t("surface", "paving_stones")]).unwrap();
        let names: Vec<_> = way.tags.iter().map(|i| m.tags(Kind::Way)[*i as usize].as_str()).collect();
        assert_eq!(names, ["highway=cycleway", "bicycle=bic_yes", "surface=smooth_paved"]);
    }
}
