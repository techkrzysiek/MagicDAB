//! Location referencing container (TPEG2-LRC, ISO 21219-7) with the methods
//! OpenLR (TPEG2-OLR, ISO 21219-22), TMC (TPEG2-TLR), extended TMC (ETL) and
//! geographic location referencing (GLR). Other methods are named only.

use super::binary::{Component, Reader};
use super::{Location, TmcLocation};

const TMC: u8 = 2;
const ETL: u8 = 5;
const GLR: u8 = 6;
const OLR: u8 = 8;

fn method_name(id: u8) -> String {
    match id {
        TMC => "TMC".into(),
        ETL => "ETL".into(),
        GLR => "GLR".into(),
        OLR => "OLR".into(),
        _ => format!("#{id}"),
    }
}

/// Decode a location referencing container; each method it holds is listed,
/// the geometry comes from the first OLR or GLR method, the TMC location
/// from a TMC or ETL method.
pub(crate) fn decode(lrc: &Component) -> Location {
    let mut loc = Location::default();
    for m in lrc.children() {
        loc.methods.push(method_name(m.id));
        match m.id {
            OLR if loc.points.is_empty() => {
                olr(&m, &mut loc);
                if !loc.points.is_empty() {
                    loc.method = method_name(m.id);
                }
            }
            GLR if loc.points.is_empty() => {
                if glr(&mut m.attributes(), &mut loc).is_some() && !loc.points.is_empty() {
                    loc.method = method_name(m.id);
                }
            }
            TMC if loc.tmc.is_none() => loc.tmc = tmc(&mut m.attributes(), false),
            ETL if loc.tmc.is_none() => loc.tmc = etl(&mut m.attributes()),
            _ => {}
        }
    }
    if loc.method.is_empty() {
        loc.method = loc.methods.first().cloned().unwrap_or_default();
    }
    loc
}

/// IntSi24 in 360 / 2^24 degree units, rounded to five decimals as TPEG does.
fn degrees24(v: i32) -> f64 {
    let v = v as f64;
    round5((v - 0.5f64.copysign(v)) * 360.0 / (1u32 << 24) as f64)
}

fn round5(x: f64) -> f64 {
    (x * 1e5).round() / 1e5
}

/// OLR AbsoluteGeoCoordinate: longitude, latitude, optional altitude; [lat, lon].
fn absolute(r: &mut Reader) -> Option<[f64; 2]> {
    let lon = r.i24()?;
    let lat = r.i24()?;
    if r.selector()?.has(0) {
        r.mb_signed()?;
    }
    Some([degrees24(lat), degrees24(lon)])
}

/// OLR RelativeGeoCoordinate: offsets from the previous point in 10 micro
/// degree units; [lat, lon].
fn relative(r: &mut Reader) -> Option<[f64; 2]> {
    let dlon = r.i16()? as f64;
    let dlat = r.i16()? as f64;
    if r.selector()?.has(0) {
        r.mb_signed()?;
    }
    Some([dlat * 1e-5, dlon * 1e-5])
}

fn add(p: [f64; 2], d: [f64; 2]) -> [f64; 2] {
    [round5(p[0] + d[0]), round5(p[1] + d[1])]
}

/// Line properties (component 9) and, for all but the last point, path
/// properties (component 10); returns the distance to the next point.
fn properties(r: &mut Reader, path: bool) -> Option<Option<u32>> {
    let mut dnp = None;
    let n = if path { 2 } else { 1 };
    for _ in 0..n {
        let c = r.component()?;
        if c.id == 10 {
            let mut a = c.attributes();
            a.u8()?;
            dnp = Some(a.mb()?);
        }
    }
    Some(dnp)
}

/// First location reference point, the last one as an offset from the point
/// before it, and the first point's distance to the next point along the road.
fn first_and_last(r: &mut Reader) -> Option<([f64; 2], [f64; 2], Option<u32>)> {
    let first = absolute(r)?;
    let dnp = properties(r, true)?;
    let last = relative(r)?;
    properties(r, false)?;
    Some((first, last, dnp))
}

/// A point `offset` metres from `a` towards `b`, `dnp` metres away along the road.
fn along(a: [f64; 2], b: [f64; 2], offset: u32, dnp: Option<u32>) -> [f64; 2] {
    let f = match dnp {
        Some(d) if d > 0 => (offset as f64 / d as f64).min(1.0),
        _ => 0.0,
    };
    [round5(a[0] + (b[0] - a[0]) * f), round5(a[1] + (b[1] - a[1]) * f)]
}

fn olr(m: &Component, loc: &mut Location) {
    for c in m.children() {
        let mut r = c.attributes();
        let ok = match c.id {
            0 => line(&mut r, loc),
            1 => absolute(&mut r).map(|p| {
                loc.kind = Some("point");
                loc.point = Some(p);
                loc.points = vec![p];
            }),
            2 | 3 => point_along_line(&mut r, loc, c.id == 3),
            4 => circle(&mut r, loc),
            5 => polygon(&mut r, loc),
            6 | 7 => {
                // rectangle, grid: lower left and upper right corner first
                let kind = if c.id == 6 { "rectangle" } else { "grid" };
                absolute(&mut r).zip(absolute(&mut r)).map(|(a, b)| {
                    loc.kind = Some(kind);
                    loc.points = vec![a, b];
                })
            }
            8 => absolute(&mut r).map(|p| {
                loc.kind = Some("closedLine");
                loc.points = vec![p];
            }),
            11 => description(&mut r, loc),
            _ => Some(()),
        };
        if ok.is_some() && c.id <= 8 {
            // shape and description of a location reference, if sent
            for s in c.children() {
                let mut a = s.attributes();
                match s.id {
                    12 => {
                        let n = a.mb().unwrap_or(0);
                        loc.shape = (0..n).map_while(|_| absolute(&mut a)).collect();
                    }
                    11 => {
                        description(&mut a, loc);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Linear location: first, last, then optional intermediates and offsets.
/// Every point is relative to the one before it on the line, the last one
/// to the last intermediate point although it is sent before them.
fn line(r: &mut Reader, loc: &mut Location) -> Option<()> {
    let (first, last, _) = first_and_last(r)?;
    loc.kind = Some("line");
    loc.points = vec![first, add(first, last)];
    let s = r.selector()?;
    if s.has(0) {
        let n = r.mb()?;
        let mut prev = first;
        let mut points = vec![first];
        for _ in 0..n {
            prev = add(prev, relative(r)?);
            properties(r, true)?;
            points.push(prev);
        }
        points.push(add(prev, last));
        loc.points = points;
    }
    if s.has(1) {
        loc.positive_offset = Some(r.mb()?);
    }
    if s.has(2) {
        loc.negative_offset = Some(r.mb()?);
    }
    Some(())
}

/// Point along line (and POI with access point): the line, side of road,
/// orientation and the point's offset from the first point.
fn point_along_line(r: &mut Reader, loc: &mut Location, poi: bool) -> Option<()> {
    let (first, last, dnp) = first_and_last(r)?;
    let last = add(first, last);
    loc.kind = Some(if poi { "poiWithAccessPoint" } else { "pointAlongLine" });
    loc.points = vec![first, last];
    loc.point = Some(first);
    loc.side_of_road = Some(r.u8()?);
    loc.orientation = Some(r.u8()?);
    if r.selector()?.has(0) {
        let offset = r.mb()?;
        loc.positive_offset = Some(offset);
        loc.point = Some(along(first, last, offset, dnp));
    }
    Some(())
}

fn circle(r: &mut Reader, loc: &mut Location) -> Option<()> {
    let c = absolute(r)?;
    loc.kind = Some("circle");
    loc.point = Some(c);
    loc.points = vec![c];
    loc.radius = Some(r.mb()?);
    Some(())
}

fn polygon(r: &mut Reader, loc: &mut Location) -> Option<()> {
    let start = absolute(r)?;
    loc.kind = Some("polygon");
    loc.points = vec![start];
    let n = r.mb()?;
    let mut prev = start;
    for _ in 0..n {
        prev = add(prev, relative(r)?);
        loc.points.push(prev);
    }
    Some(())
}

fn description(r: &mut Reader, loc: &mut Location) -> Option<()> {
    let n = r.mb()?;
    for _ in 0..n {
        loc.text.push(r.localised_long_string()?);
    }
    Some(())
}

/// TMC location (TPEG2-TLR); `etl` adds the internal location flags.
fn tmc(r: &mut Reader, etl: bool) -> Option<TmcLocation> {
    let location = r.u16()?;
    let country = r.u8()?;
    let table = r.u8()?;
    let s = r.selector()?;
    let mut t = TmcLocation { location, country, table, positive: s.has(0), both_directions: s.has(1), ..Default::default() };
    // the rest is optional: keep the location if it is cut short
    let _ = (|| {
        if s.has(2) {
            t.extent = r.u8()?;
        }
        if s.has(3) {
            t.ecc = Some(r.u8()?);
        }
        if s.has(4) {
            // one byte: 4 bits major, 3 bits minor; two: 7 bits each
            let v = r.mb()?;
            t.table_version = Some(if v > 127 { format!("{}.{}", (v >> 7) & 0x7F, v & 0x7F) } else { format!("{}.{}", v >> 3, v & 7) });
        }
        if s.has(5) {
            let p = r.selector()?;
            if p.has(0) {
                r.u8()?;
            }
            if p.has(1) {
                t.distance = Some(r.u8()? as u32 * 100);
            }
            if p.has(2) {
                t.distance = Some(r.u16()? as u32 * 100);
            }
            if p.has(3) {
                t.length = Some(r.u8()? as u32 * 100);
            }
            if p.has(4) {
                t.length = Some(r.u16()? as u32 * 100);
            }
        }
        Some(())
    })();
    if etl {
        t.internal_primary = s.has(6);
        t.internal_secondary = s.has(7);
    }
    Some(t)
}

/// Extended TMC location: version, then a TMC location or an exit/entry
/// location that starts with one.
fn etl(r: &mut Reader) -> Option<TmcLocation> {
    r.u8()?;
    let s = r.selector()?;
    if s.has(0) || s.has(1) {
        tmc(r, true)
    } else {
        None
    }
}

/// GLR WGS84 coordinate (no altitude); [lat, lon].
fn wgs84(r: &mut Reader) -> Option<[f64; 2]> {
    let lon = r.i24()?;
    let lat = r.i24()?;
    Some([degrees24(lat), degrees24(lon)])
}

fn wgs84_list(r: &mut Reader) -> Option<Vec<[f64; 2]>> {
    let n = r.mb()?;
    (0..n).map(|_| wgs84(r)).collect()
}

/// Geographic location reference: the first of its geometries.
fn glr(r: &mut Reader, loc: &mut Location) -> Option<()> {
    let s = r.selector()?;
    if s.has(0) {
        loc.kind = Some("boundingBox");
        loc.points = vec![wgs84(r)?, wgs84(r)?];
    } else if s.has(1) {
        let c = wgs84(r)?;
        loc.kind = Some("circle");
        loc.point = Some(c);
        loc.points = vec![c];
        loc.radius = Some(r.mb()?);
    } else if s.has(2) {
        let p = wgs84(r)?;
        loc.kind = Some("point");
        loc.point = Some(p);
        loc.points = vec![p];
    } else if s.has(3) {
        loc.kind = Some("line");
        loc.points = wgs84_list(r)?;
    } else if s.has(4) || s.has(5) {
        loc.kind = Some("area");
        loc.points = wgs84_list(r)?;
    }
    Some(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::binary::build::{component, coord24, mb, selector};
    use super::*;

    /// An OLR AbsoluteGeoCoordinate.
    fn abs(lat: f64, lon: f64) -> Vec<u8> {
        let mut v = coord24(lon).to_vec();
        v.extend(coord24(lat));
        v.push(0);
        v
    }

    /// An OLR RelativeGeoCoordinate (10 micro degree steps).
    fn rel(dlat: i16, dlon: i16) -> Vec<u8> {
        let mut v = dlon.to_be_bytes().to_vec();
        v.extend(dlat.to_be_bytes());
        v.push(0);
        v
    }

    fn line_props() -> Vec<u8> {
        component(9, &[2, 3, 0x40, 0], &[])
    }

    fn path_props(dnp: u32) -> Vec<u8> {
        let mut a = vec![3];
        a.extend(mb(dnp));
        a.push(0);
        component(10, &a, &[])
    }

    /// An OLR line location from (50.0, 20.0) to (50.001, 20.002), with offsets.
    pub fn olr_line() -> Vec<u8> {
        let mut a = abs(50.0, 20.0);
        a.extend(line_props());
        a.extend(path_props(300));
        a.extend(rel(100, 200));
        a.extend(line_props());
        a.extend(selector(&[1, 2]));
        a.extend(mb(150));
        a.extend(mb(20));
        let shape = [vec![2], abs(50.0, 20.0), abs(50.001, 20.002)].concat();
        let loc = component(0, &a, &[&component(12, &shape, &[])]);
        component(OLR, &[0x10], &[&loc])
    }

    pub fn tmc_location() -> Vec<u8> {
        let mut a = 12345u16.to_be_bytes().to_vec();
        a.extend([13, 1]);
        a.extend(selector(&[0, 2, 3]));
        a.extend([4, 0xE0]);
        component(TMC, &a, &[])
    }

    /// Equal within the 24 bit coordinate resolution.
    pub fn near(a: &[[f64; 2]], b: &[[f64; 2]]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(p, q)| (p[0] - q[0]).abs() < 3e-5 && (p[1] - q[1]).abs() < 3e-5)
    }

    fn decode_bytes(lrc: &[u8]) -> Location {
        decode(&Reader::new(lrc).component().unwrap())
    }

    #[test]
    fn olr_line_with_tmc() {
        let lrc = component(1, &[], &[&olr_line(), &tmc_location()]);
        let loc = decode_bytes(&lrc);
        assert_eq!(loc.method, "OLR");
        assert_eq!(loc.methods, ["OLR", "TMC"]);
        assert_eq!(loc.kind, Some("line"));
        assert!(near(&loc.points, &[[50.0, 20.0], [50.001, 20.002]]), "{:?}", loc.points);
        assert!(near(&loc.shape, &[[50.0, 20.0], [50.001, 20.002]]), "{:?}", loc.shape);
        assert_eq!((loc.positive_offset, loc.negative_offset), (Some(150), Some(20)));
        let t = loc.tmc.unwrap();
        assert_eq!((t.location, t.country, t.table, t.ecc), (12345, 13, 1, Some(0xE0)));
        assert!(t.positive && !t.both_directions);
        assert_eq!(t.extent, 4);
    }

    #[test]
    fn olr_line_with_intermediate_points() {
        let mut a = abs(50.0, 20.0);
        a.extend(line_props());
        a.extend(path_props(300));
        a.extend(rel(300, 0));
        a.extend(line_props());
        a.extend(selector(&[0]));
        a.extend(mb(2));
        a.extend(rel(100, 100));
        a.extend(line_props());
        a.extend(path_props(200));
        a.extend(rel(-50, 200));
        a.extend(line_props());
        a.extend(path_props(200));
        let lrc = component(1, &[], &[&component(OLR, &[0x10], &[&component(0, &a, &[])])]);
        let loc = decode_bytes(&lrc);
        let want = [[50.0, 20.0], [50.001, 20.001], [50.0005, 20.003], [50.0035, 20.003]];
        assert!(near(&loc.points, &want), "{:?}", loc.points);
    }

    #[test]
    fn olr_point_along_line() {
        let mut a = abs(50.0, 20.0);
        a.extend(line_props());
        a.extend(path_props(400));
        a.extend(rel(0, 400));
        a.extend(line_props());
        a.extend([1, 2]);
        a.extend(selector(&[0]));
        a.extend(mb(100));
        let lrc = component(1, &[], &[&component(OLR, &[0x10], &[&component(2, &a, &[])])]);
        let loc = decode_bytes(&lrc);
        assert_eq!(loc.kind, Some("pointAlongLine"));
        assert_eq!((loc.side_of_road, loc.orientation, loc.positive_offset), (Some(1), Some(2), Some(100)));
        assert!(near(&[loc.point.unwrap()], &[[50.0, 20.001]]), "{:?}", loc.point);
    }

    #[test]
    fn olr_geo_coordinate_and_negative_coordinates() {
        let lrc = component(1, &[], &[&component(OLR, &[0x10], &[&component(1, &abs(-33.5, -70.25), &[])])]);
        let loc = decode_bytes(&lrc);
        assert_eq!(loc.kind, Some("point"));
        assert!(near(&[loc.point.unwrap()], &[[-33.5, -70.25]]), "{:?}", loc.point);
    }

    #[test]
    fn glr_point_and_unknown_methods() {
        let mut a = selector(&[2]);
        a.extend(coord24(19.5));
        a.extend(coord24(49.25));
        a.push(0);
        let lrc = component(1, &[], &[&component(3, &[1, 2], &[]), &component(GLR, &a, &[])]);
        let loc = decode_bytes(&lrc);
        assert_eq!(loc.methods, ["#3", "GLR"]);
        assert_eq!(loc.method, "GLR");
        assert!(near(&[loc.point.unwrap()], &[[49.25, 19.5]]), "{:?}", loc.point);
        // only a method this decoder does not know
        let loc = decode_bytes(&component(1, &[], &[&component(9, &[1], &[])]));
        assert_eq!(loc.method, "#9");
        assert!(loc.points.is_empty());
    }

    #[test]
    fn etl_location() {
        let mut a = vec![0x10];
        a.extend(selector(&[0]));
        a.extend(500u16.to_be_bytes());
        a.extend([13, 1]);
        a.extend(selector(&[1, 6]));
        let loc = decode_bytes(&component(1, &[], &[&component(ETL, &a, &[])]));
        let t = loc.tmc.unwrap();
        assert_eq!(t.location, 500);
        assert!(t.both_directions && t.internal_primary && !t.internal_secondary && !t.positive);
    }

    #[test]
    fn truncated_locations_do_not_panic() {
        let lrc = component(1, &[], &[&olr_line(), &tmc_location()]);
        for n in 0..lrc.len() {
            if let Some(c) = Reader::new(&lrc[..n]).component() {
                decode(&c);
            }
        }
        // a cut attribute block inside a well formed component
        let a = abs(50.0, 20.0);
        decode_bytes(&component(1, &[], &[&component(OLR, &[0x10], &[&component(0, &a[..4], &[])])]));
    }
}
