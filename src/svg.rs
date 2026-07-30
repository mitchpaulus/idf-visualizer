//! Headless SVG export: orthographic (isometric by default) vector drawing of
//! the model, sized to fit its content. Intended for figures in reports.

use crate::model::{Model, Surface, SurfaceType};
use glam::Vec3;
use regex::Regex;
use std::fmt::Write as _;

/// True isometric elevation: atan(1/sqrt(2)) in degrees.
pub const ISO_ELEVATION: f32 = 35.264_39;

pub struct SvgOptions {
    /// View azimuth in degrees; 0 looks from the south, positive swings east
    /// (same convention as the interactive camera's yaw).
    pub rotation: f32,
    /// Degrees above the horizon.
    pub elevation: f32,
    /// Output width in px. Height follows the content unless `height` is set.
    pub width: f32,
    pub height: Option<f32>,
    pub margin: f32,
    pub stroke_width: f32,
    /// Hide back-facing opaque surfaces (interior of the far side).
    pub cull: bool,
    /// Shade faces by their angle to the viewer.
    pub shade: bool,
    pub legend: bool,
    pub background: Option<String>,
    pub zone: Option<Regex>,
    pub name: Option<Regex>,
    pub hide: Vec<SurfaceType>,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            rotation: 45.0,
            elevation: ISO_ELEVATION,
            width: 1000.0,
            height: None,
            margin: 24.0,
            stroke_width: 0.8,
            cull: true,
            shade: true,
            legend: false,
            background: None,
            zone: None,
            name: None,
            hide: Vec::new(),
        }
    }
}

/// Orthographic camera basis: `dir` points from the model toward the eye.
struct Basis {
    dir: Vec3,
    right: Vec3,
    up: Vec3,
}

impl Basis {
    fn new(rotation_deg: f32, elevation_deg: f32) -> Self {
        let (sy, cy) = rotation_deg.to_radians().sin_cos();
        let (sp, cp) = elevation_deg
            .to_radians()
            .clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2)
            .sin_cos();
        let dir = Vec3::new(sy * cp, -cy * cp, sp).normalize();
        // Keep world +Z up on screen; fall back to +Y when looking straight down.
        let world_up = if dir.z.abs() > 0.9999 {
            Vec3::Y
        } else {
            Vec3::Z
        };
        let right = world_up.cross(dir).normalize();
        let up = dir.cross(right).normalize();
        Self { dir, right, up }
    }

    /// (screen x, screen y with +y up, depth increasing toward the eye).
    fn project(&self, v: Vec3) -> (f32, f32, f32) {
        (v.dot(self.right), v.dot(self.up), v.dot(self.dir))
    }
}

fn included(s: &Surface, opt: &SvgOptions) -> bool {
    if opt.hide.contains(&s.stype) {
        return false;
    }
    if s.verts.len() < 3 || s.area <= 1e-9 {
        return false;
    }
    if let Some(re) = &opt.zone
        && !re.is_match(&s.zone)
    {
        return false;
    }
    if let Some(re) = &opt.name
        && !re.is_match(&s.name)
    {
        return false;
    }
    true
}

fn hex(c: [f32; 3]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

/// A projected surface, ready to sort and emit.
struct Face<'a> {
    s: &'a Surface,
    /// Screen-space polygon (+y up).
    pts: Vec<(f32, f32)>,
    /// Depth of the farthest and nearest vertex (larger = nearer the eye).
    min_d: f32,
    max_d: f32,
    /// Screen-space bounds: (min x, max x, min y, max y).
    bb: (f32, f32, f32, f32),
    /// Plane offset, so the plane is `normal · x = plane_d`.
    plane_d: f32,
    lum: f32,
    /// Sub-surfaces (windows, doors) hosted by this face, drawn right after it.
    subs: Vec<usize>,
}

/// Do all of `pts` lie on the side of the plane away from the eye?
/// Inconclusive (`false`) when the plane is edge-on to the view.
fn all_beyond(pts: &[Vec3], n: Vec3, d: f32, dir: Vec3, eps: f32, near: bool) -> bool {
    let eye_side = n.dot(dir);
    if eye_side.abs() < 1e-6 {
        return false;
    }
    let s = eye_side.signum() * if near { -1.0 } else { 1.0 };
    pts.iter().all(|p| (n.dot(*p) - d) * s <= eps)
}

/// Must `q` be painted before `p`, i.e. can `p` not be shown to lie behind it?
/// The four cheap escapes are Newell's: disjoint in depth, disjoint on screen,
/// `p` wholly beyond `q`'s plane, or `q` wholly in front of `p`'s plane.
/// Coplanar faces take the third escape, so their order stays as sorted.
fn must_precede(p: &Face, q: &Face, dir: Vec3, eps: f32) -> bool {
    if p.max_d <= q.min_d + eps {
        return false;
    }
    if p.bb.1 <= q.bb.0 || q.bb.1 <= p.bb.0 || p.bb.3 <= q.bb.2 || q.bb.3 <= p.bb.2 {
        return false;
    }
    if all_beyond(&p.s.verts, q.s.normal, q.plane_d, dir, eps, false) {
        return false;
    }
    if all_beyond(&q.s.verts, p.s.normal, p.plane_d, dir, eps, true) {
        return false;
    }
    true
}

/// Farthest-vertex first, then the renderer's coplanar tie-break (a window or
/// door beats its host wall, a floor beats the ceiling below it).
fn depth_cmp(a: &Face, b: &Face) -> std::cmp::Ordering {
    a.min_d
        .partial_cmp(&b.min_d)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(a.s.stype.depth_priority().cmp(&b.s.stype.depth_priority()))
}

/// Order faces back to front. Starts from a farthest-vertex sort, then applies
/// Newell's swap pass so that a face which genuinely occludes another is
/// painted after it — a plain depth sort gets this wrong whenever a large
/// polygon (a floor plate) reaches nearer the eye than the small ones covering
/// it (the roof above). Cyclic overlaps and a work cap fall back to the sort.
fn newell_order(faces: &[Face], mut list: Vec<usize>, dir: Vec3, eps: f32) -> Vec<usize> {
    let mut out = Vec::with_capacity(list.len());
    let mut deferred = vec![false; faces.len()];
    let mut budget: u64 = 4_000_000;
    list.reverse(); // pop() now yields the farthest face first

    while let Some(mut p) = list.pop() {
        'rescan: loop {
            for k in (0..list.len()).rev() {
                if budget == 0 {
                    out.push(p);
                    list.reverse();
                    out.extend(list);
                    return out;
                }
                budget -= 1;
                let q = list[k];
                if !must_precede(&faces[p], &faces[q], dir, eps) {
                    continue;
                }
                if deferred[q] {
                    continue; // cyclic overlap: accept the current order
                }
                deferred[q] = true;
                list.remove(k);
                list.push(p);
                p = q;
                continue 'rescan;
            }
            break;
        }
        out.push(p);
    }
    out
}

pub fn render(model: &Model, opt: &SvgOptions) -> String {
    let basis = Basis::new(opt.rotation, opt.elevation);

    // Faces to draw, with their projected polygons. `face_of` maps a model
    // surface index to its face so sub-surfaces can find their base.
    let mut faces: Vec<Face> = Vec::new();
    let mut face_of: Vec<Option<usize>> = vec![None; model.surfaces.len()];
    for (i, s) in model.surfaces.iter().enumerate() {
        if !included(s, opt) {
            continue;
        }
        let facing = s.normal.dot(basis.dir);
        if opt.cull && !s.stype.is_transparent() && facing <= 1e-4 {
            continue;
        }
        let mut pts = Vec::with_capacity(s.verts.len());
        let (mut min_d, mut max_d) = (f32::INFINITY, f32::NEG_INFINITY);
        let mut bb = (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        );
        for &v in &s.verts {
            let (x, y, d) = basis.project(v);
            pts.push((x, y));
            min_d = min_d.min(d);
            max_d = max_d.max(d);
            bb = (bb.0.min(x), bb.1.max(x), bb.2.min(y), bb.3.max(y));
        }
        face_of[i] = Some(faces.len());
        faces.push(Face {
            s,
            pts,
            min_d,
            max_d,
            bb,
            plane_d: s.normal.dot(s.centroid),
            lum: if opt.shade {
                0.45 + 0.55 * facing.abs()
            } else {
                1.0
            },
            subs: Vec::new(),
        });
    }

    if faces.is_empty() {
        return empty_svg(opt);
    }

    // Attach each drawn sub-surface to its base. A window is coplanar with its
    // wall, so no depth test can separate them; painting it immediately after
    // its host is the only ordering that always shows it.
    let mut roots: Vec<usize> = Vec::with_capacity(faces.len());
    for i in 0..faces.len() {
        match faces[i]
            .s
            .base_surface
            .and_then(|b| face_of.get(b).copied().flatten())
        {
            Some(base) if base != i => faces[base].subs.push(i),
            _ => roots.push(i),
        }
    }
    for i in 0..faces.len() {
        let mut subs = std::mem::take(&mut faces[i].subs);
        subs.sort_by(|&a, &b| depth_cmp(&faces[a], &faces[b]));
        faces[i].subs = subs;
    }

    // Painter's algorithm over the base surfaces, sub-surfaces riding along.
    roots.sort_by(|&a, &b| depth_cmp(&faces[a], &faces[b]));
    let eps = 1e-3;
    let order = newell_order(&faces, roots, basis.dir, eps);
    let order: Vec<usize> = order
        .into_iter()
        .flat_map(|i| std::iter::once(i).chain(faces[i].subs.iter().copied()))
        .collect();
    let faces: Vec<&Face> = order.into_iter().map(|i| &faces[i]).collect();

    // --- Fit ---------------------------------------------------------------
    let (mut min_x, mut min_y) = (f32::INFINITY, f32::INFINITY);
    let (mut max_x, mut max_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for f in &faces {
        for &(x, y) in &f.pts {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }
    let span_x = (max_x - min_x).max(1e-6);
    let span_y = (max_y - min_y).max(1e-6);

    let legend_types: Vec<SurfaceType> = if opt.legend {
        let mut t: Vec<SurfaceType> = SurfaceType::ALL
            .into_iter()
            .filter(|t| faces.iter().any(|f| f.s.stype == *t))
            .collect();
        t.sort();
        t
    } else {
        Vec::new()
    };
    let legend_h = if legend_types.is_empty() { 0.0 } else { 30.0 };

    let inner_w = (opt.width - 2.0 * opt.margin).max(1.0);
    let (scale, height) = match opt.height {
        Some(h) => {
            let inner_h = (h - 2.0 * opt.margin - legend_h).max(1.0);
            ((inner_w / span_x).min(inner_h / span_y), h)
        }
        None => {
            let s = inner_w / span_x;
            (s, span_y * s + 2.0 * opt.margin + legend_h)
        }
    };

    // Center the drawing in the canvas above the legend band.
    let draw_h = height - legend_h;
    let ox = (opt.width - span_x * scale) / 2.0 - min_x * scale;
    let oy = (draw_h - span_y * scale) / 2.0 + max_y * scale;
    let map = |(x, y): (f32, f32)| (ox + x * scale, oy - y * scale);

    // --- Emit --------------------------------------------------------------
    let mut out = String::with_capacity(faces.len() * 160 + 512);
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{:.0}" height="{:.0}" viewBox="0 0 {:.0} {:.0}">"#,
        opt.width, height, opt.width, height
    );
    if let Some(bg) = &opt.background {
        let _ = writeln!(
            out,
            r#"<rect width="100%" height="100%" fill="{}"/>"#,
            escape(bg)
        );
    }
    let _ = writeln!(
        out,
        r#"<g stroke-linejoin="round" stroke-width="{}">"#,
        trim(opt.stroke_width)
    );

    for f in &faces {
        let rgba = f.s.stype.color();
        let fill = hex([rgba[0] * f.lum, rgba[1] * f.lum, rgba[2] * f.lum]);
        // Outline: the same hue, darkened, so edges read without a heavy grid.
        let stroke = hex([rgba[0] * 0.45, rgba[1] * 0.45, rgba[2] * 0.45]);
        let mut d = String::with_capacity(f.pts.len() * 16);
        for (i, &p) in f.pts.iter().enumerate() {
            let (x, y) = map(p);
            let _ = write!(d, "{}{:.2} {:.2}", if i == 0 { "M" } else { "L" }, x, y);
            if i + 1 < f.pts.len() {
                d.push(' ');
            }
        }
        d.push('Z');
        let opacity = if rgba[3] < 0.999 {
            format!(r#" fill-opacity="{}""#, trim(rgba[3]))
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            r#"<path d="{d}" fill="{fill}"{opacity} stroke="{stroke}"/>"#
        );
    }
    let _ = writeln!(out, "</g>");

    if !legend_types.is_empty() {
        let y = height - legend_h + 8.0;
        let _ = writeln!(
            out,
            r##"<g font-family="sans-serif" font-size="12" fill="#333">"##
        );
        let mut x = opt.margin;
        for t in legend_types {
            let rgba = t.color();
            let _ = writeln!(
                out,
                r#"<rect x="{:.1}" y="{:.1}" width="12" height="12" fill="{}" fill-opacity="{}" stroke="{}" stroke-width="0.8"/><text x="{:.1}" y="{:.1}">{}</text>"#,
                x,
                y,
                hex([rgba[0], rgba[1], rgba[2]]),
                trim(rgba[3]),
                hex([rgba[0] * 0.45, rgba[1] * 0.45, rgba[2] * 0.45]),
                x + 17.0,
                y + 11.0,
                t.label()
            );
            x += 17.0 + t.label().len() as f32 * 7.2 + 14.0;
        }
        let _ = writeln!(out, "</g>");
    }

    let _ = writeln!(out, "</svg>");
    out
}

fn empty_svg(opt: &SvgOptions) -> String {
    let h = opt.height.unwrap_or(opt.width * 0.6);
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{:.0}\" height=\"{:.0}\" viewBox=\"0 0 {:.0} {:.0}\"></svg>\n",
        opt.width, h, opt.width, h
    )
}

/// Format a float without trailing zeros (keeps the markup tidy).
fn trim(v: f32) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".to_string()
    } else {
        s.to_string()
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// --- CLI round-trip --------------------------------------------------------

/// Single-quote an argument for a POSIX-ish shell if it needs it.
fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/=,+:@".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The `idf-visualizer svg` command line that reproduces `opt` for `path`,
/// omitting anything left at its default. Written by the viewer's "Copy CLI"
/// button so a lined-up view can be pasted into a build script.
pub fn cli_command(path: &str, opt: &SvgOptions, out: Option<&str>) -> String {
    let d = SvgOptions::default();
    let mut p: Vec<String> = vec!["idf-visualizer".into(), "svg".into(), shell_quote(path)];
    let mut arg = |flag: &str, val: String| {
        p.push(flag.to_string());
        p.push(val);
    };
    // Rotation and elevation are always emitted: they are the whole point of
    // copying a view, and a reader shouldn't have to know the defaults.
    arg("-r", trim(opt.rotation));
    arg("-e", trim(opt.elevation));
    if opt.width != d.width {
        arg("-w", trim(opt.width));
    }
    if let Some(h) = opt.height {
        arg("-H", trim(h));
    }
    if opt.margin != d.margin {
        arg("--margin", trim(opt.margin));
    }
    if opt.stroke_width != d.stroke_width {
        arg("--stroke-width", trim(opt.stroke_width));
    }
    if let Some(re) = &opt.zone {
        arg("--zone", shell_quote(re.as_str()));
    }
    if let Some(re) = &opt.name {
        arg("--name", shell_quote(re.as_str()));
    }
    if !opt.hide.is_empty() {
        let mut hide: Vec<SurfaceType> = opt.hide.clone();
        hide.sort();
        hide.dedup();
        let list: Vec<String> = hide.iter().map(|t| t.label().to_lowercase()).collect();
        arg("--hide", shell_quote(&list.join(",")));
    }
    if !opt.cull {
        p.push("--no-cull".into());
    }
    if !opt.shade {
        p.push("--flat".into());
    }
    if opt.legend {
        p.push("--legend".into());
    }
    if let Some(bg) = &opt.background {
        p.push("--background".into());
        p.push(shell_quote(bg));
    }
    if let Some(o) = out {
        p.push("-o".into());
        p.push(shell_quote(o));
    }
    p.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(stype: SurfaceType, verts: Vec<Vec3>, normal: Vec3) -> Surface {
        Surface {
            name: "s".into(),
            stype,
            class: String::new(),
            construction: String::new(),
            zone: "Z1".into(),
            space: String::new(),
            boundary: String::new(),
            boundary_object: String::new(),
            verts,
            tris: vec![0, 1, 2, 0, 2, 3],
            normal,
            centroid: Vec3::ZERO,
            area: 1.0,
            azimuth: 0.0,
            tilt: 90.0,
            raw: String::new(),
            line: 1,
            problems: Vec::new(),
            vert_flags: Vec::new(),
            base_surface: None,
        }
    }

    fn south_wall() -> Surface {
        quad(
            SurfaceType::Wall,
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(4.0, 0.0, 0.0),
                Vec3::new(4.0, 0.0, 3.0),
                Vec3::new(0.0, 0.0, 3.0),
            ],
            -Vec3::Y,
        )
    }

    fn north_wall() -> Surface {
        quad(
            SurfaceType::Wall,
            vec![
                Vec3::new(0.0, 5.0, 0.0),
                Vec3::new(4.0, 5.0, 0.0),
                Vec3::new(4.0, 5.0, 3.0),
                Vec3::new(0.0, 5.0, 3.0),
            ],
            Vec3::Y,
        )
    }

    /// Byte offset of the first path drawn for `stype`. Keys off the stroke,
    /// which is a fixed darkening of the type color and so is unaffected by
    /// the per-face shading applied to the fill.
    fn path_pos(svg: &str, stype: SurfaceType) -> usize {
        let c = stype.color();
        let stroke = format!(
            "stroke=\"{}\"",
            hex([c[0] * 0.45, c[1] * 0.45, c[2] * 0.45])
        );
        svg.find(&stroke)
            .unwrap_or_else(|| panic!("no {} path in {svg}", stype.label()))
    }

    #[test]
    fn small_roof_paints_over_the_wide_floor_below_it() {
        // The floor reaches nearer the eye than the roof patch above it, so a
        // sort on the nearest vertex would paint the floor last and hide it.
        let floor = quad(
            SurfaceType::Floor,
            vec![
                Vec3::new(0.0, -10.0, 0.0),
                Vec3::new(10.0, -10.0, 0.0),
                Vec3::new(10.0, 10.0, 0.0),
                Vec3::new(0.0, 10.0, 0.0),
            ],
            -Vec3::Z,
        );
        let roof = quad(
            SurfaceType::Roof,
            vec![
                Vec3::new(4.0, 4.0, 3.0),
                Vec3::new(6.0, 4.0, 3.0),
                Vec3::new(6.0, 6.0, 3.0),
                Vec3::new(4.0, 6.0, 3.0),
            ],
            Vec3::Z,
        );
        let model = Model {
            surfaces: vec![floor, roof],
            warnings: vec![],
        };
        let opt = SvgOptions {
            rotation: 0.0,
            elevation: 60.0,
            cull: false,
            ..Default::default()
        };
        let svg = render(&model, &opt);
        assert!(
            path_pos(&svg, SurfaceType::Floor) < path_pos(&svg, SurfaceType::Roof),
            "{svg}"
        );
    }

    #[test]
    fn window_paints_over_its_host_wall() {
        // Seen at an angle the wall's nearest corner is nearer than the small
        // window's, so only riding along with its base keeps the window visible.
        let wall = quad(
            SurfaceType::Wall,
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 3.0),
                Vec3::new(0.0, 0.0, 3.0),
            ],
            -Vec3::Y,
        );
        let mut win = quad(
            SurfaceType::Window,
            vec![
                Vec3::new(2.0, 0.0, 1.0),
                Vec3::new(3.0, 0.0, 1.0),
                Vec3::new(3.0, 0.0, 2.0),
                Vec3::new(2.0, 0.0, 2.0),
            ],
            -Vec3::Y,
        );
        win.base_surface = Some(0);
        let model = Model {
            surfaces: vec![wall, win],
            warnings: vec![],
        };
        let opt = SvgOptions {
            rotation: 30.0,
            elevation: 10.0,
            ..Default::default()
        };
        let svg = render(&model, &opt);
        assert!(
            path_pos(&svg, SurfaceType::Wall) < path_pos(&svg, SurfaceType::Window),
            "{svg}"
        );
    }

    #[test]
    fn cli_command_omits_defaults_but_keeps_angles() {
        let cmd = cli_command("model.idf", &SvgOptions::default(), None);
        assert_eq!(cmd, "idf-visualizer svg model.idf -r 45 -e 35.264");
    }

    #[test]
    fn cli_command_round_trips_view_and_filters() {
        let opt = SvgOptions {
            rotation: 137.5,
            elevation: 20.0,
            width: 600.0,
            legend: true,
            cull: false,
            zone: Some(Regex::new("^Zone 1$").unwrap()),
            name: Some(Regex::new("^rtu-4").unwrap()),
            hide: vec![SurfaceType::Ceiling, SurfaceType::Roof],
            ..Default::default()
        };
        assert_eq!(
            cli_command("my models/a.idf", &opt, Some("a.svg")),
            "idf-visualizer svg 'my models/a.idf' -r 137.5 -e 20 -w 600 \
             --zone '^Zone 1$' --name '^rtu-4' --hide ceiling,roof --no-cull --legend -o a.svg"
        );
    }

    #[test]
    fn cli_command_quotes_awkward_paths() {
        assert!(
            cli_command("it's a.idf", &SvgOptions::default(), None)
                .starts_with(r"idf-visualizer svg 'it'\''s a.idf'")
        );
    }

    #[test]
    fn basis_is_right_handed_and_z_up() {
        let b = Basis::new(0.0, 0.0);
        assert!((b.dir - -Vec3::Y).length() < 1e-5);
        assert!((b.right - Vec3::X).length() < 1e-5);
        assert!((b.up - Vec3::Z).length() < 1e-5);
    }

    #[test]
    fn culls_back_faces_but_keeps_front() {
        let model = Model {
            surfaces: vec![south_wall(), north_wall()],
            warnings: vec![],
        };
        let opt = SvgOptions {
            rotation: 0.0,
            elevation: 0.0,
            ..Default::default()
        };
        // Viewed from the south, only the south wall faces the camera.
        assert_eq!(render(&model, &opt).matches("<path").count(), 1);
        let no_cull = SvgOptions { cull: false, ..opt };
        assert_eq!(render(&model, &no_cull).matches("<path").count(), 2);
    }

    #[test]
    fn rotation_changes_which_face_is_visible() {
        let model = Model {
            surfaces: vec![north_wall()],
            warnings: vec![],
        };
        let from_south = SvgOptions {
            rotation: 0.0,
            elevation: 0.0,
            ..Default::default()
        };
        assert_eq!(render(&model, &from_south).matches("<path").count(), 0);
        let from_north = SvgOptions {
            rotation: 180.0,
            ..from_south
        };
        assert_eq!(render(&model, &from_north).matches("<path").count(), 1);
    }

    #[test]
    fn fits_content_width_and_derives_height() {
        let model = Model {
            surfaces: vec![south_wall()],
            warnings: vec![],
        };
        let opt = SvgOptions {
            rotation: 0.0,
            elevation: 0.0,
            width: 400.0,
            margin: 20.0,
            legend: false,
            ..Default::default()
        };
        let svg = render(&model, &opt);
        // 4 m x 3 m wall in a 360 px inner width -> 270 px tall plus margins.
        assert!(svg.contains(r#"width="400" height="310""#), "{svg}");
        // Content spans the full inner width.
        assert!(svg.contains("M20.00"), "{svg}");
    }

    #[test]
    fn zone_and_type_filters_apply() {
        let mut roof = south_wall();
        roof.stype = SurfaceType::Roof;
        roof.zone = "Z2".into();
        let model = Model {
            surfaces: vec![south_wall(), roof],
            warnings: vec![],
        };
        let opt = SvgOptions {
            rotation: 0.0,
            elevation: 0.0,
            cull: false,
            zone: Some(Regex::new("(?i)^z1$").unwrap()),
            ..Default::default()
        };
        assert_eq!(render(&model, &opt).matches("<path").count(), 1);
        let opt = SvgOptions {
            zone: None,
            hide: vec![SurfaceType::Roof],
            ..opt
        };
        assert_eq!(render(&model, &opt).matches("<path").count(), 1);
    }

    #[test]
    fn windows_draw_after_their_wall_at_equal_depth() {
        let mut win = south_wall();
        win.stype = SurfaceType::Window;
        let model = Model {
            surfaces: vec![win, south_wall()],
            warnings: vec![],
        };
        let opt = SvgOptions {
            rotation: 0.0,
            elevation: 0.0,
            ..Default::default()
        };
        let svg = render(&model, &opt);
        let wall = hex([
            SurfaceType::Wall.color()[0] * 1.0,
            SurfaceType::Wall.color()[1],
            SurfaceType::Wall.color()[2],
        ]);
        let win_fill = hex([
            SurfaceType::Window.color()[0],
            SurfaceType::Window.color()[1],
            SurfaceType::Window.color()[2],
        ]);
        assert!(
            svg.find(&wall).unwrap() < svg.rfind(&win_fill).unwrap(),
            "{svg}"
        );
    }
}
