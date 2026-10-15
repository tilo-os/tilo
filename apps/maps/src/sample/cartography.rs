//! Hand-laid, deliberately schematic Munich capture fixture. Coordinates are
//! local offsets from Marienplatz, not a surveyed or navigable street database.
//! The live map continues to use provider imagery and planner geometry.

use super::{Area, AreaKind, Basemap, LabelKind, MapLabel, Rng, Road, RoadClass, cell};
use crate::model::LatLon;

fn points(coords: &[(f64, f64)]) -> Vec<LatLon> {
    coords.iter().map(|&(x, y)| cell(x, y)).collect()
}

/// Sendling → Goetheplatz → Sendlinger Tor → Marienplatz. The intermediate
/// vertices keep the offline road and transit overlays on the same corridor.
pub(super) fn corridor() -> Vec<LatLon> {
    points(&[
        (-6.5, -6.0),
        (-5.8, -4.5),
        (-4.9, -3.2),
        (-3.8, -2.4),
        (-2.45, -1.5),
        (-1.7, -0.9),
        (-0.7, -0.35),
        (0.0, 0.0),
    ])
}

pub(super) fn tram_path() -> Vec<LatLon> {
    points(&[(-2.45, -1.5), (-1.1, -2.4), (0.6, -1.9), (1.5, -1.5)])
}

pub(super) fn bus_path() -> Vec<LatLon> {
    points(&[(1.5, -1.5), (1.0, -0.5), (0.0, 0.0)])
}

pub(super) fn northern_path() -> Vec<LatLon> {
    corridor()
        .into_iter()
        .skip(1)
        .chain(points(&[(0.2, 1.3), (0.6, 2.6)]))
        .collect()
}

// Unequal blocks and offset junctions: long southwest residential frontages
// meet smaller, angled old-town blocks rather than a repeated square lattice.
const JUNCTIONS: &[&[(f64, f64)]] = &[
    &[
        (-16.0, -12.0),
        (-11.0, -12.4),
        (-7.8, -11.8),
        (-4.2, -12.0),
        (-0.5, -11.4),
        (3.0, -11.8),
        (6.0, -11.2),
        (11.0, -12.0),
        (16.0, -11.5),
    ],
    &[
        (-16.0, -9.0),
        (-11.5, -9.6),
        (-7.1, -9.0),
        (-3.8, -8.7),
        (0.2, -9.2),
        (3.3, -8.5),
        (6.3, -8.8),
        (11.0, -8.5),
        (16.0, -8.0),
    ],
    &[
        (-16.0, -6.3),
        (-11.8, -6.8),
        (-6.5, -6.0),
        (-3.0, -6.4),
        (0.8, -6.2),
        (3.7, -5.7),
        (6.8, -5.5),
        (11.5, -5.8),
        (16.0, -5.0),
    ],
    &[
        (-16.0, -4.0),
        (-12.2, -4.3),
        (-5.8, -4.5),
        (-2.7, -4.0),
        (0.8, -3.8),
        (4.2, -3.4),
        (7.0, -3.0),
        (11.4, -3.2),
        (16.0, -2.8),
    ],
    &[
        (-16.0, -1.3),
        (-11.5, -1.8),
        (-4.9, -3.2),
        (-2.45, -1.5),
        (0.6, -1.9),
        (4.0, -1.5),
        (7.2, -0.4),
        (11.8, -0.8),
        (16.0, 0.0),
    ],
    &[
        (-16.0, 1.2),
        (-10.8, 1.0),
        (-5.2, 0.6),
        (-2.9, 0.8),
        (0.0, 0.0),
        (3.0, 0.9),
        (7.4, 2.1),
        (12.0, 2.2),
        (16.0, 2.8),
    ],
    &[
        (-16.0, 4.0),
        (-10.0, 3.5),
        (-5.0, 3.0),
        (-2.5, 2.5),
        (0.6, 2.6),
        (3.4, 3.4),
        (8.0, 4.9),
        (12.2, 5.1),
        (16.0, 5.8),
    ],
    &[
        (-16.0, 7.0),
        (-9.5, 6.5),
        (-4.5, 5.8),
        (-1.7, 5.2),
        (1.0, 5.5),
        (4.2, 6.5),
        (8.6, 7.6),
        (12.4, 8.0),
        (16.0, 8.5),
    ],
    &[
        (-16.0, 11.0),
        (-9.0, 10.5),
        (-4.0, 10.0),
        (-1.2, 9.8),
        (1.5, 10.2),
        (5.2, 10.8),
        (9.2, 11.5),
        (12.8, 12.0),
        (16.0, 12.5),
    ],
];

fn park(kind: AreaKind, coords: &[(f64, f64)]) -> Area {
    Area {
        kind,
        ring: points(coords),
    }
}

fn parks() -> Vec<Area> {
    vec![
        park(
            AreaKind::Park,
            &[
                (-11.6, -5.3),
                (-8.6, -5.0),
                (-8.2, -3.4),
                (-8.8, -0.8),
                (-11.2, -1.2),
            ],
        ),
        park(
            AreaKind::Park,
            &[
                (-3.8, -5.5),
                (-2.2, -5.8),
                (-1.1, -4.0),
                (-1.4, -2.7),
                (-2.7, -3.1),
            ],
        ),
        park(
            AreaKind::Park,
            &[(-0.4, 3.6), (2.3, 3.9), (2.8, 5.8), (0.1, 6.1)],
        ),
        park(
            AreaKind::Park,
            &[
                (6.0, -11.5),
                (7.8, -11.3),
                (8.5, -7.0),
                (7.3, -4.5),
                (7.6, -1.0),
                (8.8, 2.3),
                (10.0, 6.0),
                (8.5, 6.5),
                (7.3, 2.0),
                (6.0, -1.0),
                (5.9, -4.0),
                (6.7, -7.0),
            ],
        ),
    ]
}

fn inside(p: LatLon, ring: &[LatLon]) -> bool {
    let mut contained = false;
    for (a, b) in ring
        .iter()
        .zip(ring.iter().cycle().skip(1))
        .take(ring.len())
    {
        if (a.lat > p.lat) != (b.lat > p.lat)
            && p.lon < (b.lon - a.lon) * (p.lat - a.lat) / (b.lat - a.lat) + a.lon
        {
            contained = !contained;
        }
    }
    contained
}

fn lerp(a: LatLon, b: LatLon, t: f64) -> LatLon {
    LatLon::new(a.lat + (b.lat - a.lat) * t, a.lon + (b.lon - a.lon) * t)
}

/// Broken perimeter frontages leave courtyards, alleys and gaps. A varying
/// depth and occasional return wing make L-shaped and trapezoidal footprints;
/// no building is a little square stamped at the centre of a grid cell.
fn buildings(ring: &[LatLon], random: &mut Rng) -> Vec<Area> {
    let count = ring.len() as f64;
    let center = LatLon::new(
        ring.iter().map(|p| p.lat).sum::<f64>() / count,
        ring.iter().map(|p| p.lon).sum::<f64>() / count,
    );
    let mut out = Vec::new();
    for (a, b) in ring
        .iter()
        .zip(ring.iter().cycle().skip(1))
        .take(ring.len())
    {
        let a = lerp(*a, center, 0.10);
        let b = lerp(*b, center, 0.10);
        let along_x = (b.lon - a.lon) / super::DLON;
        let along_y = (b.lat - a.lat) / super::DLAT;
        let length = along_x.hypot(along_y).max(0.01);
        // Parallel wall offsets produce rectangular wings, not pointed wedges
        // aimed at the centre of every courtyard.
        let inset = |p: LatLon, depth: f64| {
            LatLon::new(
                p.lat + along_x / length * depth * super::DLAT,
                p.lon - along_y / length * depth * super::DLON,
            )
        };
        let divisions = if random.f() > 0.45 { 3 } else { 2 };
        for n in 0..divisions {
            if random.f() < 0.12 {
                continue;
            }
            let start = lerp(a, b, (f64::from(n) + 0.035) / f64::from(divisions));
            let end = lerp(a, b, (f64::from(n + 1) - 0.035) / f64::from(divisions));
            let depth = random.range(0.16, 0.36);
            let mut footprint = vec![start, end, inset(end, depth)];
            if random.f() > 0.55 {
                footprint.extend([
                    inset(lerp(start, end, 0.3), depth),
                    inset(lerp(start, end, 0.3), depth + 0.22),
                    inset(start, depth + 0.22),
                ]);
            } else {
                footprint.push(inset(start, depth));
            }
            out.push(Area {
                kind: AreaKind::Building,
                ring: footprint,
            });
        }
    }
    out
}

fn streets() -> Vec<Road> {
    let mut roads: Vec<_> = JUNCTIONS
        .iter()
        .map(|row| Road {
            class: RoadClass::Residential,
            pts: points(row),
        })
        .collect();
    for col in 0..9 {
        roads.push(Road {
            class: RoadClass::Residential,
            pts: JUNCTIONS
                .iter()
                .filter_map(|row| row.get(col).copied())
                .map(|(x, y)| cell(x, y))
                .collect(),
        });
    }
    // Old-town lanes, Sonnenstraße and the eastern riverbank/bridges.
    for coords in [
        &[
            (-5.2, 0.6),
            (-4.3, -0.6),
            (-2.45, -1.5),
            (-1.1, -2.4),
            (0.8, -3.8),
        ][..],
        &[(-2.45, -1.5), (-2.9, 0.8), (-2.5, 2.5), (-1.7, 5.2)],
        &[(-2.9, 0.8), (-1.4, 0.6), (0.0, 0.0), (1.4, 0.6), (3.0, 0.9)],
        &[
            (-2.5, 2.5),
            (-0.8, 1.6),
            (0.0, 0.0),
            (0.6, -1.9),
            (0.8, -3.8),
        ],
        &[
            (0.6, -1.9),
            (2.1, -0.4),
            (3.0, 0.9),
            (4.0, -1.5),
            (4.2, -3.4),
        ],
        &[(-1.4, 0.6), (-0.7, 1.3), (-0.8, 1.6)],
        &[(0.0, 0.0), (1.0, -0.5), (2.1, -0.4)],
        &[(0.6, 2.6), (1.7, 1.9), (3.0, 0.9)],
        &[(-2.7, -4.0), (-1.4, -2.7), (0.6, -1.9)],
        &[(0.8, -6.2), (1.7, -5.0), (3.7, -5.7)],
        &[(4.0, -1.5), (5.4, -0.8), (7.2, -0.4)],
        &[
            (3.3, -8.5),
            (3.7, -5.7),
            (4.2, -3.4),
            (4.0, -1.5),
            (3.0, 0.9),
            (3.4, 3.4),
        ],
        &[
            (6.3, -8.8),
            (6.8, -5.5),
            (7.0, -3.0),
            (7.2, -0.4),
            (7.4, 2.1),
            (8.0, 4.9),
        ],
    ] {
        roads.push(Road {
            class: RoadClass::Secondary,
            pts: points(coords),
        });
    }
    for pts in [corridor(), tram_path(), bus_path(), northern_path()] {
        roads.push(Road {
            class: RoadClass::Primary,
            pts,
        });
    }
    roads
}

fn labels() -> Vec<MapLabel> {
    use LabelKind::{Neighborhood, Park, Station, Street, Water};
    [
        ("Goetheplatz", -4.9, -3.2, Station("U")),
        ("Sendlinger Tor", -2.45, -1.5, Station("U")),
        ("Marienplatz", 0.0, 0.0, Station("U/S")),
        ("Lindwurmstraße", -5.4, -4.7, Street),
        ("Sendlinger Straße", -1.5, -0.8, Street),
        ("Sonnenstraße", -3.0, 1.5, Street),
        ("Isar", 8.0, -4.1, Water),
        ("SENDLING", -5.6, -8.8, Neighborhood),
        ("ALTSTADT", 2.7, 1.8, Neighborhood),
        ("Alter Südfriedhof", -2.2, -4.4, Park),
        ("Theresienwiese", -9.8, -2.6, Park),
        ("Hofgarten", 1.1, 4.8, Park),
        ("Fraunhoferstraße", 2.9, -4.3, Street),
        ("Gärtnerplatz", 2.3, -2.4, Street),
        ("Goethestraße", -7.0, 0.3, Street),
    ]
    .into_iter()
    .map(|(name, x, y, kind)| MapLabel {
        name,
        pos: cell(x, y),
        kind,
    })
    .collect()
}

pub fn basemap() -> Basemap {
    let mut areas = parks();
    let mut random = Rng(0x51ED_2026_C0FF_EE01);
    for rows in JUNCTIONS.windows(2) {
        let Some((south, north)) = rows.first().zip(rows.get(1)) else {
            continue;
        };
        for (bottom, top) in south.windows(2).zip(north.windows(2)) {
            let Some((&sw, &se, &nw, &ne)) = bottom
                .first()
                .zip(bottom.get(1))
                .zip(top.first().zip(top.get(1)))
                .map(|((a, b), (c, d))| (a, b, c, d))
            else {
                continue;
            };
            let ring = points(&[sw, se, ne, nw]);
            for building in buildings(&ring, &mut random) {
                // Reserve the actual green polygons and a continuous river channel.
                if building.ring.iter().any(|p| {
                    areas
                        .iter()
                        .filter(|a| a.kind == AreaKind::Park)
                        .any(|a| inside(*p, &a.ring))
                }) {
                    continue;
                }
                let near_water = building.ring.iter().any(|p| {
                    let y = (p.lat - super::CENTER.lat) / super::DLAT;
                    let x = (p.lon - super::CENTER.lon) / super::DLON;
                    (x - (8.6 + (y * 0.35).sin() * 0.9)).abs() < 0.7
                });
                if !near_water {
                    areas.push(building);
                }
            }
        }
    }
    let river = (-28..=28)
        .map(|n| {
            let y = f64::from(n) * 0.5;
            cell(8.6 + (y * 0.35).sin() * 0.9, y)
        })
        .collect();
    // Streets break at park boundaries instead of drawing a lattice through
    // the cemetery and fairground. Primary navigation stays continuous.
    let roads = streets()
        .into_iter()
        .flat_map(|road| {
            if road.class == RoadClass::Primary {
                return vec![road];
            }
            road.pts
                .windows(2)
                .filter_map(|segment| {
                    let (a, b) = (*segment.first()?, *segment.get(1)?);
                    let crosses_park = [0.25, 0.5, 0.75].into_iter().any(|t| {
                        areas
                            .iter()
                            .filter(|area| area.kind == AreaKind::Park)
                            .any(|area| inside(lerp(a, b, t), &area.ring))
                    });
                    (!crosses_park).then_some(Road {
                        class: road.class,
                        pts: vec![a, b],
                    })
                })
                .collect()
        })
        .collect();
    Basemap {
        areas,
        roads,
        river,
        labels: labels(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn courtyard_footprints_are_varied_and_respect_green_space() {
        let base = basemap();
        let repeat = basemap();
        let buildings: Vec<_> = base
            .areas
            .iter()
            .filter(|a| a.kind == AreaKind::Building)
            .collect();
        assert!(buildings.len() > 250);
        assert!(
            buildings.iter().any(|a| a.ring.len() == 6),
            "return wings make real courtyards"
        );
        assert!(
            buildings.iter().any(|a| a.ring.len() == 4),
            "simple frontages remain"
        );
        let widths: std::collections::BTreeSet<_> = buildings
            .iter()
            .map(|a| {
                let min = a.ring.iter().map(|p| p.lon).fold(f64::INFINITY, f64::min);
                let max = a
                    .ring
                    .iter()
                    .map(|p| p.lon)
                    .fold(f64::NEG_INFINITY, f64::max);
                ((max - min) * 1_000_000.0) as i64
            })
            .collect();
        assert!(widths.len() > 50, "not identical square stamps");
        for building in buildings {
            assert!(building.ring.iter().all(|p| {
                base.areas
                    .iter()
                    .filter(|a| a.kind == AreaKind::Park)
                    .all(|a| !inside(*p, &a.ring))
            }));
        }
        assert_eq!(base.areas.len(), repeat.areas.len());
        assert!(
            base.areas
                .iter()
                .zip(&repeat.areas)
                .all(|(a, b)| a.ring == b.ring)
        );
        for name in [
            "Lindwurmstraße",
            "Sendlinger Straße",
            "Sonnenstraße",
            "Isar",
            "SENDLING",
            "ALTSTADT",
            "Alter Südfriedhof",
            "Theresienwiese",
        ] {
            assert!(
                base.labels.iter().any(|label| label.name == name),
                "missing {name}"
            );
        }
    }
}
