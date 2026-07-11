//! Builds world-space, triangulated surface geometry from parsed IDF objects.

use crate::idf::IdfObject;
use glam::{DVec3, Vec3};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SurfaceType {
    Wall,
    Floor,
    Ceiling,
    Roof,
    Window,
    Door,
    Shading,
}

impl SurfaceType {
    pub const ALL: [SurfaceType; 7] = [
        SurfaceType::Wall,
        SurfaceType::Floor,
        SurfaceType::Ceiling,
        SurfaceType::Roof,
        SurfaceType::Window,
        SurfaceType::Door,
        SurfaceType::Shading,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SurfaceType::Wall => "Wall",
            SurfaceType::Floor => "Floor",
            SurfaceType::Ceiling => "Ceiling",
            SurfaceType::Roof => "Roof",
            SurfaceType::Window => "Window",
            SurfaceType::Door => "Door",
            SurfaceType::Shading => "Shading",
        }
    }

    /// OpenStudio-style render-by-surface-type colors (RGBA).
    pub fn color(self) -> [f32; 4] {
        let c = |r: u8, g: u8, b: u8, a: f32| {
            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a]
        };
        match self {
            SurfaceType::Wall => c(204, 178, 102, 1.0),
            SurfaceType::Floor => c(128, 128, 128, 1.0),
            SurfaceType::Ceiling => c(207, 122, 90, 1.0),
            SurfaceType::Roof => c(153, 76, 76, 1.0),
            SurfaceType::Window => c(102, 178, 204, 0.55),
            SurfaceType::Door => c(153, 133, 76, 1.0),
            SurfaceType::Shading => c(136, 110, 176, 0.8),
        }
    }

    pub fn is_transparent(self) -> bool {
        matches!(self, SurfaceType::Window | SurfaceType::Shading)
    }

    /// Draw-order tie-break for coplanar overlaps: higher priority renders in
    /// front (window/door beat their host wall, a floor beats the ceiling of
    /// the zone below).
    pub fn depth_priority(self) -> u32 {
        match self {
            SurfaceType::Window | SurfaceType::Door => 2,
            SurfaceType::Floor | SurfaceType::Roof => 1,
            SurfaceType::Wall | SurfaceType::Ceiling | SurfaceType::Shading => 0,
        }
    }

    fn from_idf(s: &str) -> SurfaceType {
        match s.to_ascii_lowercase().as_str() {
            "wall" => SurfaceType::Wall,
            "floor" => SurfaceType::Floor,
            "ceiling" => SurfaceType::Ceiling,
            "roof" => SurfaceType::Roof,
            "window" | "glazeddoor" | "tdd:dome" | "tdd:diffuser" => SurfaceType::Window,
            "door" => SurfaceType::Door,
            _ => SurfaceType::Wall,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// Kinds of geometry problems the loader and analysis passes can flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProblemKind {
    Degenerate,
    SelfIntersecting,
    TriangulationFailed,
    NonPlanar,
    DuplicateVertex,
    CollinearVertex,
    Sliver,
    UpsideDown,
    SubSurfaceOffBase,
    SubSurfaceTooBig,
    SubSurfaceOverlap,
    InterzoneMismatch,
    DuplicateSurface,
    CoplanarOverlap,
    ZoneNotClosed,
    Outlier,
}

impl ProblemKind {
    pub fn label(self) -> &'static str {
        match self {
            ProblemKind::Degenerate => "Degenerate",
            ProblemKind::SelfIntersecting => "Self-intersecting",
            ProblemKind::TriangulationFailed => "Triangulation failed",
            ProblemKind::NonPlanar => "Non-planar",
            ProblemKind::DuplicateVertex => "Duplicate vertices",
            ProblemKind::CollinearVertex => "Collinear vertices",
            ProblemKind::Sliver => "Sliver",
            ProblemKind::UpsideDown => "Type/tilt mismatch",
            ProblemKind::SubSurfaceOffBase => "Sub-surface off base",
            ProblemKind::SubSurfaceTooBig => "Sub-surface too big",
            ProblemKind::SubSurfaceOverlap => "Sub-surfaces overlap",
            ProblemKind::InterzoneMismatch => "Interzone mismatch",
            ProblemKind::DuplicateSurface => "Duplicate surface",
            ProblemKind::CoplanarOverlap => "Coplanar overlap",
            ProblemKind::ZoneNotClosed => "Zone not closed",
            ProblemKind::Outlier => "Far from model",
        }
    }

    pub fn default_severity(self) -> Severity {
        match self {
            ProblemKind::Degenerate
            | ProblemKind::SelfIntersecting
            | ProblemKind::TriangulationFailed
            | ProblemKind::SubSurfaceOffBase
            | ProblemKind::SubSurfaceTooBig
            | ProblemKind::InterzoneMismatch => Severity::Error,
            ProblemKind::NonPlanar
            | ProblemKind::DuplicateVertex
            | ProblemKind::CollinearVertex
            | ProblemKind::UpsideDown
            | ProblemKind::SubSurfaceOverlap
            | ProblemKind::DuplicateSurface
            | ProblemKind::CoplanarOverlap
            | ProblemKind::Outlier => Severity::Warning,
            ProblemKind::Sliver | ProblemKind::ZoneNotClosed => Severity::Info,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Problem {
    pub kind: ProblemKind,
    pub severity: Severity,
    pub message: String,
}

impl Problem {
    pub fn new(kind: ProblemKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            severity: kind.default_severity(),
            message: message.into(),
        }
    }

    pub fn with_severity(kind: ProblemKind, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            kind,
            severity,
            message: message.into(),
        }
    }
}

/// Per-vertex diagnostics (drawn as colored dots on the selected surface).
#[derive(Debug, Clone, Copy, Default)]
pub struct VertexFlags {
    /// Coincides with the next vertex (< 1 mm apart).
    pub duplicate: bool,
    /// Lies exactly on the line between its neighbors (redundant).
    pub collinear: bool,
    /// Within ~1 degree of collinear with its neighbors.
    pub near_collinear: bool,
    /// Signed distance (m) from the surface's mean plane.
    pub plane_dev: f32,
}

#[derive(Debug, Clone)]
pub struct Surface {
    pub name: String,
    pub stype: SurfaceType,
    pub class: String,
    pub construction: String,
    pub zone: String,
    pub space: String,
    pub boundary: String,
    pub boundary_object: String,
    /// World-space polygon vertices (meters).
    pub verts: Vec<Vec3>,
    /// Triangle indices into `verts`.
    pub tris: Vec<u32>,
    /// Outward unit normal (Newell).
    pub normal: Vec3,
    pub centroid: Vec3,
    pub area: f64,
    /// Degrees clockwise from north (+Y). Only meaningful for non-horizontal surfaces.
    pub azimuth: f64,
    /// Degrees from horizontal-facing-up: 0 = facing up, 90 = vertical, 180 = facing down.
    pub tilt: f64,
    /// Raw IDF text of the defining object.
    pub raw: String,
    /// 1-based line number in the source file.
    pub line: usize,
    /// Geometry problems detected while building/analyzing (empty = OK).
    pub problems: Vec<Problem>,
    /// Per-vertex diagnostics, same length as `verts`.
    pub vert_flags: Vec<VertexFlags>,
    /// Index of the base surface if this is a sub-surface.
    pub base_surface: Option<usize>,
}

pub struct Model {
    pub surfaces: Vec<Surface>,
    pub warnings: Vec<String>,
}

struct ZoneInfo {
    origin: DVec3,
    /// Radians; zone "Direction of Relative North" plus building north axis.
    rotation: f64,
}

pub fn build(objects: &[IdfObject]) -> Model {
    let mut warnings = Vec::new();

    // --- Global settings ---------------------------------------------------
    let mut ccw = true;
    let mut world_coords = false;
    if let Some(ggr) = objects.iter().find(|o| eq(&o.class, "GlobalGeometryRules")) {
        ccw = !ggr.field(1).eq_ignore_ascii_case("Clockwise");
        world_coords = ggr.field(2).eq_ignore_ascii_case("World")
            || ggr.field(2).eq_ignore_ascii_case("Absolute");
    }

    let building_north = objects
        .iter()
        .find(|o| eq(&o.class, "Building"))
        .and_then(|o| o.field_f64(1))
        .unwrap_or(0.0);

    let version = objects
        .iter()
        .find(|o| eq(&o.class, "Version"))
        .and_then(|o| {
            let f = o.field(0);
            let mut it = f.split('.');
            let maj: u32 = it.next()?.trim().parse().ok()?;
            let min: u32 = it.next().unwrap_or("0").trim().parse().unwrap_or(0);
            Some((maj, min))
        })
        .unwrap_or((25, 2));
    // Space Name field was added to BuildingSurface:Detailed in 22.1.
    let has_space_field = version >= (22, 1);

    // --- Zones -------------------------------------------------------------
    let mut zones: HashMap<String, ZoneInfo> = HashMap::new();
    let mut zone_volumes: HashMap<String, f64> = HashMap::new();
    for o in objects.iter().filter(|o| eq(&o.class, "Zone")) {
        if let Some(v) = o.field_f64(8) {
            if v > 0.0 {
                zone_volumes.insert(o.field(0).to_ascii_uppercase(), v);
            }
        }
        zones.insert(
            o.field(0).to_ascii_uppercase(),
            ZoneInfo {
                origin: DVec3::new(
                    o.field_f64(2).unwrap_or(0.0),
                    o.field_f64(3).unwrap_or(0.0),
                    o.field_f64(4).unwrap_or(0.0),
                ),
                rotation: (o.field_f64(1).unwrap_or(0.0) + building_north).to_radians(),
            },
        );
    }

    let to_world = |zone: Option<&ZoneInfo>, v: DVec3| -> DVec3 {
        if world_coords {
            return v;
        }
        match zone {
            Some(z) => {
                let (s, c) = z.rotation.sin_cos();
                DVec3::new(
                    z.origin.x + v.x * c + v.y * s,
                    z.origin.y - v.x * s + v.y * c,
                    z.origin.z + v.z,
                )
            }
            None => v,
        }
    };

    let mut surfaces: Vec<Surface> = Vec::new();

    // --- BuildingSurface:Detailed -------------------------------------------
    for o in objects.iter().filter(|o| eq(&o.class, "BuildingSurface:Detailed")) {
        // coord_start is the index of the "Number of Vertices" field.
        let (zone_idx, coord_start) = if has_space_field { (3, 10) } else { (3, 9) };
        let zone_name = o.field(zone_idx).to_string();
        let zone = zones.get(&zone_name.to_ascii_uppercase());
        if zone.is_none() && !world_coords {
            warnings.push(format!(
                "{} \"{}\": zone \"{}\" not found; treating vertices as world coordinates",
                o.class,
                o.field(0),
                zone_name
            ));
        }
        let Some(verts) = read_vertices(o, coord_start) else {
            warnings.push(format!("{} \"{}\": could not read vertices", o.class, o.field(0)));
            continue;
        };
        let verts: Vec<DVec3> = verts.into_iter().map(|v| to_world(zone, v)).collect();
        surfaces.push(make_surface(
            o,
            o.field(0),
            SurfaceType::from_idf(o.field(1)),
            o.field(2),
            &zone_name,
            if has_space_field { o.field(4) } else { "" },
            o.field(if has_space_field { 5 } else { 4 }),
            o.field(if has_space_field { 6 } else { 5 }),
            verts,
            ccw,
        ));
    }

    // Base-surface lookup for subsurfaces (upper-cased name -> index).
    let base_index: HashMap<String, usize> = surfaces
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.to_ascii_uppercase(), i))
        .collect();
    let base_zone_map: HashMap<String, String> = surfaces
        .iter()
        .map(|s| (s.name.to_ascii_uppercase(), s.zone.clone()))
        .collect();
    let base_zone =
        |name: &str| -> Option<String> { base_zone_map.get(&name.to_ascii_uppercase()).cloned() };

    // --- FenestrationSurface:Detailed ---------------------------------------
    for o in objects.iter().filter(|o| eq(&o.class, "FenestrationSurface:Detailed")) {
        let base_name = o.field(3);
        let zone_name = base_zone(base_name).unwrap_or_default();
        let zone = zones.get(&zone_name.to_ascii_uppercase());
        let Some(verts) = read_vertices(o, 8) else {
            warnings.push(format!("{} \"{}\": could not read vertices", o.class, o.field(0)));
            continue;
        };
        let verts: Vec<DVec3> = verts.into_iter().map(|v| to_world(zone, v)).collect();
        let mut s = make_surface(
            o,
            o.field(0),
            SurfaceType::from_idf(o.field(1)),
            o.field(2),
            &zone_name,
            "",
            "",
            o.field(4),
            verts,
            ccw,
        );
        s.boundary = format!("(sub-surface of {base_name})");
        s.base_surface = base_index.get(&base_name.to_ascii_uppercase()).copied();
        offset_along_normal(&mut s, 0.015);
        surfaces.push(s);
    }

    // --- Rectangular subsurfaces: Window / Door / GlazedDoor ----------------
    for o in objects
        .iter()
        .filter(|o| eq(&o.class, "Window") || eq(&o.class, "Door") || eq(&o.class, "GlazedDoor"))
    {
        // Window/GlazedDoor: base at field 2, x,z,len,h at 5..9. Door: base 2, x,z,len,h at 4..8.
        let is_door = eq(&o.class, "Door");
        let base_name = o.field(2).to_string();
        // Door: Name, Construction, Base Surface, Multiplier, X, Z, Len, H.
        // Window/GlazedDoor additionally have Frame and Divider before Multiplier.
        let (fx, fz, fl, fh) = if is_door { (4, 5, 6, 7) } else { (5, 6, 7, 8) };
        let (Some(sx), Some(sz), Some(len), Some(h)) = (
            o.field_f64(fx),
            o.field_f64(fz),
            o.field_f64(fl),
            o.field_f64(fh),
        ) else {
            warnings.push(format!("{} \"{}\": missing rectangle fields", o.class, o.field(0)));
            continue;
        };
        let Some(&bi) = base_index.get(&base_name.to_ascii_uppercase()) else {
            warnings.push(format!(
                "{} \"{}\": base surface \"{}\" not found",
                o.class,
                o.field(0),
                base_name
            ));
            continue;
        };
        let base = surfaces[bi].clone();
        let verts = rectangle_on_surface(&base, sx, sz, len, h);
        let stype = if is_door { SurfaceType::Door } else { SurfaceType::Window };
        let mut s = make_surface(
            o,
            o.field(0),
            stype,
            o.field(1),
            &base.zone.clone(),
            "",
            "",
            "",
            verts,
            true, // constructed CCW-from-outside by rectangle_on_surface
        );
        s.boundary = format!("(sub-surface of {base_name})");
        s.base_surface = Some(bi);
        offset_along_normal(&mut s, 0.015);
        surfaces.push(s);
    }

    // --- Shading surfaces ----------------------------------------------------
    for o in objects.iter().filter(|o| {
        eq(&o.class, "Shading:Site:Detailed") || eq(&o.class, "Shading:Building:Detailed")
    }) {
        // Name, Transmittance Schedule, NumVertices, coords...
        let Some(verts) = read_vertices(o, 2) else {
            warnings.push(format!("{} \"{}\": could not read vertices", o.class, o.field(0)));
            continue;
        };
        let verts: Vec<DVec3> = verts.into_iter().collect();
        surfaces.push(make_surface(
            o, o.field(0), SurfaceType::Shading, "", "", "", "", "", verts, ccw,
        ));
    }
    for o in objects.iter().filter(|o| eq(&o.class, "Shading:Zone:Detailed")) {
        // Name, Base Surface Name, Transmittance Schedule, NumVertices, coords...
        let zone_name = base_zone(o.field(1)).unwrap_or_default();
        let zone = zones.get(&zone_name.to_ascii_uppercase());
        let Some(verts) = read_vertices(o, 3) else {
            warnings.push(format!("{} \"{}\": could not read vertices", o.class, o.field(0)));
            continue;
        };
        let verts: Vec<DVec3> = verts.into_iter().map(|v| to_world(zone, v)).collect();
        surfaces.push(make_surface(
            o, o.field(0), SurfaceType::Shading, "", &zone_name, "", "", "", verts, ccw,
        ));
    }

    let mut model = Model { surfaces, warnings };
    crate::analysis::analyze(&mut model, &zone_volumes);
    model
}

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Read trailing X,Y,Z vertex fields starting at `start` (skipping a leading
/// "Number of Vertices" style field if present there).
fn read_vertices(o: &IdfObject, start: usize) -> Option<Vec<DVec3>> {
    let mut i = start;
    // The field at `start` is "Number of Vertices" (may be blank or autocalculate).
    // Coordinates begin at start+1.
    i += 1;
    if i >= o.fields.len() {
        return None;
    }
    let coords: Vec<f64> = o.fields[i..]
        .iter()
        .map(|f| f.parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    if coords.len() < 9 || coords.len() % 3 != 0 {
        return None;
    }
    Some(
        coords
            .chunks_exact(3)
            .map(|c| DVec3::new(c[0], c[1], c[2]))
            .collect(),
    )
}

/// Newell's method; returns non-unit normal whose length is 2x polygon area.
fn newell(verts: &[DVec3]) -> DVec3 {
    let mut n = DVec3::ZERO;
    for i in 0..verts.len() {
        let a = verts[i];
        let b = verts[(i + 1) % verts.len()];
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n
}

#[allow(clippy::too_many_arguments)]
fn make_surface(
    o: &IdfObject,
    name: &str,
    stype: SurfaceType,
    construction: &str,
    zone: &str,
    space: &str,
    boundary: &str,
    boundary_object: &str,
    verts: Vec<DVec3>,
    ccw: bool,
) -> Surface {
    let mut problems: Vec<Problem> = Vec::new();
    let nv = verts.len();
    const DUP_TOL: f64 = 1e-3; // 1 mm

    let mut n = newell(&verts);
    if !ccw {
        n = -n; // vertices entered clockwise viewed from outside
    }
    let area = n.length() / 2.0;
    if area < 1e-9 {
        problems.push(Problem::new(
            ProblemKind::Degenerate,
            "zero-area surface".to_string(),
        ));
    }
    let normal = if area > 1e-12 { n.normalize() } else { DVec3::Z };

    let mut flags = vec![VertexFlags::default(); nv];

    // Coincident consecutive vertices.
    let mut dups = 0;
    for i in 0..nv {
        if (verts[(i + 1) % nv] - verts[i]).length() < DUP_TOL {
            flags[i].duplicate = true;
            dups += 1;
        }
    }
    if dups > 0 {
        problems.push(Problem::new(
            ProblemKind::DuplicateVertex,
            format!("{dups} pair(s) of consecutive vertices less than 1 mm apart"),
        ));
    }

    // Collinear / nearly collinear vertices (zero-length edges skipped —
    // those are already flagged as duplicates).
    let (mut collinear, mut near) = (0, 0);
    for i in 0..nv {
        let e1 = verts[i] - verts[(i + nv - 1) % nv];
        let e2 = verts[(i + 1) % nv] - verts[i];
        if e1.length() < DUP_TOL || e2.length() < DUP_TOL {
            continue;
        }
        let sin = e1.cross(e2).length() / (e1.length() * e2.length());
        if sin < 1e-4 {
            flags[i].collinear = true;
            collinear += 1;
        } else if sin < 0.0175 {
            flags[i].near_collinear = true;
            near += 1;
        }
    }
    if collinear > 0 {
        problems.push(Problem::new(
            ProblemKind::CollinearVertex,
            format!("{collinear} redundant vertex(es) exactly on the line between their neighbors"),
        ));
    }
    if near > 0 {
        problems.push(Problem::with_severity(
            ProblemKind::CollinearVertex,
            Severity::Info,
            format!("{near} vertex(es) within 1° of collinear with their neighbors"),
        ));
    }

    // Sliver shapes: very short edges or an extreme aspect ratio.
    let (mut shortest, mut longest) = (f64::MAX, 0.0f64);
    for i in 0..nv {
        let l = (verts[(i + 1) % nv] - verts[i]).length();
        if l >= DUP_TOL {
            shortest = shortest.min(l);
        }
        longest = longest.max(l);
    }
    if nv >= 3 && shortest < 0.01 {
        problems.push(Problem::new(
            ProblemKind::Sliver,
            format!("shortest edge is only {:.0} mm", shortest * 1000.0),
        ));
    }
    if area > 1e-9 && longest * longest / area > 1e4 {
        problems.push(Problem::new(
            ProblemKind::Sliver,
            format!("extremely thin (longest edge² / area ≈ {:.0})", longest * longest / area),
        ));
    }

    // Planarity: vertex distance from the mean plane.
    let centroid = verts.iter().copied().sum::<DVec3>() / verts.len().max(1) as f64;
    let mut max_dev = 0.0f64;
    for (i, v) in verts.iter().enumerate() {
        let d = (*v - centroid).dot(normal);
        flags[i].plane_dev = d as f32;
        max_dev = max_dev.max(d.abs());
    }
    if max_dev > 0.01 {
        problems.push(Problem::with_severity(
            ProblemKind::NonPlanar,
            if max_dev > 0.1 { Severity::Error } else { Severity::Warning },
            format!("vertices deviate up to {max_dev:.3} m from the mean plane"),
        ));
    }

    // Triangulate in the plane of the polygon.
    let (u, v) = plane_basis(normal);
    let pts2d: Vec<(f64, f64)> = verts
        .iter()
        .map(|p| {
            let d = *p - centroid;
            (d.dot(u), d.dot(v))
        })
        .collect();

    if nv >= 4 && area > 1e-9 {
        if let Some((i, j)) = crate::analysis::self_intersection(&pts2d) {
            problems.push(Problem::new(
                ProblemKind::SelfIntersecting,
                format!(
                    "edge {}→{} crosses edge {}→{}",
                    i + 1,
                    (i + 1) % nv + 1,
                    j + 1,
                    (j + 1) % nv + 1
                ),
            ));
        }
    }

    let poly2d: Vec<f64> = pts2d.iter().flat_map(|&(x, y)| [x, y]).collect();
    let tris: Vec<u32> = match earcutr::earcut(&poly2d, &[], 2) {
        Ok(t) if !t.is_empty() => t.into_iter().map(|i| i as u32).collect(),
        _ => {
            if verts.len() >= 3 {
                problems.push(Problem::new(
                    ProblemKind::TriangulationFailed,
                    "triangulation failed; rendering with a vertex fan".to_string(),
                ));
                (1..verts.len() as u32 - 1)
                    .flat_map(|i| [0, i, i + 1])
                    .collect()
            } else {
                Vec::new()
            }
        }
    };

    // Azimuth: degrees clockwise from north (+Y), of the outward normal.
    let azimuth = {
        let a = normal.x.atan2(normal.y).to_degrees();
        if a < 0.0 { a + 360.0 } else { a }
    };
    let tilt = normal.z.clamp(-1.0, 1.0).acos().to_degrees();

    // Surface type vs. orientation (usually a reversed vertex order).
    if area >= 1e-9 {
        let tilt_msg = match stype {
            SurfaceType::Wall => (!(60.0..=120.0).contains(&tilt))
                .then(|| format!("wall tilt is {tilt:.0}° (expected ~90°)")),
            SurfaceType::Floor => (tilt < 150.0).then(|| {
                if tilt < 30.0 {
                    format!("floor faces up (tilt {tilt:.0}°); vertex order is likely reversed")
                } else {
                    format!("floor tilt is {tilt:.0}° (expected ~180°, facing down)")
                }
            }),
            SurfaceType::Ceiling | SurfaceType::Roof => (tilt > 60.0).then(|| {
                let what = if stype == SurfaceType::Ceiling { "ceiling" } else { "roof" };
                if tilt > 150.0 {
                    format!("{what} faces down (tilt {tilt:.0}°); vertex order is likely reversed")
                } else {
                    format!("{what} tilt is {tilt:.0}° (expected < 60°, facing up)")
                }
            }),
            _ => None,
        };
        if let Some(msg) = tilt_msg {
            problems.push(Problem::new(ProblemKind::UpsideDown, msg));
        }
    }

    Surface {
        name: name.to_string(),
        stype,
        class: o.class.clone(),
        construction: construction.to_string(),
        zone: zone.to_string(),
        space: space.to_string(),
        boundary: boundary.to_string(),
        boundary_object: boundary_object.to_string(),
        verts: verts.iter().map(|p| p.as_vec3()).collect(),
        tris,
        normal: normal.as_vec3(),
        centroid: centroid.as_vec3(),
        area,
        azimuth,
        tilt,
        raw: o.raw.clone(),
        line: o.line,
        problems,
        vert_flags: flags,
        base_surface: None,
    }
}

/// Orthonormal basis (u, v) spanning the plane with normal n, with v pointing
/// as "up" as possible (so u is horizontal for walls).
pub(crate) fn plane_basis(n: DVec3) -> (DVec3, DVec3) {
    let up = if n.z.abs() > 0.99 { DVec3::Y } else { DVec3::Z };
    let u = up.cross(n).normalize();
    let v = n.cross(u).normalize();
    (u, v)
}

/// Build the 4 corners of a rectangular subsurface (Window/Door object) in the
/// plane of `base`, positioned from the base surface's lower-left corner as
/// viewed from outside; sx along the width axis, sz up.
fn rectangle_on_surface(base: &Surface, sx: f64, sz: f64, len: f64, h: f64) -> Vec<DVec3> {
    let n = base.normal.as_dvec3();
    let (u, v) = plane_basis(n);
    // Origin: the base surface corner with minimal (u, v) coordinates.
    let c = base.centroid.as_dvec3();
    let (mut min_u, mut min_v) = (f64::MAX, f64::MAX);
    for p in &base.verts {
        let d = p.as_dvec3() - c;
        min_u = min_u.min(d.dot(u));
        min_v = min_v.min(d.dot(v));
    }
    let origin = c + u * min_u + v * min_v;
    // CCW as viewed from outside (from the +n side).
    [
        (sx, sz),
        (sx + len, sz),
        (sx + len, sz + h),
        (sx, sz + h),
    ]
    .into_iter()
    .map(|(x, z)| origin + u * x + v * z)
    .collect()
}

/// Nudge a sub-surface off its host plane to avoid z-fighting.
fn offset_along_normal(s: &mut Surface, dist: f32) {
    let off = s.normal * dist;
    for v in &mut s.verts {
        *v += off;
    }
    s.centroid += off;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idf::parse;

    fn simple_wall_idf() -> &'static str {
        "\
Version, 25.2;
Building, Test, 0, Suburbs, 0.04, 0.4, FullExterior, 25, 6;
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, Relative;
Zone, Z1, 0, 10, 20, 5, 1, 1;
BuildingSurface:Detailed,
  South Wall, Wall, C1, Z1, SP1, Outdoors, , SunExposed, WindExposed, , 4,
  0, 0, 0,
  10, 0, 0,
  10, 0, 3,
  0, 0, 3;
"
    }

    #[test]
    fn wall_world_transform_and_normal() {
        let m = build(&parse(simple_wall_idf()));
        assert_eq!(m.surfaces.len(), 1);
        let s = &m.surfaces[0];
        assert_eq!(s.name, "South Wall");
        assert_eq!(s.stype, SurfaceType::Wall);
        // Zone origin (10, 20, 5) applied.
        assert!((s.verts[0] - Vec3::new(10.0, 20.0, 5.0)).length() < 1e-4);
        assert!((s.verts[2] - Vec3::new(20.0, 20.0, 8.0)).length() < 1e-4);
        // South-facing wall: normal -Y, azimuth 180, tilt 90.
        assert!((s.normal - Vec3::new(0.0, -1.0, 0.0)).length() < 1e-4);
        assert!((s.azimuth - 180.0).abs() < 1e-3);
        assert!((s.tilt - 90.0).abs() < 1e-3);
        assert!((s.area - 30.0).abs() < 1e-6);
        assert_eq!(s.tris.len(), 6);
        assert!(s.problems.is_empty(), "{:?}", s.problems);
    }

    #[test]
    fn zone_relative_north_rotates() {
        let src = "\
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, Relative;
Zone, Z1, 90, 0, 0, 0, 1, 1;
BuildingSurface:Detailed,
  W, Wall, C1, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0, 0, 0,
  10, 0, 0,
  10, 0, 3,
  0, 0, 3;
Version, 25.2;
";
        let m = build(&parse(src));
        let s = &m.surfaces[0];
        // Zone rotated 90 deg clockwise: local +X becomes world -Y.
        assert!((s.verts[1] - Vec3::new(0.0, -10.0, 0.0)).length() < 1e-4);
        // South wall (local -Y normal) now faces west (-X): azimuth 270.
        assert!((s.azimuth - 270.0).abs() < 1e-3);
    }

    #[test]
    fn rectangular_window_on_wall() {
        let src = format!(
            "{}\nWindow, Win1, WinCon, South Wall, , 1, 2, 1, 3, 1.5;",
            simple_wall_idf()
        );
        let m = build(&parse(&src));
        assert_eq!(m.surfaces.len(), 2);
        let w = &m.surfaces[1];
        assert_eq!(w.stype, SurfaceType::Window);
        assert!((w.area - 4.5).abs() < 1e-6);
        // Window normal matches wall normal.
        assert!((w.normal - Vec3::new(0.0, -1.0, 0.0)).length() < 1e-4);
        // Lower-left of wall is (10,20,5); wall runs along -X viewed... its u/v
        // frame gives origin at min corner; window z from 1 above base z.
        let zmin = w.verts.iter().map(|v| v.z).fold(f32::MAX, f32::min);
        let zmax = w.verts.iter().map(|v| v.z).fold(f32::MIN, f32::max);
        assert!((zmin - 6.0).abs() < 0.02); // 5 + 1
        assert!((zmax - 7.5).abs() < 0.02); // 5 + 1 + 1.5
    }

    #[test]
    fn nonconvex_polygon_triangulates() {
        // L-shaped floor polygon (CCW viewed from above -> facing up; floors
        // are usually listed to face down, but this checks triangulation).
        let src = "\
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, World;
BuildingSurface:Detailed,
  L, Floor, C1, Z1, , Ground, , NoSun, NoWind, , 6,
  0, 0, 0,
  4, 0, 0,
  4, 2, 0,
  2, 2, 0,
  2, 4, 0,
  0, 4, 0;
Version, 25.2;
";
        let m = build(&parse(src));
        let s = &m.surfaces[0];
        assert_eq!(s.tris.len(), 4 * 3);
        assert!((s.area - 12.0).abs() < 1e-6);
        // Faces up but is typed Floor -> exactly the type/tilt mismatch, nothing else.
        assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
        assert_eq!(s.problems[0].kind, ProblemKind::UpsideDown);
    }

    #[test]
    fn collinear_vertex_flagged() {
        let src = "\
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, World;
BuildingSurface:Detailed,
  W, Wall, C1, Z1, , Outdoors, , SunExposed, WindExposed, , 5,
  0, 0, 0,
  5, 0, 0,
  10, 0, 0,
  10, 0, 3,
  0, 0, 3;
Version, 25.2;
";
        let m = build(&parse(src));
        let s = &m.surfaces[0];
        assert!(s.problems.iter().any(|p| p.kind == ProblemKind::CollinearVertex));
        assert!(s.vert_flags[1].collinear);
        assert!(!s.vert_flags[0].collinear);
    }

    #[test]
    fn duplicate_vertex_flagged() {
        let src = "\
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, World;
BuildingSurface:Detailed,
  W, Wall, C1, Z1, , Outdoors, , SunExposed, WindExposed, , 5,
  0, 0, 0,
  10, 0, 0,
  10, 0, 0,
  10, 0, 3,
  0, 0, 3;
Version, 25.2;
";
        let m = build(&parse(src));
        let s = &m.surfaces[0];
        assert!(s.problems.iter().any(|p| p.kind == ProblemKind::DuplicateVertex));
        assert!(s.vert_flags[1].duplicate);
    }

    #[test]
    fn self_intersecting_flagged() {
        // Bowtie: edge 2->3 crosses edge 4->1.
        let src = "\
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, World;
BuildingSurface:Detailed,
  W, Wall, C1, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0, 0, 0,
  4, 0, 0,
  1, 0, 3,
  3, 0, 3;
Version, 25.2;
";
        let m = build(&parse(src));
        let s = &m.surfaces[0];
        assert!(s.problems.iter().any(|p| p.kind == ProblemKind::SelfIntersecting));
    }

    #[test]
    fn sliver_flagged() {
        let src = "\
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, World;
BuildingSurface:Detailed,
  W, Wall, C1, Z1, , Outdoors, , SunExposed, WindExposed, , 4,
  0, 0, 0,
  10, 0, 0,
  10, 0, 0.005,
  0, 0, 0.005;
Version, 25.2;
";
        let m = build(&parse(src));
        let s = &m.surfaces[0];
        assert!(s.problems.iter().any(|p| p.kind == ProblemKind::Sliver));
    }

    #[test]
    fn degenerate_surface_flagged() {
        let src = "\
GlobalGeometryRules, LowerLeftCorner, Counterclockwise, World;
BuildingSurface:Detailed,
  Bad, Wall, C1, Z1, , Outdoors, , SunExposed, WindExposed, , 3,
  0, 0, 0,
  1, 0, 0,
  2, 0, 0;
Version, 25.2;
";
        let m = build(&parse(src));
        assert!(m.surfaces[0]
            .problems
            .iter()
            .any(|p| p.kind == ProblemKind::Degenerate));
    }
}
