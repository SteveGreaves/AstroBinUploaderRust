//! `GeocodeStep` — coordinate alignment, GPS-drift clustering, site lookup.
//!
//! Three stages, none of which changes the row count:
//!
//! 1. Calibration frames rarely carry GPS, so each inherits the coordinates of
//!    the nearest light frame (or the first one, when it has none at all).
//! 2. A mount reports slightly different coordinates every night. Readings
//!    within 110 m of each other are one physical site, and each cluster's
//!    **centroid** replaces the drifting readings — otherwise one back garden
//!    becomes a dozen "sites" in the report.
//! 3. The centroid is looked up in the `[sites]` database to 4 decimal places
//!    for a name, a Bortle class and an SQM reading.
//!
//! Two things about the output shape, both consequences of how the Python does
//! it rather than choices: `sitelat`/`sitelong` are **dropped and re-appended
//! at the end** of the frame (they come back from a merge with the centroid
//! table), and their values are the centroids, not what the header said.
//!
//! The clustering is greedy single-linkage and **order-dependent**: the first
//! unclaimed point seeds a cluster and claims every unclaimed point within the
//! radius, and a claimed point is never reassigned. A3 upstream: without that
//! last rule a later seed could steal points from an earlier cluster, leaving
//! it a single un-averaged reading.

use anyhow::{bail, Result};

use crate::appconfig::AppConfig;
use crate::constants as col;
use crate::constants::image_type::LIGHT;
use crate::numeric::{numpy_round, python_round};
use crate::steps::{astype_str, kahan_mean, to_numeric};
use crate::table::{Cell, Column, DType, Table};

/// Mean Earth radius in metres (IUGG).
const EARTH_RADIUS_M: f64 = 6371000.0;

/// GPS readings within this distance are the same physical site.
const CLUSTER_RADIUS_M: f64 = 110.0;

/// Great-circle distance in metres.
///
/// Rust's `sin`/`cos`/`asin` may differ from numpy's vectorised versions in
/// the last bit. That is tolerable *here specifically*: the result is only
/// ever compared against a 110 m threshold or minimised over, never stored or
/// printed, so a sub-nanometre difference cannot reach the output.
fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (lat1r, lon1r, lat2r, lon2r) = (
        lat1.to_radians(),
        lon1.to_radians(),
        lat2.to_radians(),
        lon2.to_radians(),
    );
    let dlat = lat2r - lat1r;
    let dlon = lon2r - lon1r;
    let a =
        (dlat / 2.0).sin().powi(2) + lat1r.cos() * lat2r.cos() * (dlon / 2.0).sin().powi(2);
    EARTH_RADIUS_M * 2.0 * a.sqrt().asin()
}

pub fn execute(table: &Table, cfg: &AppConfig) -> Result<Table> {
    let mut df = table.clone();
    if df.n_rows == 0 {
        return Ok(df);
    }

    align_coordinates(&mut df)?;

    // --- Stage 2: cluster ---------------------------------------------------
    // The coordinates are already hardened to non-null floats by stage 7, so
    // the `to_numeric(...).fillna(0.0)` here is defensive rather than
    // load-bearing; it is reproduced anyway because a 0.0 substituted for a
    // missing reading is a real (if unfortunate) coordinate that clusters.
    let lat: Vec<f64> = coords(&df, col::SITE_LAT)?;
    let lon: Vec<f64> = coords(&df, col::SITE_LONG)?;

    // `drop_duplicates()` on the coordinate pairs, first occurrence wins.
    let mut unique: Vec<(f64, f64)> = Vec::new();
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    for i in 0..df.n_rows {
        if seen.insert((lat[i].to_bits(), lon[i].to_bits())) {
            unique.push((lat[i], lon[i]));
        }
    }

    let mut cluster_of: Vec<i64> = vec![-1; unique.len()];
    let mut next_cluster = 0i64;
    for i in 0..unique.len() {
        if cluster_of[i] != -1 {
            continue;
        }
        cluster_of[i] = next_cluster;
        let (lat_ref, lon_ref) = unique[i];
        for j in 0..unique.len() {
            if cluster_of[j] == -1
                && haversine_m(unique[j].0, unique[j].1, lat_ref, lon_ref) < CLUSTER_RADIUS_M
            {
                cluster_of[j] = next_cluster;
            }
        }
        next_cluster += 1;
    }

    // Centroids, summed the way pandas' groupby does it: Kahan-compensated,
    // in row order.
    let mut centroid: Vec<(f64, f64)> = Vec::with_capacity(next_cluster as usize);
    for c in 0..next_cluster {
        let members: Vec<usize> = (0..unique.len()).filter(|&i| cluster_of[i] == c).collect();
        centroid.push((
            kahan_mean(members.iter().map(|&i| unique[i].0)),
            kahan_mean(members.iter().map(|&i| unique[i].1)),
        ));
    }

    // Map every row onto its cluster through the unique-pair table, exactly as
    // the merge on `(sitelat, sitelong)` does.
    let index: std::collections::HashMap<(u64, u64), usize> = unique
        .iter()
        .enumerate()
        .map(|(i, (a, b))| ((a.to_bits(), b.to_bits()), i))
        .collect();
    let row_cluster: Vec<i64> = (0..df.n_rows)
        .map(|i| cluster_of[index[&(lat[i].to_bits(), lon[i].to_bits())]])
        .collect();

    // --- Stage 3: site lookup ----------------------------------------------
    let default_site = default_str(cfg, "SITE", "Unknown Site");
    let default_bortle = default_int(cfg, "BORTLE", 4)?;
    let default_sqm = default_float(cfg, "SQM", 21.0)?;

    let mut site_of: Vec<(String, i64, f64)> = Vec::with_capacity(next_cluster as usize);
    for c in 0..next_cluster {
        let (avg_lat, avg_lon) = centroid[c as usize];
        site_of.push(match find_site_in_db(cfg, avg_lat, avg_lon)? {
            Some((name, bortle, sqm)) => (
                name,
                bortle.unwrap_or(default_bortle),
                sqm.unwrap_or(default_sqm),
            ),
            None => (default_site.clone(), default_bortle, default_sqm),
        });
    }

    for (name, cells) in [
        (
            col::SITE_NAME,
            (0..df.n_rows)
                .map(|i| Cell::Str(site_of[row_cluster[i] as usize].0.clone()))
                .collect::<Vec<_>>(),
        ),
        (
            col::BORTLE,
            (0..df.n_rows)
                .map(|i| Cell::Float(site_of[row_cluster[i] as usize].1 as f64))
                .collect(),
        ),
        (
            col::MEAN_SQM,
            (0..df.n_rows)
                .map(|i| Cell::Float(site_of[row_cluster[i] as usize].2))
                .collect(),
        ),
    ] {
        // `df.loc[mask, col] = value` writes into the existing column, so the
        // dtype it already has is what survives -- an int Bortle lands in a
        // float64 column as 4.0.
        if let Some(existing) = df.column(name) {
            let dtype = existing.dtype;
            df.set_column(
                name,
                Column {
                    name: name.to_string(),
                    dtype,
                    cells,
                },
            );
        }
    }

    // The two merges drop the original coordinate columns and re-append the
    // centroid ones, so they end up last.
    df.drop_column(col::SITE_LAT);
    df.drop_column(col::SITE_LONG);
    for (name, pick) in [(col::SITE_LAT, 0usize), (col::SITE_LONG, 1)] {
        df.set_column(
            name,
            Column {
                name: name.to_string(),
                dtype: DType::Float,
                cells: (0..df.n_rows)
                    .map(|i| {
                        let c = centroid[row_cluster[i] as usize];
                        Cell::Float(if pick == 0 { c.0 } else { c.1 })
                    })
                    .collect(),
            },
        );
    }

    Ok(df)
}

/// Stage 1 — give every calibration frame the coordinates of a light frame.
fn align_coordinates(df: &mut Table) -> Result<()> {
    let Some(itype) = df.column(col::IMAGE_TYPE) else {
        return Ok(());
    };
    let is_light: Vec<bool> = itype
        .cells
        .iter()
        .map(|c| matches!(c, Cell::Str(s) if s == LIGHT))
        .collect();
    let lights: Vec<usize> = (0..df.n_rows).filter(|&i| is_light[i]).collect();
    if lights.is_empty() {
        return Ok(());
    }

    let lat: Vec<Option<f64>> = optional_coords(df, col::SITE_LAT);
    let lon: Vec<Option<f64>> = optional_coords(df, col::SITE_LONG);

    let mut new_lat = lat.clone();
    let mut new_lon = lon.clone();
    for i in 0..df.n_rows {
        if is_light[i] {
            continue;
        }
        match (lat[i], lon[i]) {
            // No usable reading at all: take the first light frame's.
            (None, _) | (_, None) => {
                new_lat[i] = lat[lights[0]];
                new_lon[i] = lon[lights[0]];
            }
            (Some(plat), Some(plon)) => {
                // Nearest light frame by great-circle distance; ties go to the
                // earliest row, as `idxmin` does.
                let closest = lights
                    .iter()
                    .filter_map(|&l| match (lat[l], lon[l]) {
                        (Some(a), Some(b)) => Some((haversine_m(a, b, plat, plon), l)),
                        _ => None,
                    })
                    .fold(None::<(f64, usize)>, |best, (d, l)| match best {
                        Some((bd, _)) if bd <= d => best,
                        _ if d.is_nan() => best,
                        _ => Some((d, l)),
                    });
                // An all-NaN distance column makes `idxmin` raise, and the
                // broad `except` there leaves the row's coordinates as found.
                if let Some((_, l)) = closest {
                    new_lat[i] = lat[l];
                    new_lon[i] = lon[l];
                }
            }
        }
    }

    for (name, values) in [(col::SITE_LAT, &new_lat), (col::SITE_LONG, &new_lon)] {
        if let Some(existing) = df.column(name) {
            let dtype = existing.dtype;
            df.set_column(
                name,
                Column {
                    name: name.to_string(),
                    dtype,
                    cells: values
                        .iter()
                        .map(|v| match v {
                            Some(x) => Cell::Float(*x),
                            None => Cell::Null,
                        })
                        .collect(),
                },
            );
        }
    }
    Ok(())
}

fn optional_coords(df: &Table, name: &str) -> Vec<Option<f64>> {
    match df.column(name) {
        Some(c) => c.cells.iter().map(to_numeric).collect(),
        None => vec![None; df.n_rows],
    }
}

/// `to_numeric(...).fillna(0.0)`.
fn coords(df: &Table, name: &str) -> Result<Vec<f64>> {
    let Some(c) = df.column(name) else {
        bail!("geocoding needs a '{name}' column");
    };
    Ok(c.cells
        .iter()
        .map(|v| to_numeric(v).unwrap_or(0.0))
        .collect())
}

/// `_find_site_in_db` — a match to `precision` (4) decimal places.
///
/// Note the asymmetry, which is in the Python and is preserved here: the
/// database side is rounded with pandas' `.round()` and the query side with
/// the builtin `round()`. The two disagree at exact decimal boundaries, and
/// this is one equality with one of each on either side of it.
fn find_site_in_db(
    cfg: &AppConfig,
    lat: f64,
    lon: f64,
) -> Result<Option<(String, Option<i64>, Option<f64>)>> {
    let precision = cfg.precision;
    let want_lat = python_round(lat, precision);
    let want_lon = python_round(lon, precision);

    for (name, section) in &cfg.sites {
        let db_lat = section.get("latitude").and_then(|v| parse_db(&v.as_str()));
        let db_lon = section.get("longitude").and_then(|v| parse_db(&v.as_str()));
        let (Some(db_lat), Some(db_lon)) = (db_lat, db_lon) else {
            continue;
        };
        if numpy_round(db_lat, precision) == want_lat && numpy_round(db_lon, precision) == want_lon
        {
            let bortle = match section.get("bortle") {
                None => None,
                Some(v) => Some(v.as_str().trim().parse::<i64>().map_err(|_| {
                    anyhow::anyhow!("[sites] {name}: bortle = {:?} is not an integer", v.as_str())
                })?),
            };
            let sqm = match section.get("sqm") {
                None => None,
                Some(v) => Some(v.as_str().trim().parse::<f64>().map_err(|_| {
                    anyhow::anyhow!("[sites] {name}: sqm = {:?} is not a number", v.as_str())
                })?),
            };
            return Ok(Some((name.clone(), bortle, sqm)));
        }
    }
    Ok(None)
}

/// `pd.to_numeric(errors='coerce')` on a config string.
fn parse_db(text: &str) -> Option<f64> {
    crate::table::parse_float(text.trim())
}

fn default_str(cfg: &AppConfig, key: &str, fallback: &str) -> String {
    cfg.defaults
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| fallback.to_string())
}

fn default_int(cfg: &AppConfig, key: &str, fallback: i64) -> Result<i64> {
    match cfg.defaults.iter().find(|(k, _)| k == key) {
        None => Ok(fallback),
        Some((_, v)) => v.as_str().trim().parse::<i64>().map_err(|_| {
            anyhow::anyhow!("[defaults] {key} = {:?} is not an integer", v.as_str())
        }),
    }
}

fn default_float(cfg: &AppConfig, key: &str, fallback: f64) -> Result<f64> {
    match cfg.defaults.iter().find(|(k, _)| k == key) {
        None => Ok(fallback),
        Some((_, v)) => v
            .as_str()
            .trim()
            .parse::<f64>()
            .map_err(|_| anyhow::anyhow!("[defaults] {key} = {:?} is not a number", v.as_str())),
    }
}

/// Unused today but kept beside its sibling: `astype_str` is how every other
/// step reads a cell as text.
#[allow(dead_code)]
fn as_text(cell: &Cell) -> String {
    astype_str(cell)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigFile;

    fn cfg(text: &str) -> AppConfig {
        AppConfig::from_config(&ConfigFile::parse_str(text).unwrap()).unwrap()
    }

    #[test]
    fn readings_within_the_radius_become_one_site_at_their_centroid() {
        // ~0.0001 degrees of latitude is about 11 m: well inside the radius.
        let df = Table::parse_str(
            "imagetyp,sitelat,sitelong,site,bortle,sqm\n\
             LIGHT,52.2484,-0.1231,x,4.0,21.0\n\
             LIGHT,52.2486,-0.1231,x,4.0,21.0\n",
        )
        .unwrap();
        let out = execute(&df, &cfg("[defaults]\nSITE = Home\nBORTLE = 4\nSQM = 21\n")).unwrap();
        let lat = out.column("sitelat").unwrap();
        assert_eq!(lat.cells[0], lat.cells[1]);
        assert_eq!(lat.cells[0], Cell::Float(52.2485));
        // ...and the coordinate columns have moved to the end of the frame.
        let names: Vec<&str> = out.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(&names[names.len() - 2..], &["sitelat", "sitelong"]);
    }

    #[test]
    fn distant_readings_stay_separate_sites() {
        let df = Table::parse_str(
            "imagetyp,sitelat,sitelong,site,bortle,sqm\n\
             LIGHT,52.2484,-0.1231,x,4.0,21.0\n\
             LIGHT,40.7500,-111.8833,x,4.0,21.0\n",
        )
        .unwrap();
        let out = execute(&df, &cfg("[defaults]\nSITE = Home\nBORTLE = 4\nSQM = 21\n")).unwrap();
        let lat = out.column("sitelat").unwrap();
        assert_ne!(lat.cells[0], lat.cells[1]);
    }

    #[test]
    fn a_matching_site_supplies_its_name_bortle_and_sqm() {
        let config = cfg(
            "[defaults]\nSITE = Fallback\nBORTLE = 9\nSQM = 18\n\
             [sites]\n [[Papworth Everard]]\n  latitude = 52.2484\n  longitude = -0.1231\n  bortle = 4\n  sqm = 21\n",
        );
        let df = Table::parse_str(
            "imagetyp,sitelat,sitelong,site,bortle,sqm\n\
             LIGHT,52.2484,-0.1231,x,0.0,0.0\n",
        )
        .unwrap();
        let out = execute(&df, &config).unwrap();
        assert_eq!(
            out.column("site").unwrap().cells[0],
            Cell::Str("Papworth Everard".into())
        );
        assert_eq!(out.column("bortle").unwrap().cells[0], Cell::Float(4.0));
        assert_eq!(out.column("sqm").unwrap().cells[0], Cell::Float(21.0));
    }

    #[test]
    fn an_unmatched_cluster_falls_back_to_the_config_defaults() {
        let config = cfg("[defaults]\nSITE = Fallback\nBORTLE = 9\nSQM = 18\n");
        let df =
            Table::parse_str("imagetyp,sitelat,sitelong,site,bortle,sqm\nLIGHT,1.0,2.0,x,0.0,0.0\n")
                .unwrap();
        let out = execute(&df, &config).unwrap();
        assert_eq!(
            out.column("site").unwrap().cells[0],
            Cell::Str("Fallback".into())
        );
        assert_eq!(out.column("bortle").unwrap().cells[0], Cell::Float(9.0));
    }

    #[test]
    fn a_calibration_frame_without_gps_inherits_the_first_lights() {
        let df = Table::parse_str(
            "imagetyp,sitelat,sitelong,site,bortle,sqm\n\
             LIGHT,52.2484,-0.1231,x,4.0,21.0\n\
             DARK,,,x,4.0,21.0\n",
        )
        .unwrap();
        let out = execute(&df, &cfg("[defaults]\nSITE = Home\nBORTLE = 4\nSQM = 21\n")).unwrap();
        let lat = out.column("sitelat").unwrap();
        assert_eq!(lat.cells[0], lat.cells[1]);
    }
}
