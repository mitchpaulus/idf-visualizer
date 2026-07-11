//! egui application: viewport, side panels, selection, filtering, demo mode.

use crate::camera::{ray_triangle, OrbitCamera};
use crate::model::{Model, ProblemKind, Severity, Surface, SurfaceType};
use crate::scene::{self, SceneRenderer, Uniforms, Vertex, ViewCallback};
use eframe::{egui, egui_wgpu};
use egui::{Color32, RichText};
use glam::Vec3;
use std::path::PathBuf;

const OVERLAY_ID: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorMode {
    Type,
    Zone,
    Boundary,
    Problems,
}

impl ColorMode {
    const ALL: [ColorMode; 4] = [
        ColorMode::Type,
        ColorMode::Zone,
        ColorMode::Boundary,
        ColorMode::Problems,
    ];

    fn label(self) -> &'static str {
        match self {
            ColorMode::Type => "Surface type",
            ColorMode::Zone => "Zone",
            ColorMode::Boundary => "Boundary",
            ColorMode::Problems => "Problems",
        }
    }
}

fn severity_color(sev: Severity) -> Color32 {
    match sev {
        Severity::Error => Color32::from_rgb(220, 70, 70),
        Severity::Warning => Color32::from_rgb(235, 185, 50),
        Severity::Info => Color32::from_rgb(100, 150, 220),
    }
}

fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let f = h - h.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match h as i32 % 6 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

fn boundary_color(boundary: &str) -> [f32; 3] {
    let b = boundary.to_ascii_lowercase();
    if b.is_empty() {
        [0.55, 0.55, 0.55]
    } else if b == "outdoors" {
        [0.35, 0.62, 0.90]
    } else if b.starts_with("ground") {
        [0.58, 0.44, 0.28]
    } else if b == "surface" {
        [0.42, 0.72, 0.42]
    } else if b == "zone" {
        [0.30, 0.68, 0.68]
    } else if b == "adiabatic" {
        [0.80, 0.45, 0.80]
    } else {
        [0.90, 0.60, 0.30]
    }
}

pub struct App {
    model: Model,
    file_name: String,
    camera: OrbitCamera,
    type_visible: std::collections::BTreeMap<SurfaceType, bool>,
    type_counts: std::collections::BTreeMap<SurfaceType, usize>,
    /// Unique zone names, sorted. `None` filter = all zones.
    zones: Vec<String>,
    zone_filter: Option<String>,
    color_mode: ColorMode,
    colors_dirty: bool,
    /// When set, only surfaces with an enabled problem kind are shown.
    problem_only: bool,
    problem_kind_enabled: std::collections::BTreeMap<ProblemKind, bool>,
    /// (kind, number of surfaces with it), for kinds present in the model.
    problem_counts: Vec<(ProblemKind, usize)>,
    regex_text: String,
    regex: Option<regex::Regex>,
    regex_error: Option<String>,
    visible: Vec<bool>,
    visibility_dirty: bool,
    selected: Option<usize>,
    scene_min: Vec3,
    scene_max: Vec3,
    last_viewport: egui::Rect,
    demo: Option<Demo>,
}

struct Demo {
    dir: PathBuf,
    frame: u32,
    pending_shots: usize,
    closing: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, model: Model, file_name: String, demo_dir: Option<PathBuf>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("wgpu render state (eframe must use the wgpu backend)");

        let (verts, edge_verts, per_surface) = scene::build_mesh(&model);
        let mut scene_renderer = SceneRenderer::new(
            &rs.device,
            rs.target_format,
            egui_wgpu::wgpu::TextureFormat::Depth32Float,
            4,
            &verts,
            &edge_verts,
            per_surface,
        );
        let n = model.surfaces.len();
        scene_renderer.set_visibility(&rs.queue, &vec![true; n]);
        rs.renderer
            .write()
            .callback_resources
            .insert(scene_renderer);

        let mut scene_min = Vec3::splat(f32::MAX);
        let mut scene_max = Vec3::splat(f32::MIN);
        for s in &model.surfaces {
            for v in &s.verts {
                scene_min = scene_min.min(*v);
                scene_max = scene_max.max(*v);
            }
        }
        if n == 0 {
            scene_min = Vec3::ZERO;
            scene_max = Vec3::ONE;
        }

        let mut type_counts = std::collections::BTreeMap::new();
        for s in &model.surfaces {
            *type_counts.entry(s.stype).or_insert(0) += 1;
        }
        let type_visible = SurfaceType::ALL.iter().map(|&t| (t, true)).collect();

        let mut zones: Vec<String> = model
            .surfaces
            .iter()
            .filter(|s| !s.zone.is_empty())
            .map(|s| s.zone.clone())
            .collect();
        zones.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
        zones.dedup_by(|a, b| a.eq_ignore_ascii_case(b));

        let mut kind_counts: std::collections::BTreeMap<ProblemKind, usize> =
            std::collections::BTreeMap::new();
        for s in &model.surfaces {
            let mut kinds: Vec<ProblemKind> = s.problems.iter().map(|p| p.kind).collect();
            kinds.sort();
            kinds.dedup();
            for k in kinds {
                *kind_counts.entry(k).or_insert(0) += 1;
            }
        }
        let problem_kind_enabled = kind_counts.keys().map(|&k| (k, true)).collect();
        let problem_counts: Vec<(ProblemKind, usize)> = kind_counts.into_iter().collect();

        let mut camera = OrbitCamera::default();
        camera.fit(scene_min, scene_max);

        Self {
            visible: vec![true; n],
            model,
            file_name,
            camera,
            type_visible,
            type_counts,
            zones,
            zone_filter: None,
            color_mode: ColorMode::Type,
            colors_dirty: false,
            problem_only: false,
            problem_kind_enabled,
            problem_counts,
            regex_text: String::new(),
            regex: None,
            regex_error: None,
            visibility_dirty: false,
            selected: None,
            scene_min,
            scene_max,
            last_viewport: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)),
            demo: demo_dir.map(|dir| Demo {
                dir,
                frame: 0,
                pending_shots: 0,
                closing: false,
            }),
        }
    }

    fn scene_size(&self) -> f32 {
        (self.scene_max - self.scene_min).length().max(1.0)
    }

    fn has_enabled_problem(&self, s: &Surface) -> bool {
        s.problems
            .iter()
            .any(|p| self.problem_kind_enabled.get(&p.kind).copied().unwrap_or(true))
    }

    /// Cycle the selection through visible surfaces with (enabled) problems.
    fn select_next_problem(&mut self) {
        let mut flagged = Vec::new();
        for (i, (s, &vis)) in self.model.surfaces.iter().zip(&self.visible).enumerate() {
            if vis
                && s.problems
                    .iter()
                    .any(|p| self.problem_kind_enabled.get(&p.kind).copied().unwrap_or(true))
            {
                flagged.push(i);
            }
        }
        let Some(&first) = flagged.first() else { return };
        let next = match self.selected {
            Some(cur) => flagged.iter().copied().find(|&i| i > cur).unwrap_or(first),
            None => first,
        };
        self.select(Some(next));
    }

    /// One color per surface for the current color mode.
    fn surface_colors(&self) -> Vec<[f32; 4]> {
        self.model
            .surfaces
            .iter()
            .map(|s| {
                let alpha = s.stype.color()[3];
                let rgb = match self.color_mode {
                    ColorMode::Type => {
                        let c = s.stype.color();
                        [c[0], c[1], c[2]]
                    }
                    ColorMode::Zone => {
                        if s.zone.is_empty() {
                            [0.55, 0.55, 0.55]
                        } else {
                            let idx = self
                                .zones
                                .iter()
                                .position(|z| z.eq_ignore_ascii_case(&s.zone))
                                .unwrap_or(0);
                            hsv(idx as f32 * 0.618_034, 0.55, 0.85)
                        }
                    }
                    ColorMode::Boundary => {
                        // Sub-surfaces inherit their base surface's boundary.
                        let b = match s.base_surface {
                            Some(bi) => &self.model.surfaces[bi].boundary,
                            None => &s.boundary,
                        };
                        boundary_color(b)
                    }
                    ColorMode::Problems => {
                        match s.problems.iter().map(|p| p.severity).max() {
                            Some(Severity::Error) => [0.86, 0.25, 0.25],
                            Some(Severity::Warning) => [0.92, 0.72, 0.18],
                            Some(Severity::Info) => [0.36, 0.56, 0.85],
                            None => [0.60, 0.60, 0.60],
                        }
                    }
                };
                [rgb[0], rgb[1], rgb[2], alpha]
            })
            .collect()
    }

    fn surface_visible(&self, s: &Surface) -> bool {
        if !self.type_visible.get(&s.stype).copied().unwrap_or(true) {
            return false;
        }
        if let Some(zone) = &self.zone_filter {
            if !s.zone.eq_ignore_ascii_case(zone) {
                return false;
            }
        }
        if self.problem_only && !self.has_enabled_problem(s) {
            return false;
        }
        match &self.regex {
            Some(re) => re.is_match(&s.name),
            None => true,
        }
    }

    fn recompute_visibility(&mut self) {
        self.visible = self
            .model
            .surfaces
            .iter()
            .map(|s| self.surface_visible(s))
            .collect();
        self.visibility_dirty = true;
        if let Some(sel) = self.selected {
            if !self.visible[sel] {
                self.selected = None;
            }
        }
    }

    fn set_regex(&mut self, text: String) {
        self.regex_text = text;
        if self.regex_text.trim().is_empty() {
            self.regex = None;
            self.regex_error = None;
        } else {
            match regex::RegexBuilder::new(&self.regex_text)
                .case_insensitive(true)
                .build()
            {
                Ok(re) => {
                    self.regex = Some(re);
                    self.regex_error = None;
                }
                Err(e) => {
                    self.regex = None;
                    self.regex_error = Some(e.to_string());
                }
            }
        }
        self.recompute_visibility();
    }

    fn visible_bbox(&self) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        let mut any = false;
        for (s, &vis) in self.model.surfaces.iter().zip(&self.visible) {
            if !vis {
                continue;
            }
            any = true;
            for v in &s.verts {
                min = min.min(*v);
                max = max.max(*v);
            }
        }
        if any {
            (min, max)
        } else {
            (self.scene_min, self.scene_max)
        }
    }

    fn zoom_to_fit(&mut self) {
        let (min, max) = self.visible_bbox();
        self.camera.fit(min, max);
    }

    /// Pick the nearest visible surface under a viewport position (in points).
    fn pick(&self, pos: egui::Pos2, rect: egui::Rect) -> Option<usize> {
        let ndc_x = 2.0 * (pos.x - rect.left()) / rect.width() - 1.0;
        let ndc_y = 1.0 - 2.0 * (pos.y - rect.top()) / rect.height();
        let (orig, dir) = self.camera.ray(ndc_x, ndc_y, rect.aspect_ratio());
        // Near-tie epsilon so coplanar overlaps pick the same surface the
        // renderer shows in front (see SurfaceType::depth_priority).
        const TIE_EPS: f32 = 1e-3;
        let mut best: Option<(f32, u32, usize)> = None;
        for (i, (s, &vis)) in self.model.surfaces.iter().zip(&self.visible).enumerate() {
            if !vis {
                continue;
            }
            let pri = s.stype.depth_priority();
            for tri in s.tris.chunks_exact(3) {
                let (a, b, c) = (
                    s.verts[tri[0] as usize],
                    s.verts[tri[1] as usize],
                    s.verts[tri[2] as usize],
                );
                if let Some(t) = ray_triangle(orig, dir, a, b, c) {
                    let better = match best {
                        None => true,
                        Some((bt, bp, _)) => {
                            t < bt - TIE_EPS || ((t - bt).abs() <= TIE_EPS && pri > bp)
                        }
                    };
                    if better {
                        best = Some((t, pri, i));
                    }
                }
            }
        }
        best.map(|(_, _, i)| i)
    }

    /// Overlay line list: world axes + normal arrow for the selection.
    fn overlay(&self) -> Vec<Vertex> {
        let mut out = Vec::new();
        let mut line = |a: Vec3, b: Vec3, color: [f32; 4]| {
            for p in [a, b] {
                out.push(Vertex {
                    pos: p.into(),
                    normal: [0.0, 0.0, 1.0],
                    color,
                    id: OVERLAY_ID,
                    priority: 0,
                });
            }
        };

        let axis_len = (self.scene_size() * 0.08).clamp(1.0, 25.0);
        let o = Vec3::ZERO;
        line(o, o + Vec3::X * axis_len, [0.9, 0.2, 0.2, 1.0]);
        line(o, o + Vec3::Y * axis_len, [0.2, 0.8, 0.2, 1.0]);
        line(o, o + Vec3::Z * axis_len, [0.25, 0.45, 1.0, 1.0]);

        if let Some(sel) = self.selected {
            let s = &self.model.surfaces[sel];
            let len = ((s.area as f32).sqrt() * 0.9).clamp(1.0, self.scene_size() * 0.25);
            let n = s.normal;
            let tip = s.centroid + n * len;
            let magenta = [1.0, 0.15, 0.9, 1.0];
            line(s.centroid, tip, magenta);
            // Arrowhead: 4 short lines back from the tip.
            let up = if n.z.abs() > 0.99 { Vec3::Y } else { Vec3::Z };
            let u = up.cross(n).normalize();
            let v = n.cross(u).normalize();
            let back = tip - n * (len * 0.15);
            for d in [u, -u, v, -v] {
                line(tip, back + d * (len * 0.06), magenta);
            }
        }
        out
    }

    fn select(&mut self, idx: Option<usize>) {
        self.selected = idx;
    }

    // --- UI pieces ----------------------------------------------------------

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("IDF Visualizer");
        ui.label(RichText::new(&self.file_name).weak());
        let shown = self.visible.iter().filter(|&&v| v).count();
        ui.label(format!(
            "{shown} of {} surfaces shown",
            self.model.surfaces.len()
        ));
        if ui.button("Zoom to fit  (F)").clicked() {
            self.zoom_to_fit();
        }
        ui.horizontal(|ui| {
            ui.label("Color by");
            egui::ComboBox::from_id_salt("color_mode")
                .selected_text(self.color_mode.label())
                .show_ui(ui, |ui| {
                    for m in ColorMode::ALL {
                        if ui.selectable_value(&mut self.color_mode, m, m.label()).changed() {
                            self.colors_dirty = true;
                        }
                    }
                });
        });
        ui.separator();

        ui.strong("Surface types");
        let mut changed = false;
        for &t in SurfaceType::ALL.iter() {
            let count = self.type_counts.get(&t).copied().unwrap_or(0);
            if count == 0 {
                continue;
            }
            ui.horizontal(|ui| {
                let c = t.color();
                let swatch = Color32::from_rgb(
                    (c[0] * 255.0) as u8,
                    (c[1] * 255.0) as u8,
                    (c[2] * 255.0) as u8,
                );
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 3.0, swatch);
                let vis = self.type_visible.get_mut(&t).unwrap();
                if ui.checkbox(vis, format!("{} ({count})", t.label())).changed() {
                    changed = true;
                }
            });
        }
        ui.separator();

        if !self.zones.is_empty() {
            ui.strong("Zone");
            let zones = self.zones.clone();
            egui::ComboBox::from_id_salt("zone_filter")
                .width(ui.available_width())
                .selected_text(self.zone_filter.as_deref().unwrap_or("All zones"))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut self.zone_filter, None, "All zones")
                        .changed()
                    {
                        changed = true;
                    }
                    for z in zones {
                        let label = z.clone();
                        if ui
                            .selectable_value(&mut self.zone_filter, Some(z), label)
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });
            ui.separator();
        }

        ui.strong("Filter (regex, case-insensitive)");
        let mut text = self.regex_text.clone();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut text)
                .hint_text("e.g. ^rtu-4 .*wall")
                .desired_width(f32::INFINITY),
        );
        if resp.changed() {
            self.set_regex(text);
        }
        if let Some(err) = &self.regex_error {
            ui.label(RichText::new(err).color(Color32::LIGHT_RED).small());
        }
        ui.separator();

        if !self.problem_counts.is_empty() {
            ui.strong("Problems");
            if ui
                .checkbox(&mut self.problem_only, "Show only flagged surfaces")
                .changed()
            {
                changed = true;
            }
            let counts = self.problem_counts.clone();
            for (kind, count) in counts {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(
                        rect.center(),
                        4.0,
                        severity_color(kind.default_severity()),
                    );
                    let enabled = self.problem_kind_enabled.get_mut(&kind).unwrap();
                    if ui
                        .checkbox(enabled, format!("{} ({count})", kind.label()))
                        .changed()
                    {
                        changed = true;
                    }
                });
            }
            let flagged: Vec<(usize, String)> = self
                .model
                .surfaces
                .iter()
                .enumerate()
                .filter(|(_, s)| self.has_enabled_problem(s))
                .map(|(i, s)| (i, s.name.clone()))
                .collect();
            egui::CollapsingHeader::new(
                RichText::new(format!("⚠ Flagged surfaces ({})  ·  N: next", flagged.len()))
                    .color(Color32::YELLOW),
            )
            .show(ui, |ui| {
                for (i, name) in flagged {
                    if ui.link(&name).clicked() {
                        self.select(Some(i));
                    }
                }
            });
            ui.separator();
        }

        if !self.model.warnings.is_empty() {
            egui::CollapsingHeader::new(
                RichText::new(format!("Model warnings ({})", self.model.warnings.len()))
                    .color(Color32::YELLOW),
            )
            .show(ui, |ui| {
                for w in &self.model.warnings {
                    ui.label(w);
                }
            });
            ui.separator();
        }

        ui.strong("Surfaces");
        let row_height = ui.text_style_height(&egui::TextStyle::Body);
        let indices: Vec<usize> = (0..self.model.surfaces.len())
            .filter(|&i| self.visible[i])
            .collect();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_height, indices.len(), |ui, range| {
                for &i in &indices[range] {
                    let s = &self.model.surfaces[i];
                    let is_sel = self.selected == Some(i);
                    let label = format!("{}  [{}]", s.name, s.stype.label());
                    if ui.selectable_label(is_sel, label).clicked() {
                        self.select(if is_sel { None } else { Some(i) });
                    }
                }
            });

        if changed {
            self.recompute_visibility();
        }
    }

    fn properties_panel(&mut self, ui: &mut egui::Ui, sel: usize) {
        let s = self.model.surfaces[sel].clone();
        ui.heading("Surface properties");
        ui.separator();
        egui::Grid::new("props")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.label(RichText::new(k).strong());
                    ui.label(v);
                    ui.end_row();
                };
                row("Name", s.name.clone());
                row("Class", s.class.clone());
                row("Type", s.stype.label().to_string());
                if !s.construction.is_empty() {
                    row("Construction", s.construction.clone());
                }
                if !s.zone.is_empty() {
                    row("Zone", s.zone.clone());
                }
                if !s.space.is_empty() {
                    row("Space", s.space.clone());
                }
                if !s.boundary.is_empty() {
                    row("Boundary", s.boundary.clone());
                }
                if !s.boundary_object.is_empty() {
                    row("Boundary object", s.boundary_object.clone());
                }
                row(
                    "Area",
                    format!("{:.2} m²  ({:.1} ft²)", s.area, s.area * 10.7639),
                );
                row("Azimuth", format!("{:.1}°  ({})", s.azimuth, cardinal(s.azimuth)));
                row("Tilt", format!("{:.1}°", s.tilt));
                row(
                    "Normal",
                    format!("({:.3}, {:.3}, {:.3})", s.normal.x, s.normal.y, s.normal.z),
                );
                row("Vertices", format!("{}", s.verts.len()));
                row("IDF line", format!("{}", s.line));
            });

        for p in &s.problems {
            ui.label(
                RichText::new(format!("⚠ {}: {}", p.kind.label(), p.message))
                    .color(severity_color(p.severity)),
            );
        }

        ui.horizontal(|ui| {
            if ui.button("Zoom to surface").clicked() {
                let mut min = Vec3::splat(f32::MAX);
                let mut max = Vec3::splat(f32::MIN);
                for v in &s.verts {
                    min = min.min(*v);
                    max = max.max(*v);
                }
                self.camera.fit(min, max);
            }
            if ui.button("Deselect  (Esc)").clicked() {
                self.select(None);
            }
        });

        egui::CollapsingHeader::new("Vertices (world, m)")
            .default_open(false)
            .show(ui, |ui| {
                for (i, (v, f)) in s.verts.iter().zip(&s.vert_flags).enumerate() {
                    let mut line = format!("{}: ({:.4}, {:.4}, {:.4})", i + 1, v.x, v.y, v.z);
                    if f.duplicate {
                        line.push_str("  · duplicate");
                    }
                    if f.collinear {
                        line.push_str("  · collinear");
                    } else if f.near_collinear {
                        line.push_str("  · nearly collinear");
                    }
                    if f.plane_dev.abs() > 0.005 {
                        line.push_str(&format!("  · {:+.3} m off plane", f.plane_dev));
                    }
                    let flagged = f.duplicate
                        || f.collinear
                        || f.near_collinear
                        || f.plane_dev.abs() > 0.005;
                    let text = RichText::new(line).monospace();
                    ui.label(if flagged {
                        text.color(Color32::from_rgb(240, 200, 60))
                    } else {
                        text
                    });
                }
            });

        egui::CollapsingHeader::new("Raw IDF")
            .default_open(true)
            .show(ui, |ui| {
                egui::ScrollArea::both().max_height(400.0).show(ui, |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&s.raw).monospace())
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                });
            });
    }

    fn viewport(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        self.last_viewport = rect;

        // --- camera input ---
        let shift = ui.input(|i| i.modifiers.shift);
        let d = response.drag_delta();
        if response.dragged_by(egui::PointerButton::Primary) && !shift {
            self.camera.orbit(d.x, d.y);
        } else if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged_by(egui::PointerButton::Primary) && shift)
        {
            self.camera.pan(d.x, d.y, rect.height());
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                self.camera.zoom(scroll);
            }
        }
        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let hit = self.pick(pos, rect);
                self.select(hit);
            }
        }

        // --- draw ---
        let aspect = rect.aspect_ratio();
        let eye = self.camera.eye();
        // Per-priority depth bias: k * near in clip space = a pull toward the
        // camera of ~k * view distance (see vs_main in scene.rs).
        let depth_bias = 1e-3 * self.camera.near();
        let uniforms = Uniforms {
            view_proj: self.camera.view_proj(aspect).to_cols_array_2d(),
            eye: [eye.x, eye.y, eye.z, depth_bias],
            misc: [self.selected.map_or(0, |i| i as u32 + 1), 0, 0, 0],
        };
        let visibility = self.visibility_dirty.then(|| self.visible.clone());
        self.visibility_dirty = false;
        let colors = self.colors_dirty.then(|| self.surface_colors());
        self.colors_dirty = false;
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewCallback {
                uniforms,
                overlay: self.overlay(),
                visibility,
                colors,
            },
        ));

        self.draw_vertex_dots(ui, rect, aspect);

        // Hint text overlay.
        ui.painter().text(
            rect.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            "drag: orbit · shift-drag / right-drag: pan · scroll: zoom · click: select · F: fit · N: next problem",
            egui::FontId::proportional(12.0),
            Color32::from_rgba_unmultiplied(255, 255, 255, 140),
        );
    }

    /// Numbered vertex dots + winding-direction arrow for the selected surface.
    /// Colors: white = ok, yellow = collinear, orange = nearly collinear,
    /// red = duplicate; the first vertex gets a magenta ring.
    fn draw_vertex_dots(&self, ui: &egui::Ui, rect: egui::Rect, aspect: f32) {
        let Some(sel) = self.selected else { return };
        let s = &self.model.surfaces[sel];
        let vp = self.camera.view_proj(aspect);
        let to_screen = |v: Vec3| -> Option<egui::Pos2> {
            let clip = vp * v.extend(1.0);
            if clip.w <= 0.0 {
                return None;
            }
            let ndc = clip.truncate() / clip.w;
            if !(0.0..=1.0).contains(&ndc.z) {
                return None;
            }
            Some(egui::pos2(
                rect.left() + (ndc.x + 1.0) * 0.5 * rect.width(),
                rect.top() + (1.0 - ndc.y) * 0.5 * rect.height(),
            ))
        };
        let painter = ui.painter().with_clip_rect(rect);
        let magenta = Color32::from_rgb(255, 38, 230);

        if s.verts.len() >= 2 {
            if let (Some(a), Some(b)) = (to_screen(s.verts[0]), to_screen(s.verts[1])) {
                painter.arrow(a, (b - a) * 0.45, egui::Stroke::new(2.0, magenta));
            }
        }
        for (i, (v, f)) in s.verts.iter().zip(&s.vert_flags).enumerate() {
            let Some(p) = to_screen(*v) else { continue };
            let (fill, r) = if f.duplicate {
                (Color32::from_rgb(235, 70, 70), 4.5)
            } else if f.collinear {
                (Color32::from_rgb(245, 205, 40), 4.5)
            } else if f.near_collinear {
                (Color32::from_rgb(240, 150, 60), 4.0)
            } else {
                (Color32::WHITE, 3.0)
            };
            painter.circle_filled(p, r, fill);
            painter.circle_stroke(p, r, egui::Stroke::new(1.0, Color32::from_black_alpha(160)));
            if i == 0 {
                painter.circle_stroke(p, r + 3.0, egui::Stroke::new(1.5, magenta));
            }
            painter.text(
                p + egui::vec2(6.0, -4.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{}", i + 1),
                egui::FontId::proportional(11.0),
                Color32::from_rgba_unmultiplied(255, 255, 255, 220),
            );
        }
    }

    // --- demo mode ----------------------------------------------------------

    fn run_demo(&mut self, ctx: &egui::Context) {
        // Save any screenshots that arrived.
        let shots: Vec<(String, std::sync::Arc<egui::ColorImage>)> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot {
                        user_data, image, ..
                    } => user_data
                        .data
                        .as_ref()
                        .and_then(|d| d.downcast_ref::<String>().cloned())
                        .map(|name| (name, image.clone())),
                    _ => None,
                })
                .collect()
        });
        let Some(demo) = &mut self.demo else { return };
        let dir = demo.dir.clone();
        let got_shots = !shots.is_empty();
        for (name, img) in shots {
            let path = dir.join(&name);
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Err(e) = image::save_buffer(
                &path,
                &bytes,
                img.size[0] as u32,
                img.size[1] as u32,
                image::ExtendedColorType::Rgba8,
            ) {
                eprintln!("demo: failed to save {}: {e}", path.display());
            } else {
                println!("demo: saved {}", path.display());
            }
            demo.pending_shots = demo.pending_shots.saturating_sub(1);
        }
        if got_shots {
            // reborrow dance done above; nothing else
        }

        let Some(demo) = &mut self.demo else { return };
        demo.frame += 1;
        let frame = demo.frame;

        let shoot = |demo: &mut Demo, ctx: &egui::Context, name: &str| {
            demo.pending_shots += 1;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                name.to_string(),
            )));
        };

        match frame {
            5 => self.zoom_to_fit(),
            10 => {
                let d = self.demo.as_mut().unwrap();
                shoot(d, ctx, "01-overview.png");
            }
            15 => {
                // Select whatever is at the viewport center, using the real picker.
                let rect = self.last_viewport;
                let hit = self.pick(rect.center(), rect);
                self.select(hit);
            }
            20 => {
                let d = self.demo.as_mut().unwrap();
                shoot(d, ctx, "02-selection-normal.png");
            }
            25 => self.set_regex("^rtu-4".to_string()),
            27 => self.zoom_to_fit(),
            32 => {
                let d = self.demo.as_mut().unwrap();
                shoot(d, ctx, "03-regex-filter.png");
            }
            37 => {
                self.set_regex(String::new());
                *self.type_visible.get_mut(&SurfaceType::Roof).unwrap() = false;
                *self.type_visible.get_mut(&SurfaceType::Ceiling).unwrap() = false;
                self.recompute_visibility();
                self.camera.pitch = 1.2;
                self.zoom_to_fit();
            }
            42 => {
                let d = self.demo.as_mut().unwrap();
                shoot(d, ctx, "04-hidden-roof-ceiling.png");
            }
            45 => {
                // Window close-up: restore everything, select the first window,
                // zoom to it and pull back for context.
                for v in self.type_visible.values_mut() {
                    *v = true;
                }
                self.recompute_visibility();
                if let Some(i) = self
                    .model
                    .surfaces
                    .iter()
                    .position(|s| s.stype == SurfaceType::Window)
                {
                    self.select(Some(i));
                    let s = &self.model.surfaces[i];
                    let mut min = Vec3::splat(f32::MAX);
                    let mut max = Vec3::splat(f32::MIN);
                    for v in &s.verts {
                        min = min.min(*v);
                        max = max.max(*v);
                    }
                    self.camera.fit(min, max);
                    self.camera.distance *= 3.0;
                    self.camera.pitch = 0.35;
                    // Face the window: look from the direction of its normal.
                    self.camera.yaw = s.normal.x.atan2(-s.normal.y);
                }
            }
            50 => {
                let d = self.demo.as_mut().unwrap();
                shoot(d, ctx, "05-window-closeup.png");
            }
            53 => {
                self.select(None);
                self.color_mode = ColorMode::Problems;
                self.colors_dirty = true;
                self.camera.pitch = 0.5;
                self.zoom_to_fit();
            }
            58 => {
                let d = self.demo.as_mut().unwrap();
                shoot(d, ctx, "06-color-problems.png");
            }
            61.. => {
                let d = self.demo.as_mut().unwrap();
                if d.pending_shots == 0 && !d.closing {
                    d.closing = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            _ => {}
        }
        ctx.request_repaint();
    }
}

fn cardinal(azimuth: f64) -> &'static str {
    const DIRS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    DIRS[(((azimuth + 22.5) / 45.0) as usize) % 8]
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.demo.is_some() {
            self.run_demo(&ctx);
        }

        // Hotkeys are disabled while a text field (e.g. the regex filter) has focus.
        let typing = ctx.egui_wants_keyboard_input();
        ctx.input(|i| {
            if typing {
                return;
            }
            if i.key_pressed(egui::Key::F) {
                self.zoom_to_fit();
            }
            if i.key_pressed(egui::Key::Escape) {
                self.selected = None;
            }
            if i.key_pressed(egui::Key::N) {
                self.select_next_problem();
            }
        });

        egui::Panel::left(egui::Id::new("left"))
            .resizable(true)
            .default_size(300.0)
            .show(ui, |ui| self.left_panel(ui));

        if let Some(sel) = self.selected {
            egui::Panel::right(egui::Id::new("props"))
                .resizable(true)
                .default_size(380.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.properties_panel(ui, sel));
                });
        }

        egui::CentralPanel::default_margins()
            .frame(egui::Frame::new().fill(Color32::from_rgb(23, 26, 31)))
            .show(ui, |ui| self.viewport(ui));
    }
}
