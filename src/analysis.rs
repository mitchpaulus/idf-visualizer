//! Cross-surface and model-level geometry checks, run after `model::build`.

use crate::model::{plane_basis, Model, Problem, ProblemKind, Severity, Surface, SurfaceType};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

pub fn analyze(model: &mut Model, zone_volumes: &HashMap<String, f64>) {
    let mut found: Vec<(usize, Problem)> = Vec::new();
    subsurface_checks(&model.surfaces, &mut found);
    interzone_checks(&model.surfaces, &mut found);
    duplicate_and_overlap_checks(&model.surfaces, &mut found);
    zone_closure_checks(model, zone_volumes, &mut found);
    outlier_checks(&model.surfaces, &mut found);
    for (i, p) in found {
        model.surfaces[i].problems.push(p);
    }
}

// --- sub-surfaces vs their base ---------------------------------------------

fn subsurface_checks(surfaces: &[Surface], out: &mut Vec<(usize, Problem)>) {
    let mut by_base: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, s) in surfaces.iter().enumerate() {
        let Some(bi) = s.base_surface else { continue };
        by_base.entry(bi).or_default().push(i);
        let base = &surfaces[bi];
        if base.area < 1e-9 || s.area < 1e-9 {
            continue;
        }
        let n = base.normal.as_dvec3();
        let c = base.centroid.as_dvec3();

        let align = s.normal.dot(base.normal) as f64;
        if align < -0.98 {
            out.push((
                i,
                Problem::new(
                    ProblemKind::SubSurfaceOffBase,
                    format!("winding is reversed relative to base \"{}\"", base.name),
                ),
            ));
        } else if align < 0.98 {
            out.push((
                i,
                Problem::new(
                    ProblemKind::SubSurfaceOffBase,
                    format!(
                        "not parallel to base \"{}\" ({:.0}° apart)",
                        base.name,
                        align.clamp(-1.0, 1.0).acos().to_degrees()
                    ),
                ),
            ));
        }

        // Sub-surfaces are nudged 0.015 m along their own normal at build time.
        let expected = 0.015 * align;
        let max_off = s
            .verts
            .iter()
            .map(|v| ((v.as_dvec3() - c).dot(n) - expected).abs())
            .fold(0.0f64, f64::max);
        if max_off > 0.05 {
            out.push((
                i,
                Problem::new(
                    ProblemKind::SubSurfaceOffBase,
                    format!("up to {max_off:.3} m off the plane of base \"{}\"", base.name),
                ),
            ));
        }

        let (u, v) = plane_basis(n);
        let bpoly: Vec<(f64, f64)> = base
            .verts
            .iter()
            .map(|p| project(c, u, v, p.as_dvec3()))
            .collect();
        let outside = s
            .verts
            .iter()
            .filter(|p| {
                let q = project(c, u, v, p.as_dvec3());
                !point_in_polygon(&bpoly, q) && dist_to_polygon_edge(&bpoly, q) > 0.02
            })
            .count();
        if outside > 0 {
            out.push((
                i,
                Problem::new(
                    ProblemKind::SubSurfaceOffBase,
                    format!("{outside} vertex(es) outside base \"{}\"", base.name),
                ),
            ));
        }

        if s.area >= base.area {
            out.push((
                i,
                Problem::new(
                    ProblemKind::SubSurfaceTooBig,
                    format!(
                        "area {:.2} m² ≥ base \"{}\" area {:.2} m²",
                        s.area, base.name, base.area
                    ),
                ),
            ));
        }
    }

    for (&bi, subs) in &by_base {
        let base = &surfaces[bi];
        if base.area > 1e-9 {
            let total: f64 = subs.iter().map(|&i| surfaces[i].area).sum();
            if total > base.area {
                out.push((
                    bi,
                    Problem::with_severity(
                        ProblemKind::SubSurfaceTooBig,
                        Severity::Warning,
                        format!(
                            "sub-surfaces total {total:.2} m² > surface area {:.2} m²",
                            base.area
                        ),
                    ),
                ));
            }
        }

        // Sibling overlap, tested in the base plane.
        let (u, v) = plane_basis(base.normal.as_dvec3());
        let c = base.centroid.as_dvec3();
        let polys: Vec<Vec<(f64, f64)>> = subs
            .iter()
            .map(|&i| {
                surfaces[i]
                    .verts
                    .iter()
                    .map(|p| project(c, u, v, p.as_dvec3()))
                    .collect()
            })
            .collect();
        for a in 0..subs.len() {
            for b in (a + 1)..subs.len() {
                if polys_overlap(&polys[a], &polys[b], 0.005) {
                    for (x, y) in [(subs[a], subs[b]), (subs[b], subs[a])] {
                        out.push((
                            x,
                            Problem::new(
                                ProblemKind::SubSurfaceOverlap,
                                format!("overlaps \"{}\" on the same base", surfaces[y].name),
                            ),
                        ));
                    }
                }
            }
        }
    }
}

// --- interzone (Outside Boundary Condition = Surface) pairs ------------------

fn interzone_checks(surfaces: &[Surface], out: &mut Vec<(usize, Problem)>) {
    let idx: HashMap<String, usize> = surfaces
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.to_ascii_uppercase(), i))
        .collect();
    for (i, s) in surfaces.iter().enumerate() {
        if !s.boundary.eq_ignore_ascii_case("Surface") {
            continue;
        }
        if s.boundary_object.is_empty() {
            out.push((
                i,
                Problem::new(
                    ProblemKind::InterzoneMismatch,
                    "boundary condition is Surface but no boundary object is named".to_string(),
                ),
            ));
            continue;
        }
        if s.boundary_object.eq_ignore_ascii_case(&s.name) {
            continue; // self-adjacency is legal
        }
        let Some(&j) = idx.get(&s.boundary_object.to_ascii_uppercase()) else {
            out.push((
                i,
                Problem::new(
                    ProblemKind::InterzoneMismatch,
                    format!("boundary object \"{}\" not found", s.boundary_object),
                ),
            ));
            continue;
        };
        let o = &surfaces[j];
        if o.boundary.eq_ignore_ascii_case("Surface")
            && !o.boundary_object.eq_ignore_ascii_case(&s.name)
        {
            out.push((
                i,
                Problem::with_severity(
                    ProblemKind::InterzoneMismatch,
                    Severity::Warning,
                    format!(
                        "\"{}\" points back to \"{}\", not this surface",
                        o.name, o.boundary_object
                    ),
                ),
            ));
        }
        if (s.area - o.area).abs() > 0.02 * s.area.max(o.area) + 0.01 {
            out.push((
                i,
                Problem::new(
                    ProblemKind::InterzoneMismatch,
                    format!("area {:.2} m² vs \"{}\" {:.2} m²", s.area, o.name, o.area),
                ),
            ));
        }
        let cdist = (s.centroid - o.centroid).length();
        if cdist > 0.1 {
            out.push((
                i,
                Problem::new(
                    ProblemKind::InterzoneMismatch,
                    format!("centroid is {cdist:.2} m from \"{}\"", o.name),
                ),
            ));
        }
        if s.normal.dot(o.normal) > -0.98 {
            out.push((
                i,
                Problem::new(
                    ProblemKind::InterzoneMismatch,
                    format!("normal is not opposite \"{}\"", o.name),
                ),
            ));
        }
    }
}

// --- duplicates and coplanar overlaps within a zone --------------------------

fn duplicate_and_overlap_checks(surfaces: &[Surface], out: &mut Vec<(usize, Problem)>) {
    let quant = |s: &Surface| -> Vec<[i64; 3]> {
        let mut k: Vec<[i64; 3]> = s
            .verts
            .iter()
            .map(|v| {
                [
                    (v.x as f64 * 1000.0).round() as i64,
                    (v.y as f64 * 1000.0).round() as i64,
                    (v.z as f64 * 1000.0).round() as i64,
                ]
            })
            .collect();
        k.sort_unstable();
        k
    };

    let mut groups: HashMap<(String, Vec<[i64; 3]>), Vec<usize>> = HashMap::new();
    for (i, s) in surfaces.iter().enumerate() {
        if s.verts.is_empty() {
            continue;
        }
        groups
            .entry((s.zone.to_ascii_uppercase(), quant(s)))
            .or_default()
            .push(i);
    }
    let mut dup_pairs: HashSet<(usize, usize)> = HashSet::new();
    for g in groups.values().filter(|g| g.len() > 1) {
        for &i in g {
            let other = g.iter().find(|&&j| j != i).copied().unwrap();
            let extra = g.len() - 2;
            let mut msg = format!("identical geometry to \"{}\"", surfaces[other].name);
            if extra > 0 {
                msg.push_str(&format!(" and {extra} more"));
            }
            out.push((i, Problem::new(ProblemKind::DuplicateSurface, msg)));
            for &j in g {
                dup_pairs.insert((i.min(j), i.max(j)));
            }
        }
    }

    // Coplanar overlapping base surfaces in the same zone.
    let mut by_zone: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, s) in surfaces.iter().enumerate() {
        if s.base_surface.is_none()
            && s.stype != SurfaceType::Shading
            && !s.zone.is_empty()
            && s.area > 1e-9
        {
            by_zone.entry(s.zone.to_ascii_uppercase()).or_default().push(i);
        }
    }
    for idxs in by_zone.values() {
        for a in 0..idxs.len() {
            for b in (a + 1)..idxs.len() {
                let (i, j) = (idxs[a], idxs[b]);
                if dup_pairs.contains(&(i.min(j), i.max(j))) {
                    continue;
                }
                let (s1, s2) = (&surfaces[i], &surfaces[j]);
                if s1.normal.dot(s2.normal).abs() < 0.999 {
                    continue;
                }
                let n = s1.normal.as_dvec3();
                if ((s2.centroid - s1.centroid).as_dvec3().dot(n)).abs() > 0.01 {
                    continue;
                }
                let (u, v) = plane_basis(n);
                let c = s1.centroid.as_dvec3();
                let to2d = |s: &Surface| -> Vec<(f64, f64)> {
                    s.verts.iter().map(|p| project(c, u, v, p.as_dvec3())).collect()
                };
                if polys_overlap(&to2d(s1), &to2d(s2), 0.01) {
                    for (x, y) in [(i, j), (j, i)] {
                        out.push((
                            x,
                            Problem::new(
                                ProblemKind::CoplanarOverlap,
                                format!("overlaps \"{}\" in the same plane", surfaces[y].name),
                            ),
                        ));
                    }
                }
            }
        }
    }
}

// --- zone closure and volume --------------------------------------------------

fn zone_closure_checks(
    model: &mut Model,
    zone_volumes: &HashMap<String, f64>,
    out: &mut Vec<(usize, Problem)>,
) {
    let surfaces = &model.surfaces;
    let mut by_zone: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, s) in surfaces.iter().enumerate() {
        if s.base_surface.is_none()
            && matches!(
                s.stype,
                SurfaceType::Wall | SurfaceType::Floor | SurfaceType::Ceiling | SurfaceType::Roof
            )
            && !s.zone.is_empty()
        {
            by_zone.entry(s.zone.to_ascii_uppercase()).or_default().push(i);
        }
    }

    let key = |v: DVec3| -> [i64; 3] {
        [
            (v.x * 100.0).round() as i64,
            (v.y * 100.0).round() as i64,
            (v.z * 100.0).round() as i64,
        ]
    };

    let mut new_warnings = Vec::new();
    for (zone, idxs) in &by_zone {
        // Skip obviously partial zones (a closure check needs an enclosure attempt).
        if idxs.len() < 4 {
            continue;
        }

        // Vertex pool for T-junction splitting.
        let mut pool: HashMap<[i64; 3], DVec3> = HashMap::new();
        for &i in idxs {
            for v in &surfaces[i].verts {
                pool.entry(key(v.as_dvec3())).or_insert(v.as_dvec3());
            }
        }
        let pool: Vec<DVec3> = pool.into_values().collect();

        // Directed sub-edges: in a closed, consistently wound shell every
        // sub-edge has a reversed twin on a neighboring surface.
        let mut count: HashMap<([i64; 3], [i64; 3]), i32> = HashMap::new();
        let mut sub_edges: Vec<(usize, [i64; 3], [i64; 3])> = Vec::new();
        for &i in idxs {
            let vs = &surfaces[i].verts;
            for e in 0..vs.len() {
                let a = vs[e].as_dvec3();
                let b = vs[(e + 1) % vs.len()].as_dvec3();
                let len = (b - a).length();
                if len < 0.02 {
                    continue;
                }
                let dir = (b - a) / len;
                let mut cuts: Vec<(f64, [i64; 3])> = pool
                    .iter()
                    .filter_map(|p| {
                        let t = (*p - a).dot(dir);
                        if t < 0.03 || t > len - 0.03 {
                            return None;
                        }
                        let perp = ((*p - a) - dir * t).length();
                        (perp < 0.01).then(|| (t, key(*p)))
                    })
                    .collect();
                cuts.sort_by(|x, y| x.0.total_cmp(&y.0));
                let mut chain = vec![key(a)];
                chain.extend(cuts.into_iter().map(|(_, k)| k));
                chain.push(key(b));
                for w in chain.windows(2) {
                    if w[0] == w[1] {
                        continue;
                    }
                    *count.entry((w[0], w[1])).or_insert(0) += 1;
                    sub_edges.push((i, w[0], w[1]));
                }
            }
        }

        let mut unmatched: HashMap<usize, usize> = HashMap::new();
        for (i, a, b) in &sub_edges {
            if count.get(&(*b, *a)).copied().unwrap_or(0) == 0 {
                *unmatched.entry(*i).or_insert(0) += 1;
            }
        }

        if unmatched.is_empty() {
            if let Some(&spec) = zone_volumes.get(zone) {
                let vol = zone_volume(surfaces, idxs);
                if (vol - spec).abs() > 0.1 * spec.max(1.0) {
                    new_warnings.push(format!(
                        "Zone \"{}\": computed volume {vol:.1} m³ differs from the Zone object's {spec:.1} m³",
                        surfaces[idxs[0]].zone
                    ));
                }
            }
        } else {
            for (&i, &n) in &unmatched {
                out.push((
                    i,
                    Problem::new(
                        ProblemKind::ZoneNotClosed,
                        format!(
                            "{n} edge(s) not shared with another surface of zone \"{}\" (gap or vertex mismatch)",
                            surfaces[i].zone
                        ),
                    ),
                ));
            }
        }
    }
    model.warnings.extend(new_warnings);
}

/// Enclosed volume of a zone shell via the divergence theorem, assuming the
/// stored outward normals are correct.
fn zone_volume(surfaces: &[Surface], idxs: &[usize]) -> f64 {
    let mut vol = 0.0;
    for &i in idxs {
        let s = &surfaces[i];
        let n = s.normal.as_dvec3();
        for t in s.tris.chunks_exact(3) {
            let a = s.verts[t[0] as usize].as_dvec3();
            let mut b = s.verts[t[1] as usize].as_dvec3();
            let mut c = s.verts[t[2] as usize].as_dvec3();
            if (b - a).cross(c - a).dot(n) < 0.0 {
                std::mem::swap(&mut b, &mut c);
            }
            vol += a.dot(b.cross(c)) / 6.0;
        }
    }
    vol
}

// --- outliers -----------------------------------------------------------------

fn outlier_checks(surfaces: &[Surface], out: &mut Vec<(usize, Problem)>) {
    if surfaces.len() < 8 {
        return;
    }
    let med = |mut v: Vec<f32>| -> f32 {
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    let center = glam::Vec3::new(
        med(surfaces.iter().map(|s| s.centroid.x).collect()),
        med(surfaces.iter().map(|s| s.centroid.y).collect()),
        med(surfaces.iter().map(|s| s.centroid.z).collect()),
    );
    let dists: Vec<f32> = surfaces
        .iter()
        .map(|s| (s.centroid - center).length())
        .collect();
    let threshold = (8.0 * med(dists.clone())).max(50.0);
    for (i, &d) in dists.iter().enumerate() {
        if d > threshold {
            out.push((
                i,
                Problem::new(
                    ProblemKind::Outlier,
                    format!("centroid is {d:.0} m from the bulk of the model"),
                ),
            ));
        }
    }
}

// --- 2D helpers ---------------------------------------------------------------

fn project(origin: DVec3, u: DVec3, v: DVec3, p: DVec3) -> (f64, f64) {
    let d = p - origin;
    (d.dot(u), d.dot(v))
}

fn orient(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

fn segments_cross(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> bool {
    // Proper crossing only: touching endpoints / collinear overlap don't count.
    orient(a, b, c) * orient(a, b, d) < 0.0 && orient(c, d, a) * orient(c, d, b) < 0.0
}

/// First pair of non-adjacent edges that properly cross, as edge start indices.
pub(crate) fn self_intersection(poly: &[(f64, f64)]) -> Option<(usize, usize)> {
    let n = poly.len();
    for i in 0..n {
        for j in (i + 1)..n {
            if (i + 1) % n == j || (j + 1) % n == i {
                continue;
            }
            if segments_cross(poly[i], poly[(i + 1) % n], poly[j], poly[(j + 1) % n]) {
                return Some((i, j));
            }
        }
    }
    None
}

fn point_in_polygon(poly: &[(f64, f64)], p: (f64, f64)) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (x1, y1) = poly[i];
        let (x2, y2) = poly[(i + 1) % n];
        if (y1 > p.1) != (y2 > p.1) {
            let x = x1 + (p.1 - y1) / (y2 - y1) * (x2 - x1);
            if x > p.0 {
                inside = !inside;
            }
        }
    }
    inside
}

fn dist_point_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let ab = (b.0 - a.0, b.1 - a.1);
    let len2 = ab.0 * ab.0 + ab.1 * ab.1;
    let t = if len2 < 1e-18 {
        0.0
    } else {
        (((p.0 - a.0) * ab.0 + (p.1 - a.1) * ab.1) / len2).clamp(0.0, 1.0)
    };
    let q = (a.0 + t * ab.0, a.1 + t * ab.1);
    ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt()
}

fn dist_to_polygon_edge(poly: &[(f64, f64)], p: (f64, f64)) -> f64 {
    (0..poly.len())
        .map(|i| dist_point_segment(p, poly[i], poly[(i + 1) % poly.len()]))
        .fold(f64::MAX, f64::min)
}

/// Do the interiors of two polygons overlap (beyond merely touching)?
fn polys_overlap(a: &[(f64, f64)], b: &[(f64, f64)], tol: f64) -> bool {
    let inside = |poly: &[(f64, f64)], p: (f64, f64)| {
        point_in_polygon(poly, p) && dist_to_polygon_edge(poly, p) > tol
    };
    if a.iter().any(|&p| inside(b, p)) || b.iter().any(|&p| inside(a, p)) {
        return true;
    }
    // Edge midpoints catch overlaps whose corners all lie on boundaries.
    let mid = |poly: &[(f64, f64)], i: usize| {
        let q = poly[(i + 1) % poly.len()];
        ((poly[i].0 + q.0) / 2.0, (poly[i].1 + q.1) / 2.0)
    };
    if (0..a.len()).any(|i| inside(b, mid(a, i))) || (0..b.len()).any(|i| inside(a, mid(b, i))) {
        return true;
    }
    (0..a.len()).any(|i| {
        (0..b.len()).any(|j| {
            segments_cross(a[i], a[(i + 1) % a.len()], b[j], b[(j + 1) % b.len()])
        })
    })
}

#[cfg(test)]
mod tests {
    use crate::idf::parse;
    use crate::model::{build, ProblemKind};

    fn has_kind(problems: &[crate::model::Problem], kind: ProblemKind) -> bool {
        problems.iter().any(|p| p.kind == kind)
    }

    fn wall_prefix() -> &'static str {
        "\
Version, 25.2;
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, Relative;
Zone, Z1, 0, 0, 0, 0, 1, 1;
BuildingSurface:Detailed,
  South Wall, Wall, C1, Z1, SP1, Outdoors, , SunExposed, WindExposed, , 4,
  0, 0, 0,
  10, 0, 0,
  10, 0, 3,
  0, 0, 3;
"
    }

    #[test]
    fn window_outside_base_flagged() {
        let src = format!("{}\nWindow, WinX, C, South Wall, , 1, 8, 2, 5, 0.5;", wall_prefix());
        let m = build(&parse(&src));
        let w = m.surfaces.iter().find(|s| s.name == "WinX").unwrap();
        assert!(has_kind(&w.problems, ProblemKind::SubSurfaceOffBase));
    }

    #[test]
    fn window_inside_base_clean() {
        let src = format!("{}\nWindow, WinOk, C, South Wall, , 1, 2, 1, 3, 1.5;", wall_prefix());
        let m = build(&parse(&src));
        let w = m.surfaces.iter().find(|s| s.name == "WinOk").unwrap();
        assert!(w.problems.is_empty(), "{:?}", w.problems);
    }

    #[test]
    fn overlapping_windows_flagged() {
        let src = format!(
            "{}\nWindow, WinA, C, South Wall, , 1, 2, 1, 3, 1.5;\nWindow, WinB, C, South Wall, , 1, 3, 1, 3, 1.0;",
            wall_prefix()
        );
        let m = build(&parse(&src));
        for name in ["WinA", "WinB"] {
            let w = m.surfaces.iter().find(|s| s.name == name).unwrap();
            assert!(has_kind(&w.problems, ProblemKind::SubSurfaceOverlap), "{name}");
        }
    }

    #[test]
    fn interzone_mismatch_flagged() {
        let src = "\
Version, 25.2;
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, World;
Zone, Z1, 0, 0, 0, 0, 1, 1;
Zone, Z2, 0, 0, 0, 0, 1, 1;
BuildingSurface:Detailed, A, Wall, C, Z1, , Surface, B, NoSun, NoWind, , 4,
  0,0,0, 10,0,0, 10,0,3, 0,0,3;
BuildingSurface:Detailed, B, Wall, C, Z2, , Surface, A, NoSun, NoWind, , 4,
  0,0,0, 0,0,3, 8,0,3, 8,0,0;
";
        let m = build(&parse(src));
        let a = m.surfaces.iter().find(|s| s.name == "A").unwrap();
        assert!(has_kind(&a.problems, ProblemKind::InterzoneMismatch));
    }

    #[test]
    fn duplicate_surface_flagged() {
        let src = "\
Version, 25.2;
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, World;
Zone, Z1, 0, 0, 0, 0, 1, 1;
BuildingSurface:Detailed, D1, Wall, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0,0,0, 10,0,0, 10,0,3, 0,0,3;
BuildingSurface:Detailed, D2, Wall, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0,0,0, 10,0,0, 10,0,3, 0,0,3;
";
        let m = build(&parse(src));
        for name in ["D1", "D2"] {
            let s = m.surfaces.iter().find(|s| s.name == name).unwrap();
            assert!(has_kind(&s.problems, ProblemKind::DuplicateSurface), "{name}");
        }
    }

    #[test]
    fn coplanar_overlap_flagged() {
        let src = "\
Version, 25.2;
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, World;
Zone, Z1, 0, 0, 0, 0, 1, 1;
BuildingSurface:Detailed, O1, Wall, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0,0,0, 10,0,0, 10,0,3, 0,0,3;
BuildingSurface:Detailed, O2, Wall, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  4,0,0, 12,0,0, 12,0,3, 4,0,3;
";
        let m = build(&parse(src));
        for name in ["O1", "O2"] {
            let s = m.surfaces.iter().find(|s| s.name == name).unwrap();
            assert!(has_kind(&s.problems, ProblemKind::CoplanarOverlap), "{name}");
        }
    }

    fn cube_idf(volume_field: &str) -> String {
        format!(
            "\
Version, 25.2;
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, World;
Zone, Z1, 0, 0, 0, 0, 1, 1, 2, {volume_field};
BuildingSurface:Detailed, Flr, Floor, C, Z1, , Ground, , NoSun, NoWind, , 4,
  0,0,0,  0,2,0,  2,2,0,  2,0,0;
BuildingSurface:Detailed, Rf, Roof, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0,0,2,  2,0,2,  2,2,2,  0,2,2;
BuildingSurface:Detailed, S, Wall, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0,0,0,  2,0,0,  2,0,2,  0,0,2;
BuildingSurface:Detailed, N, Wall, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0,2,0,  0,2,2,  2,2,2,  2,2,0;
BuildingSurface:Detailed, W, Wall, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0,2,0,  0,0,0,  0,0,2,  0,2,2;
BuildingSurface:Detailed, E, Wall, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  2,0,0,  2,2,0,  2,2,2,  2,0,2;
"
        )
    }

    #[test]
    fn zone_volume_mismatch_warned() {
        let m = build(&parse(&cube_idf("100")));
        assert!(m.warnings.iter().any(|w| w.contains("volume")), "{:?}", m.warnings);
        // Correct volume (2x2x2 = 8) -> no warning.
        let m = build(&parse(&cube_idf("8")));
        assert!(!m.warnings.iter().any(|w| w.contains("volume")), "{:?}", m.warnings);
    }

    #[test]
    fn zone_hole_flagged() {
        // Cube with the east wall removed: 5 surfaces, open on one side.
        let src: String = cube_idf("8")
            .lines()
            .take_while(|l| !l.starts_with("BuildingSurface:Detailed, E,"))
            .collect::<Vec<_>>()
            .join("\n");
        let m = build(&parse(&src));
        assert_eq!(m.surfaces.len(), 5);
        assert!(m
            .surfaces
            .iter()
            .any(|s| has_kind(&s.problems, ProblemKind::ZoneNotClosed)));
    }

    #[test]
    fn outlier_flagged() {
        let src = format!(
            "{}\
BuildingSurface:Detailed, Near, Wall, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0,1,0,  2,1,0,  2,1,2,  0,1,2;
BuildingSurface:Detailed, Far, Wall, C, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  500,0,0,  502,0,0,  502,0,2,  500,0,2;
",
            cube_idf("8")
        );
        let m = build(&parse(&src));
        let far = m.surfaces.iter().find(|s| s.name == "Far").unwrap();
        assert!(has_kind(&far.problems, ProblemKind::Outlier));
        let near = m.surfaces.iter().find(|s| s.name == "Near").unwrap();
        assert!(!has_kind(&near.problems, ProblemKind::Outlier));
    }
}
