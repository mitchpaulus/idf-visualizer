//! egui application: viewport, side panels, selection, filtering, demo mode.

use crate::camera::{ray_triangle, OrbitCamera};
use crate::loops::{BranchView, Component as LoopPart, HvacLoop, LoopKind, Side};
use crate::model::{Model, ProblemKind, Severity, Surface, SurfaceType};
use crate::scene::{self, SceneRenderer, Uniforms, Vertex, ViewCallback};
use crate::svg::{self, SvgOptions};
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Scene,
    Loops,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seg {
    SeriesIn,
    Parallel,
    SeriesOut,
    Splitter,
    Mixer,
}

/// Location of a component box within the selected loop's schematic.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CompPath {
    side: usize,
    seg: Seg,
    branch: usize,
    comp: usize,
}

fn comp_in(hl: &HvacLoop, p: CompPath) -> Option<&LoopPart> {
    let side = hl.sides.get(p.side)?;
    match p.seg {
        Seg::Splitter => side.splitter.as_ref(),
        Seg::Mixer => side.mixer.as_ref(),
        _ => branch_in(hl, p)?.components.get(p.comp),
    }
}

fn branch_in(hl: &HvacLoop, p: CompPath) -> Option<&BranchView> {
    let side = hl.sides.get(p.side)?;
    let list = match p.seg {
        Seg::SeriesIn => &side.series_in,
        Seg::Parallel => &side.parallel,
        Seg::SeriesOut => &side.series_out,
        _ => return None,
    };
    list.get(p.branch)
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
    /// Path as given on the command line, reused in the copied SVG command.
    file_path: String,
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
    /// egui time of the last "Copy CLI" click, for the transient confirmation.
    copied_at: Option<f64>,
    demo: Option<Demo>,
    tab: Tab,
    loop_sel: Option<usize>,
    loop_comp: Option<CompPath>,
    loop_filter: String,
    /// Schematic view transform: screen = panel origin + pan + world * zoom.
    loop_pan: egui::Vec2,
    loop_zoom: f32,
    loop_fit_pending: bool,
}

struct Demo {
    dir: PathBuf,
    frame: u32,
    pending_shots: usize,
    closing: bool,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        model: Model,
        file_name: String,
        file_path: String,
        demo_dir: Option<PathBuf>,
    ) -> Self {
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
            tab: Tab::Scene,
            loop_sel: (!model.loops.is_empty()).then_some(0),
            loop_comp: None,
            loop_filter: String::new(),
            loop_pan: egui::Vec2::ZERO,
            loop_zoom: 1.0,
            loop_fit_pending: true,
            model,
            file_name,
            file_path,
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
            copied_at: None,
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

    /// The SVG export settings matching what the viewport is currently showing:
    /// camera angles plus the type/zone/name filters. `problem_only` has no CLI
    /// equivalent and is reported separately by the UI.
    fn svg_options(&self) -> SvgOptions {
        let rotation = self.camera.yaw.to_degrees().rem_euclid(360.0);
        SvgOptions {
            rotation: (rotation * 10.0).round() / 10.0,
            elevation: (self.camera.pitch.to_degrees() * 10.0).round() / 10.0,
            zone: self
                .zone_filter
                .as_ref()
                // The CLI applies (?i) itself; anchor so one zone name can't
                // match another that merely contains it.
                .and_then(|z| regex::Regex::new(&format!("^{}$", regex::escape(z))).ok()),
            name: self.regex.clone(),
            hide: SurfaceType::ALL
                .into_iter()
                .filter(|t| {
                    self.type_counts.get(t).copied().unwrap_or(0) > 0
                        && !self.type_visible.get(t).copied().unwrap_or(true)
                })
                .collect(),
            // The viewport draws both sides of every surface, so match it:
            // otherwise floors (outward normal down) vanish from the export.
            cull: false,
            ..Default::default()
        }
    }

    /// `idf-visualizer svg …` for the current view, writing next to the model.
    fn svg_cli(&self) -> String {
        let out = PathBuf::from(&self.file_path)
            .file_stem()
            .map(|s| format!("{}.svg", s.to_string_lossy()))
            .unwrap_or_else(|| "model.svg".to_string());
        svg::cli_command(&self.file_path, &self.svg_options(), Some(&out))
    }

    fn svg_export_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("SVG export of this view").show(ui, |ui| {
            let cmd = self.svg_cli();
            ui.label(
                RichText::new(&cmd)
                    .monospace()
                    .small()
                    .color(Color32::LIGHT_GRAY),
            );
            ui.horizontal(|ui| {
                if ui
                    .button("Copy CLI")
                    .on_hover_text(
                        "Copy the idf-visualizer svg command for the current camera \
                         angle and filters, for use in a build script.",
                    )
                    .clicked()
                {
                    ui.ctx().copy_text(cmd);
                    self.copied_at = Some(ui.input(|i| i.time));
                }
                if let Some(t) = self.copied_at {
                    let age = ui.input(|i| i.time) - t;
                    if age < 2.5 {
                        ui.label(RichText::new("copied").color(Color32::LIGHT_GREEN).small());
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                    } else {
                        self.copied_at = None;
                    }
                }
            });
            if self.problem_only {
                ui.label(
                    RichText::new("Note: \"show only flagged surfaces\" has no CLI equivalent.")
                        .color(Color32::YELLOW)
                        .small(),
                );
            }
        });
    }

    // --- UI pieces ----------------------------------------------------------

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("IDF Visualizer");
        ui.label(RichText::new(&self.file_name).weak());
        if !self.model.loops.is_empty() {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Scene, "3D model");
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Loops,
                    format!("HVAC loops ({})", self.model.loops.len()),
                );
            });
            ui.separator();
        }
        if self.tab == Tab::Loops {
            self.loops_panel(ui);
            return;
        }
        let shown = self.visible.iter().filter(|&&v| v).count();
        ui.label(format!(
            "{shown} of {} surfaces shown",
            self.model.surfaces.len()
        ));
        if ui.button("Zoom to fit  (F)").clicked() {
            self.zoom_to_fit();
        }
        self.svg_export_ui(ui);
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

    // --- HVAC loop schematic ------------------------------------------------

    fn loops_panel(&mut self, ui: &mut egui::Ui) {
        let mut text = self.loop_filter.clone();
        if ui
            .add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text("filter loops")
                    .desired_width(f32::INFINITY),
            )
            .changed()
        {
            self.loop_filter = text;
        }
        ui.separator();

        if let Some(i) = self.loop_sel {
            let l = &self.model.loops[i];
            ui.strong(&l.name);
            ui.label(RichText::new(format!("{} loop · IDF line {}", l.kind.label(), l.line)).weak());
            for s in &l.sides {
                let comps: usize = s
                    .series_in
                    .iter()
                    .chain(&s.parallel)
                    .chain(&s.series_out)
                    .map(|b| b.components.len())
                    .sum();
                ui.label(format!(
                    "{}: {} branches, {} components",
                    s.label,
                    s.branch_count(),
                    comps
                ));
                for (tag, node) in [("in", &s.inlet_node), ("out", &s.outlet_node)] {
                    if !node.is_empty() {
                        ui.label(
                            RichText::new(format!("  {tag}: {node}")).monospace().small().weak(),
                        );
                    }
                }
            }
            if !l.warnings.is_empty() {
                egui::CollapsingHeader::new(
                    RichText::new(format!("⚠ Loop warnings ({})", l.warnings.len()))
                        .color(Color32::YELLOW),
                )
                .show(ui, |ui| {
                    for w in &l.warnings {
                        ui.label(w);
                    }
                });
            }
            egui::CollapsingHeader::new("Raw IDF").show(ui, |ui| {
                egui::ScrollArea::both().max_height(300.0).show(ui, |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&l.raw).monospace().small())
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                });
            });
            ui.label(
                RichText::new("Click a component in the diagram for details.")
                    .weak()
                    .small(),
            );
            ui.separator();
        }

        let filter = self.loop_filter.to_ascii_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("loops_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for kind in [LoopKind::Air, LoopKind::Plant, LoopKind::Condenser] {
                    let items: Vec<usize> = self
                        .model
                        .loops
                        .iter()
                        .enumerate()
                        .filter(|(_, l)| {
                            l.kind == kind
                                && (filter.is_empty()
                                    || l.name.to_ascii_lowercase().contains(&filter))
                        })
                        .map(|(i, _)| i)
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    egui::CollapsingHeader::new(format!(
                        "{} loops ({})",
                        kind.label(),
                        items.len()
                    ))
                    .default_open(true)
                    .show(ui, |ui| {
                        for i in items {
                            let name = self.model.loops[i].name.clone();
                            let is_sel = self.loop_sel == Some(i);
                            if ui.selectable_label(is_sel, name).clicked() {
                                self.loop_sel = Some(i);
                                self.loop_comp = None;
                                self.loop_fit_pending = true;
                            }
                        }
                    });
                }
            });
    }

    fn loop_props_panel(&mut self, ui: &mut egui::Ui, li: usize, cp: CompPath) {
        let hl = &self.model.loops[li];
        let Some(c) = comp_in(hl, cp).cloned() else {
            self.loop_comp = None;
            return;
        };
        let loop_name = hl.name.clone();
        let side_label = hl.sides.get(cp.side).map(|s| s.label.clone());
        let branch = branch_in(hl, cp).map(|b| b.name.clone());

        ui.heading("Loop component");
        ui.separator();
        egui::Grid::new("loop_props")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.label(RichText::new(k).strong());
                    ui.label(v);
                    ui.end_row();
                };
                row("Loop", loop_name);
                if let Some(s) = side_label {
                    row("Side", s);
                }
                if let Some(b) = branch {
                    row("Branch", b);
                }
                row("Class", c.class.clone());
                row("Name", c.name.clone());
                for (k, v) in &c.specs {
                    row(k, v.clone());
                }
                if !c.inlet.is_empty() {
                    row("Inlet node", c.inlet.clone());
                }
                if !c.outlet.is_empty() {
                    row("Outlet node", c.outlet.clone());
                }
                if c.found {
                    row("IDF line", format!("{}", c.line));
                }
            });
        if !c.found && c.class != "Zone" {
            ui.label(
                RichText::new("⚠ Object not found in the file.").color(Color32::YELLOW),
            );
        }

        if c.class == "Zone" {
            let zone = self
                .zones
                .iter()
                .find(|z| z.eq_ignore_ascii_case(&c.name))
                .cloned();
            if let Some(z) = zone {
                if ui.button("Show zone in 3D").clicked() {
                    self.zone_filter = Some(z);
                    self.tab = Tab::Scene;
                    self.recompute_visibility();
                    self.zoom_to_fit();
                }
            }
        }
        if ui.button("Deselect  (Esc)").clicked() {
            self.loop_comp = None;
        }

        if !c.children.is_empty() {
            egui::CollapsingHeader::new(format!("Referenced objects ({})", c.children.len()))
                .default_open(true)
                .show(ui, |ui| {
                    for ch in &c.children {
                        egui::CollapsingHeader::new(format!(
                            "{}  ·  {}  ·  line {}",
                            ch.name, ch.class, ch.line
                        ))
                            .show(ui, |ui| {
                                egui::ScrollArea::horizontal().id_salt(&ch.name).show(
                                    ui,
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&ch.raw).monospace().small(),
                                            )
                                            .wrap_mode(egui::TextWrapMode::Extend),
                                        );
                                    },
                                );
                            });
                    }
                });
        }

        if c.found {
            egui::CollapsingHeader::new("Raw IDF")
                .default_open(true)
                .show(ui, |ui| {
                    egui::ScrollArea::both().max_height(400.0).show(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&c.raw).monospace())
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
                });
        }
    }

    fn loop_canvas(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let painter = ui.painter().with_clip_rect(rect);
        let Some(li) = self.loop_sel else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Select a loop from the list",
                egui::FontId::proportional(16.0),
                Color32::from_gray(150),
            );
            return;
        };

        // --- pan / zoom ---
        if response.dragged() {
            self.loop_pan += response.drag_delta();
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let old = self.loop_zoom;
                let new = (old * (scroll * 0.002).exp()).clamp(0.02, 3.0);
                if let Some(hp) = response.hover_pos() {
                    let world = (hp - rect.min - self.loop_pan) / old;
                    self.loop_pan = hp - rect.min - world * new;
                }
                self.loop_zoom = new;
            }
        }

        let hl = &self.model.loops[li];

        // --- layout (world coordinates) ---
        let mut layouts = Vec::new();
        let mut y = 0.0f32;
        for (si, side) in hl.sides.iter().enumerate() {
            let l = layout_side(side, si, y);
            y += l.height + SIDE_GAP;
            layouts.push(l);
        }
        let total_h = (y - SIDE_GAP).max(1.0);
        let w = layouts.iter().map(|l| l.width).fold(1.0, f32::max);
        // Mirror every other side so the loop reads as a circuit: supply flows
        // left→right on top, demand right→left below, with short closure runs.
        for (i, l) in layouts.iter_mut().enumerate() {
            if i % 2 == 1 {
                mirror_layout(l, w);
            }
        }

        if self.loop_fit_pending && rect.width() > 0.0 {
            self.loop_fit_pending = false;
            let bb = egui::Rect::from_min_max(
                egui::pos2(-CLOSURE_MARGIN - 40.0, -50.0),
                egui::pos2(w + CLOSURE_MARGIN + 40.0, total_h + 30.0),
            );
            let z = (rect.width() / bb.width())
                .min(rect.height() / bb.height())
                .clamp(0.02, 1.2);
            self.loop_zoom = z;
            self.loop_pan = (rect.size() - bb.size() * z) / 2.0 - bb.min.to_vec2() * z;
        }

        let zoom = self.loop_zoom;
        let pan = self.loop_pan;
        let ts = |p: egui::Pos2| rect.min + pan + p.to_vec2() * zoom;
        let tr = |r: egui::Rect| egui::Rect::from_min_max(ts(r.min), ts(r.max));

        // --- draw ---
        let line_color = Color32::from_gray(130);
        let stroke = egui::Stroke::new((1.4 * zoom).clamp(0.4, 2.5), line_color);
        let mut hovered: Option<CompPath> = None;
        let mut node_hover: Option<(egui::Pos2, f32, String)> = None;
        let hover_pos = response.hover_pos();

        for l in &layouts {
            for (seg, node) in &l.lines {
                let (a, b) = (ts(seg[0]), ts(seg[1]));
                painter.line_segment([a, b], stroke);
                flow_arrow(&painter, a, b, zoom, line_color);
                // Node marker: a small dot at the segment midpoint. The name
                // shows in a callout on hover (drawn after everything else so
                // it sits on top).
                if !node.is_empty() && (b - a).length() >= 14.0 {
                    let mid = egui::pos2((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
                    let r = (3.4 * zoom).clamp(2.5, 5.0);
                    painter.circle(
                        mid,
                        r,
                        Color32::from_rgb(40, 45, 53),
                        egui::Stroke::new(1.3, Color32::from_gray(150)),
                    );
                    if hover_pos.is_some_and(|p| p.distance(mid) <= r + 5.0) {
                        node_hover = Some((mid, r, node.clone()));
                    }
                }
            }
            // Compound-parent brackets, behind the boxes they enclose.
            for (r, _) in &l.groups {
                painter.rect_stroke(
                    tr(*r),
                    7.0 * zoom,
                    egui::Stroke::new((1.0 * zoom).clamp(0.4, 1.6), Color32::from_gray(100)),
                    egui::StrokeKind::Inside,
                );
            }
            for (r, path, _) in &l.bars {
                let sr = tr(*r);
                let selected = self.loop_comp == Some(*path);
                painter.rect_filled(sr, 3.0 * zoom, Color32::from_rgb(115, 135, 165));
                if selected {
                    painter.rect_stroke(
                        sr.expand(1.5),
                        3.0 * zoom,
                        egui::Stroke::new(2.0, Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                }
                if hover_pos.is_some_and(|p| sr.contains(p)) {
                    hovered = Some(*path);
                }
            }
            for (r, path) in &l.boxes {
                let Some(c) = comp_in(hl, *path) else { continue };
                let sr = tr(*r);
                let color = class_color(&c.class);
                let selected = self.loop_comp == Some(*path);
                let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 42);
                painter.rect_filled(sr, 5.0 * zoom, fill);
                let (sw, sc) = if selected {
                    (2.2, Color32::WHITE)
                } else {
                    (1.2 * zoom.clamp(0.4, 1.5), color)
                };
                painter.rect_stroke(
                    sr,
                    5.0 * zoom,
                    egui::Stroke::new(sw, sc),
                    egui::StrokeKind::Inside,
                );
                let class_px = 9.0 * zoom;
                if class_px > 4.5 {
                    painter.text(
                        egui::pos2(sr.center().x, sr.top() + 5.0 * zoom),
                        egui::Align2::CENTER_TOP,
                        trunc(&c.class, 40),
                        egui::FontId::proportional(class_px),
                        Color32::from_gray(160),
                    );
                }
                // Sizing rows (capacity/flow/head/...) between class and name.
                let spec_px = 9.5 * zoom;
                if !c.specs.is_empty() && spec_px > 4.5 {
                    for (i, line) in spec_lines(&c.specs, SPEC_CHARS, SPEC_MAX_ROWS)
                        .iter()
                        .enumerate()
                    {
                        painter.text(
                            egui::pos2(sr.center().x, sr.top() + (17.0 + i as f32 * 11.0) * zoom),
                            egui::Align2::CENTER_TOP,
                            line,
                            egui::FontId::proportional(spec_px),
                            Color32::from_rgb(170, 195, 220),
                        );
                    }
                }
                let name_px = 11.5 * zoom;
                if name_px > 5.0 {
                    painter.text(
                        egui::pos2(sr.center().x, sr.bottom() - 8.0 * zoom),
                        egui::Align2::CENTER_BOTTOM,
                        trunc(&c.name, 32),
                        egui::FontId::proportional(name_px),
                        Color32::from_gray(230),
                    );
                }
                if hover_pos.is_some_and(|p| sr.contains(p)) {
                    hovered = Some(*path);
                }
            }
            for (pos, right, text, size) in &l.labels {
                let px = size * zoom;
                if px < 5.0 {
                    continue;
                }
                let anchor = if *right {
                    egui::Align2::RIGHT_BOTTOM
                } else {
                    egui::Align2::LEFT_BOTTOM
                };
                let font = if *size <= 9.5 {
                    egui::FontId::monospace(px)
                } else {
                    egui::FontId::proportional(px)
                };
                painter.text(ts(*pos), anchor, text, font, Color32::from_gray(140));
            }
        }

        // Dashed closure runs between consecutive sides (the "pipe" that turns
        // the two half-loops into a circuit).
        for i in 0..layouts.len().saturating_sub(1) {
            let (a, b) = (&layouts[i], &layouts[i + 1]);
            let route_x = if i % 2 == 0 {
                w + CLOSURE_MARGIN
            } else {
                -CLOSURE_MARGIN
            };
            let pts = [
                a.outlet,
                egui::pos2(route_x, a.outlet.y),
                egui::pos2(route_x, b.inlet.y),
                b.inlet,
            ];
            let screen: Vec<egui::Pos2> = pts.iter().map(|&p| ts(p)).collect();
            painter.extend(egui::Shape::dashed_line(
                &screen,
                stroke,
                8.0 * zoom.max(0.5),
                6.0 * zoom.max(0.5),
            ));
        }

        // --- hover / click ---
        if let Some(path) = hovered {
            ui.ctx()
                .output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
            if let Some(c) = comp_in(hl, path) {
                let (class, name, inlet, outlet) = (
                    c.class.clone(),
                    c.name.clone(),
                    c.inlet.clone(),
                    c.outlet.clone(),
                );
                let specs: Vec<String> = c
                    .specs
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect();
                response.clone().on_hover_ui_at_pointer(|ui| {
                    ui.strong(name);
                    ui.label(RichText::new(class).weak());
                    for s in &specs {
                        ui.label(RichText::new(s).small());
                    }
                    if !inlet.is_empty() {
                        ui.label(RichText::new(format!("in:  {inlet}")).monospace().small());
                    }
                    if !outlet.is_empty() {
                        ui.label(RichText::new(format!("out: {outlet}")).monospace().small());
                    }
                });
            }
        }
        if response.clicked() {
            self.loop_comp = hovered;
        }

        // Node-name callout, drawn on top of everything. Suppressed while a
        // component tooltip is up so the two never stack.
        if hovered.is_none() {
            if let Some((p, r, name)) = &node_hover {
                painter.circle_stroke(
                    *p,
                    r + 2.0,
                    egui::Stroke::new(1.5, Color32::from_gray(230)),
                );
                let galley = painter.layout_no_wrap(
                    name.clone(),
                    egui::FontId::monospace(13.0),
                    Color32::from_gray(235),
                );
                let size = galley.size();
                let mut pos = *p + egui::vec2(12.0, -size.y - 12.0);
                pos.x = pos
                    .x
                    .min(rect.right() - size.x - 8.0)
                    .max(rect.left() + 8.0);
                pos.y = pos.y.max(rect.top() + 8.0);
                let bg = egui::Rect::from_min_size(pos, size).expand(6.0);
                painter.rect_filled(bg, 5.0, Color32::from_rgba_unmultiplied(18, 21, 26, 235));
                painter.rect_stroke(
                    bg,
                    5.0,
                    egui::Stroke::new(1.0, Color32::from_gray(95)),
                    egui::StrokeKind::Outside,
                );
                painter.galley(pos, galley, Color32::from_gray(235));
            }
        }

        // --- overlays ---
        painter.text(
            rect.left_top() + egui::vec2(10.0, 8.0),
            egui::Align2::LEFT_TOP,
            format!("{}  ·  {} loop", hl.name, hl.kind.label()),
            egui::FontId::proportional(15.0),
            Color32::from_gray(220),
        );
        painter.text(
            rect.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            "drag: pan · scroll: zoom · click: component details · F: fit",
            egui::FontId::proportional(12.0),
            Color32::from_rgba_unmultiplied(255, 255, 255, 140),
        );
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
            63 => {
                // Loop schematic: pick the loop with the most parallel paths.
                self.tab = Tab::Loops;
                self.loop_sel = self
                    .model
                    .loops
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, l)| {
                        (l.kind == LoopKind::Plant) as usize * 1000
                            + l.sides.iter().map(|s| s.parallel.len()).sum::<usize>()
                    })
                    .map(|(i, _)| i);
                // Select the supply-side component with the most sizing info
                // (e.g. the fan) so the shot shows the spec rows and panel.
                self.loop_comp = self.loop_sel.and_then(|i| {
                    let b = self.model.loops[i].sides.first()?.series_in.first()?;
                    let ci = b
                        .components
                        .iter()
                        .enumerate()
                        .max_by_key(|(_, c)| c.specs.len())
                        .map(|(ci, _)| ci)?;
                    Some(CompPath {
                        side: 0,
                        seg: Seg::SeriesIn,
                        branch: 0,
                        comp: ci,
                    })
                });
                self.loop_fit_pending = true;
            }
            72 => {
                if self.loop_sel.is_some() {
                    let d = self.demo.as_mut().unwrap();
                    shoot(d, ctx, "07-hvac-loop.png");
                }
            }
            75.. => {
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

// --- loop schematic layout (world coordinates, pixels at zoom 1) ------------

const BOX_W: f32 = 200.0;
/// Minimum box height (no sizing rows); each spec row adds SPEC_ROW_H.
const BOX_H: f32 = 48.0;
const SPEC_ROW_H: f32 = 11.0;
/// Character budget per sizing row inside a box. Conservative for the
/// proportional font at BOX_W, so rows never spill past the border.
const SPEC_CHARS: usize = 34;
const SPEC_MAX_ROWS: usize = 3;
const GAP_X: f32 = 46.0;
const GAP_Y: f32 = 30.0;
const BAR_W: f32 = 10.0;
const STUB: f32 = 80.0;
const SIDE_GAP: f32 = 120.0;
const CLOSURE_MARGIN: f32 = 50.0;

struct SideLayout {
    boxes: Vec<(egui::Rect, CompPath)>,
    /// Splitter/mixer bars: rect, path, name (for the tooltip).
    bars: Vec<(egui::Rect, CompPath, String)>,
    /// Flow line segments, each tagged with the node name it carries
    /// (empty when unknown).
    lines: Vec<([egui::Pos2; 2], String)>,
    /// (anchor, right-aligned?, text, font size at zoom 1)
    labels: Vec<(egui::Pos2, bool, String, f32)>,
    /// Bracket outlines around runs of boxes expanded from one compound
    /// component (e.g. a unitary system), with its name.
    groups: Vec<(egui::Rect, String)>,
    width: f32,
    height: f32,
    inlet: egui::Pos2,
    outlet: egui::Pos2,
}

/// Height of a component's box: base plus one row per line of sizing info.
fn box_h(c: &LoopPart) -> f32 {
    let rows = if c.specs.is_empty() {
        0
    } else {
        spec_lines(&c.specs, SPEC_CHARS, SPEC_MAX_ROWS).len()
    };
    BOX_H + SPEC_ROW_H * rows as f32
}

/// Place one side left-to-right: inlet stub, series branches, splitter bar,
/// stacked parallel branches, mixer bar, series branches, outlet stub.
/// Box heights vary with how many sizing rows each component shows.
fn layout_side(side: &Side, side_idx: usize, y_top: f32) -> SideLayout {
    let n_rows = side.parallel.len();
    let branch_h =
        |b: &BranchView| b.components.iter().map(box_h).fold(BOX_H, f32::max);
    let row_h: Vec<f32> = side.parallel.iter().map(branch_h).collect();
    let rows_h = if n_rows > 0 {
        row_h.iter().sum::<f32>() + (n_rows - 1) as f32 * GAP_Y
    } else {
        0.0
    };
    let series_h = side
        .series_in
        .iter()
        .chain(&side.series_out)
        .map(branch_h)
        .fold(BOX_H, f32::max);
    let height = rows_h.max(series_h);
    let cy = y_top + height / 2.0;
    let row_cys: Vec<f32> = {
        let mut ys = Vec::with_capacity(n_rows);
        let mut top = y_top + (height - rows_h) / 2.0;
        for h in &row_h {
            ys.push(top + h / 2.0);
            top += h + GAP_Y;
        }
        ys
    };
    let row_cy = |k: usize| row_cys[k];

    let mut out = SideLayout {
        boxes: Vec::new(),
        bars: Vec::new(),
        lines: Vec::new(),
        labels: Vec::new(),
        groups: Vec::new(),
        width: 0.0,
        height,
        inlet: egui::pos2(0.0, cy),
        outlet: egui::pos2(0.0, cy),
    };

    let mut x = STUB;
    let mut px = 0.0f32; // where the incoming flow line currently ends

    let place_series = |branches: &[BranchView],
                            seg: Seg,
                            x: &mut f32,
                            px: &mut f32,
                            out: &mut SideLayout| {
        for (bi, b) in branches.iter().enumerate() {
            for (ci, c) in b.components.iter().enumerate() {
                out.lines
                    .push(([egui::pos2(*px, cy), egui::pos2(*x, cy)], c.inlet.clone()));
                let r = egui::Rect::from_center_size(
                    egui::pos2(*x + BOX_W / 2.0, cy),
                    egui::vec2(BOX_W, box_h(c)),
                );
                out.boxes.push((
                    r,
                    CompPath {
                        side: side_idx,
                        seg,
                        branch: bi,
                        comp: ci,
                    },
                ));
                *px = *x + BOX_W;
                *x = *px + GAP_X;
            }
        }
    };

    place_series(&side.series_in, Seg::SeriesIn, &mut x, &mut px, &mut out);

    if n_rows > 0 {
        let bar_top = row_cy(0).min(cy) - 12.0;
        let bar_bot = row_cy(n_rows - 1).max(cy) + 12.0;
        let into_splitter = side
            .series_in
            .last()
            .and_then(|b| b.components.last())
            .map(|c| c.outlet.clone())
            .unwrap_or_else(|| side.inlet_node.clone());
        out.lines
            .push(([egui::pos2(px, cy), egui::pos2(x, cy)], into_splitter));
        let sp_name = side
            .splitter
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Splitter".to_string());
        out.bars.push((
            egui::Rect::from_min_max(egui::pos2(x, bar_top), egui::pos2(x + BAR_W, bar_bot)),
            CompPath {
                side: side_idx,
                seg: Seg::Splitter,
                branch: 0,
                comp: 0,
            },
            sp_name,
        ));
        let bar_right = x + BAR_W;
        let row_x0 = bar_right + GAP_X;
        let mut row_end = vec![bar_right; n_rows];
        let mut max_right = row_x0;
        for (bi, b) in side.parallel.iter().enumerate() {
            let ry = row_cy(bi);
            let mut rx = row_x0;
            let mut rpx = bar_right;
            for (ci, c) in b.components.iter().enumerate() {
                out.lines
                    .push(([egui::pos2(rpx, ry), egui::pos2(rx, ry)], c.inlet.clone()));
                let r = egui::Rect::from_center_size(
                    egui::pos2(rx + BOX_W / 2.0, ry),
                    egui::vec2(BOX_W, box_h(c)),
                );
                out.boxes.push((
                    r,
                    CompPath {
                        side: side_idx,
                        seg: Seg::Parallel,
                        branch: bi,
                        comp: ci,
                    },
                ));
                rpx = rx + BOX_W;
                rx = rpx + GAP_X;
            }
            row_end[bi] = rpx;
            max_right = max_right.max(rpx);
        }
        let mx_x = max_right + GAP_X;
        for (bi, &re) in row_end.iter().enumerate() {
            let node = side.parallel[bi]
                .components
                .last()
                .map(|c| c.outlet.clone())
                .unwrap_or_default();
            out.lines.push((
                [egui::pos2(re, row_cy(bi)), egui::pos2(mx_x, row_cy(bi))],
                node,
            ));
        }
        let mx_name = side
            .mixer
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "Mixer".to_string());
        out.bars.push((
            egui::Rect::from_min_max(
                egui::pos2(mx_x, bar_top),
                egui::pos2(mx_x + BAR_W, bar_bot),
            ),
            CompPath {
                side: side_idx,
                seg: Seg::Mixer,
                branch: 0,
                comp: 0,
            },
            mx_name,
        ));
        px = mx_x + BAR_W;
        x = px + GAP_X;
    }

    place_series(&side.series_out, Seg::SeriesOut, &mut x, &mut px, &mut out);

    out.width = px + STUB;
    out.lines.push((
        [egui::pos2(px, cy), egui::pos2(out.width, cy)],
        side.outlet_node.clone(),
    ));
    out.outlet = egui::pos2(out.width, cy);

    // Bracket consecutive boxes expanded from the same compound parent
    // (same branch, same group name).
    let comp_of = |p: &CompPath| -> Option<&LoopPart> {
        let list = match p.seg {
            Seg::SeriesIn => &side.series_in,
            Seg::Parallel => &side.parallel,
            Seg::SeriesOut => &side.series_out,
            _ => return None,
        };
        list.get(p.branch)?.components.get(p.comp)
    };
    let mut run: Option<(egui::Rect, String, Seg, usize)> = None;
    let boxes: Vec<(egui::Rect, CompPath)> = out.boxes.clone();
    let mut flushed: Vec<(egui::Rect, String)> = Vec::new();
    for (r, p) in boxes {
        let g = comp_of(&p).and_then(|c| c.group.clone());
        let same = matches!(
            (&run, &g),
            (Some((_, rg, rseg, rbranch)), Some(g)) if rg == g && *rseg == p.seg && *rbranch == p.branch
        );
        if same {
            if let Some((rr, ..)) = &mut run {
                *rr = rr.union(r);
            }
        } else {
            if let Some((rr, rg, ..)) = run.take() {
                flushed.push((rr, rg));
            }
            run = g.map(|g| (r, g, p.seg, p.branch));
        }
    }
    if let Some((rr, rg, ..)) = run.take() {
        flushed.push((rr, rg));
    }
    for (rr, rg) in flushed {
        let gr = rr.expand2(egui::vec2(11.0, 8.0));
        out.labels
            .push((egui::pos2(gr.min.x + 2.0, gr.min.y - 3.0), false, rg.clone(), 10.0));
        out.groups.push((gr, rg));
    }

    let mut title = side.label.clone();
    if n_rows > 1 {
        title.push_str(&format!("  ·  {n_rows} parallel paths"));
    }
    out.labels
        .push((egui::pos2(0.0, y_top - 34.0), false, title, 13.0));
    out
}

/// Flip a side horizontally within total width `w` so its flow reads
/// right-to-left (used for the demand side of the circuit).
fn mirror_layout(l: &mut SideLayout, w: f32) {
    let flip_rect = |r: &mut egui::Rect| {
        *r = egui::Rect::from_min_max(
            egui::pos2(w - r.max.x, r.min.y),
            egui::pos2(w - r.min.x, r.max.y),
        );
    };
    for (r, _) in &mut l.boxes {
        flip_rect(r);
    }
    for (r, _, _) in &mut l.bars {
        flip_rect(r);
    }
    for (r, _) in &mut l.groups {
        flip_rect(r);
    }
    for (seg, _) in &mut l.lines {
        seg[0].x = w - seg[0].x;
        seg[1].x = w - seg[1].x;
    }
    for (p, right, _, _) in &mut l.labels {
        p.x = w - p.x;
        *right = !*right;
    }
    l.inlet.x = w - l.inlet.x;
    l.outlet.x = w - l.outlet.x;
}

/// Arrowhead on a flow line, placed off-center so it doesn't collide with the
/// node dot at the midpoint (screen coordinates).
fn flow_arrow(painter: &egui::Painter, a: egui::Pos2, b: egui::Pos2, zoom: f32, color: Color32) {
    let v = b - a;
    let len = v.length();
    if len < 26.0 * zoom {
        return;
    }
    let d = v / len;
    let n = egui::vec2(-d.y, d.x);
    let s = (5.0 * zoom).clamp(1.5, 7.0);
    let tip = a + v * 0.28 + d * s;
    let base = a + v * 0.28 - d * s;
    let stroke = egui::Stroke::new((1.4 * zoom).clamp(0.4, 2.5), color);
    painter.line_segment([tip, base + n * s], stroke);
    painter.line_segment([tip, base - n * s], stroke);
}

/// Build the in-box sizing rows: greedy-wrap the spec values into at most
/// `max_lines` lines of `max_chars`. Bare percentages get a short label
/// ("Motor 90%") since two unlabeled percents would be ambiguous; when every
/// value is "autosized" the whole thing collapses to a single word.
fn spec_lines(specs: &[(&'static str, String)], max_chars: usize, max_lines: usize) -> Vec<String> {
    if specs.len() > 1 && specs.iter().all(|(_, v)| v == "autosized") {
        return vec!["autosized".to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    for (k, v) in specs {
        // Percentages and bare numbers (COP, ...) need their label to be
        // readable in a joined row; unit-carrying values speak for themselves.
        let part = if v.ends_with('%') || !v.chars().any(|ch| ch.is_ascii_alphabetic()) {
            format!("{} {v}", k.split_whitespace().next().unwrap_or(k))
        } else {
            v.clone()
        };
        match lines.last_mut() {
            Some(l) if l.chars().count() + 3 + part.chars().count() <= max_chars => {
                l.push_str(" · ");
                l.push_str(&part);
            }
            _ => lines.push(part),
        }
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(l) = lines.last_mut() {
            l.push('…');
        }
    }
    lines
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}


fn class_color(class: &str) -> Color32 {
    let c = class.to_ascii_lowercase();
    let (r, g, b) = if c == "zone" {
        (110, 185, 110)
    } else if c == "?" {
        (220, 90, 90)
    } else if c.contains("unitary") || c.contains("furnace") {
        (225, 170, 90)
    } else if c.contains("outdoorairsystem") {
        (120, 180, 235)
    } else if c.contains("airterminal") || c.contains("airdistribution") {
        (170, 190, 100)
    } else if c.contains("chiller") {
        (95, 190, 220)
    } else if c.contains("boiler") || c.contains("coil:heating") || c.contains("baseboard") {
        (230, 110, 100)
    } else if c.contains("tower") || c.contains("fluidcooler") {
        (95, 185, 170)
    } else if c.contains("pump") {
        (160, 120, 230)
    } else if c.contains("coil:cooling") {
        (100, 145, 235)
    } else if c.contains("fan") {
        (235, 160, 70)
    } else if c.contains("waterheater") {
        (215, 130, 170)
    } else if c.contains("humidifier") {
        (110, 200, 215)
    } else if c.contains("heatexchanger") {
        (200, 160, 120)
    } else if c.contains("pipe") || c.contains("duct") {
        (150, 150, 155)
    } else {
        (150, 155, 165)
    };
    Color32::from_rgb(r, g, b)
}

fn cardinal(azimuth: f64) -> &'static str {
    const DIRS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    DIRS[(((azimuth + 22.5) / 45.0) as usize) % 8]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(class: &str, name: &str) -> LoopPart {
        LoopPart {
            class: class.to_string(),
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn branch(name: &str, comps: &[(&str, &str)]) -> BranchView {
        BranchView {
            name: name.to_string(),
            components: comps.iter().map(|(c, n)| comp(c, n)).collect(),
        }
    }

    /// A CHW-style side: pump → splitter → {chiller · 2-comp path · bypass} →
    /// mixer → outlet pipe. Boxes must not overlap and flow must be monotonic.
    #[test]
    fn side_layout_no_overlaps_and_mirrors() {
        let side = Side {
            label: "Supply side".to_string(),
            inlet_node: "in".to_string(),
            outlet_node: "out".to_string(),
            series_in: vec![branch("pump", &[("Pump:ConstantSpeed", "P1")])],
            parallel: vec![
                branch("ch", &[("Chiller:Electric:EIR", "CH1")]),
                branch(
                    "hx",
                    &[
                        ("HeatExchanger:FluidToFluid", "HX1"),
                        ("Pipe:Adiabatic", "HX out pipe"),
                    ],
                ),
                branch("byp", &[("Pipe:Adiabatic", "Bypass")]),
            ],
            series_out: vec![branch("outb", &[("Pipe:Adiabatic", "Out pipe")])],
            splitter: Some(comp("Connector:Splitter", "S")),
            mixer: Some(comp("Connector:Mixer", "M")),
        };
        let l = layout_side(&side, 0, 0.0);
        assert_eq!(l.boxes.len(), 6);
        assert_eq!(l.bars.len(), 2);
        for (i, (a, _)) in l.boxes.iter().enumerate() {
            for (b, _) in &l.boxes[i + 1..] {
                assert!(!a.intersects(*b), "boxes overlap: {a:?} vs {b:?}");
            }
            for (bar, _, _) in &l.bars {
                assert!(!a.intersects(*bar), "box {a:?} overlaps bar {bar:?}");
            }
        }
        // Splitter bar sits right of the series-in box, mixer right of every
        // parallel box, outlet stub ends the side.
        let (sp, mx) = (&l.bars[0].0, &l.bars[1].0);
        assert!(sp.min.x >= l.boxes[0].0.max.x);
        for (r, p) in &l.boxes {
            if p.seg == Seg::Parallel {
                assert!(r.max.x <= mx.min.x);
            }
        }
        assert_eq!(l.outlet.x, l.width);
        assert_eq!(l.inlet.x, 0.0);

        let (w, old_outlet) = (l.width + 100.0, l.outlet.x);
        let mut m = l;
        mirror_layout(&mut m, w);
        assert_eq!(m.inlet.x, w);
        assert_eq!(m.outlet.x, w - old_outlet);
        for (i, (a, _)) in m.boxes.iter().enumerate() {
            for (b, _) in &m.boxes[i + 1..] {
                assert!(!a.intersects(*b), "mirrored boxes overlap");
            }
        }
    }
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
                match self.tab {
                    Tab::Scene => self.zoom_to_fit(),
                    Tab::Loops => self.loop_fit_pending = true,
                }
            }
            if i.key_pressed(egui::Key::Escape) {
                match self.tab {
                    Tab::Scene => self.selected = None,
                    Tab::Loops => self.loop_comp = None,
                }
            }
            if i.key_pressed(egui::Key::N) {
                self.select_next_problem();
            }
        });

        egui::Panel::left(egui::Id::new("left"))
            .resizable(true)
            .default_size(300.0)
            .show(ui, |ui| self.left_panel(ui));

        match self.tab {
            Tab::Scene => {
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
            Tab::Loops => {
                if let (Some(li), Some(cp)) = (self.loop_sel, self.loop_comp) {
                    egui::Panel::right(egui::Id::new("loop_props"))
                        .resizable(true)
                        .default_size(420.0)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| self.loop_props_panel(ui, li, cp));
                        });
                }
                egui::CentralPanel::default_margins()
                    .frame(egui::Frame::new().fill(Color32::from_rgb(23, 26, 31)))
                    .show(ui, |ui| self.loop_canvas(ui));
            }
        }
    }
}
