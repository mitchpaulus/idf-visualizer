mod analysis;
mod app;
mod camera;
mod idf;
mod loops;
mod model;
mod scene;
mod svg;

use eframe::egui;
use model::SurfaceType;
use std::path::PathBuf;
use svg::SvgOptions;

const USAGE: &str = "\
idf-visualizer — 3D viewer and SVG exporter for EnergyPlus IDF models

Usage:
  idf-visualizer [model.idf] [--info] [--demo DIR]
  idf-visualizer svg [model.idf] [options]

Viewer options:
  -h, --help            Show this help and exit.
  --info                Parse, print geometry warnings, and exit.
  --demo DIR            Scripted run that saves feature screenshots to DIR.

svg options:
  -o, --out FILE        Write to FILE (default: stdout).
  -r, --rotation DEG    View azimuth; 0 = from the south, positive swings east
                        (default 45).
  -e, --elevation DEG   Angle above the horizon (default 35.264, true isometric).
  -w, --width PX        Output width (default 1000).
  -H, --height PX       Output height (default: fitted to the content).
      --margin PX       Padding around the drawing (default 24).
      --stroke-width N  Edge width in px (default 0.8).
      --zone REGEX      Only surfaces whose zone matches (case-insensitive).
      --name REGEX      Only surfaces whose name matches (case-insensitive).
      --hide TYPES      Comma-separated types to omit, e.g. roof,ceiling.
                        (wall, floor, ceiling, roof, window, door, shading)
      --no-cull         Also draw surfaces facing away from the viewer.
      --flat            Disable angle-based shading (solid type colors).
      --legend          Draw a surface-type key below the model.
      --background CSS  Fill the canvas (default: transparent).
";

fn main() -> eframe::Result {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "svg") {
        args.remove(0);
        if let Err(e) = run_svg(&args) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    run_viewer(&args)
}

/// Parse an IDF file into a model. `quiet` keeps stdout free for SVG output,
/// sending only warnings to stderr.
fn load(path: &str, quiet: bool) -> anyhow::Result<model::Model> {
    let src =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("could not read {path}: {e}"))?;
    let t0 = std::time::Instant::now();
    let objects = idf::parse(&src);
    let m = model::build(&objects);
    if quiet {
        for w in &m.warnings {
            eprintln!("warning: {w}");
        }
        return Ok(m);
    }
    println!(
        "{}: {} objects, {} surfaces, {} HVAC loops in {:.1} ms",
        path,
        objects.len(),
        m.surfaces.len(),
        m.loops.len(),
        t0.elapsed().as_secs_f64() * 1000.0
    );
    for w in &m.warnings {
        println!("warning: {w}");
    }
    for l in &m.loops {
        for w in &l.warnings {
            println!("loop \"{}\" [{}]: {}", l.name, l.kind.label(), w);
        }
    }
    for s in &m.surfaces {
        for p in &s.problems {
            println!(
                "surface \"{}\" [{} · {}]: {}",
                s.name,
                p.severity.label(),
                p.kind.label(),
                p.message
            );
        }
    }
    Ok(m)
}

fn run_svg(args: &[String]) -> anyhow::Result<()> {
    let mut file: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut opt = SvgOptions::default();

    let need = |it: &mut std::slice::Iter<String>, flag: &str| -> anyhow::Result<String> {
        it.next()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--out" => out = Some(PathBuf::from(need(&mut it, a)?)),
            "-r" | "--rotation" => opt.rotation = need(&mut it, a)?.parse()?,
            "-e" | "--elevation" => opt.elevation = need(&mut it, a)?.parse()?,
            "-w" | "--width" => opt.width = need(&mut it, a)?.parse()?,
            "-H" | "--height" => opt.height = Some(need(&mut it, a)?.parse()?),
            "--margin" => opt.margin = need(&mut it, a)?.parse()?,
            "--stroke-width" => opt.stroke_width = need(&mut it, a)?.parse()?,
            "--zone" => opt.zone = Some(regex::Regex::new(&format!("(?i){}", need(&mut it, a)?))?),
            "--name" => opt.name = Some(regex::Regex::new(&format!("(?i){}", need(&mut it, a)?))?),
            "--hide" => {
                for t in need(&mut it, a)?.split(',') {
                    opt.hide.push(parse_type(t.trim())?);
                }
            }
            "--no-cull" => opt.cull = false,
            "--flat" => opt.shade = false,
            "--legend" => opt.legend = true,
            "--background" => opt.background = Some(need(&mut it, a)?),
            _ if a.starts_with('-') => anyhow::bail!("unknown option {a}"),
            _ => file = Some(a.clone()),
        }
    }
    if opt.width <= 0.0 {
        anyhow::bail!("--width must be positive");
    }

    let path = file.unwrap_or_else(|| "in.idf".to_string());
    let m = load(&path, true)?;
    let doc = svg::render(&m, &opt);
    if !doc.contains("<path") {
        eprintln!("warning: no surfaces drawn (check --zone/--name/--hide, or try --no-cull)");
    }
    match &out {
        Some(p) => std::fs::write(p, doc)
            .map_err(|e| anyhow::anyhow!("could not write {}: {e}", p.display()))?,
        None => print!("{doc}"),
    }
    Ok(())
}

fn parse_type(s: &str) -> anyhow::Result<SurfaceType> {
    SurfaceType::ALL
        .into_iter()
        .find(|t| t.label().eq_ignore_ascii_case(s))
        .ok_or_else(|| anyhow::anyhow!("unknown surface type \"{s}\""))
}

fn run_viewer(args: &[String]) -> eframe::Result {
    let mut file: Option<String> = None;
    let mut demo_dir: Option<PathBuf> = None;
    let mut info_only = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--demo" => {
                demo_dir = Some(PathBuf::from(
                    it.next().expect("--demo requires an output directory"),
                ))
            }
            "--info" => info_only = true,
            _ => file = Some(a.clone()),
        }
    }
    let path = file.unwrap_or_else(|| "in.idf".to_string());
    let m = load(&path, false).unwrap_or_else(|e| panic!("{e}"));

    if info_only {
        return Ok(());
    }

    if let Some(dir) = &demo_dir {
        std::fs::create_dir_all(dir).expect("create demo dir");
    }

    let file_name = PathBuf::from(&path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(path.clone());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("IDF Visualizer")
            .with_inner_size([1600.0, 1000.0]),
        renderer: eframe::Renderer::Wgpu,
        depth_buffer: 32,
        multisampling: 4,
        ..Default::default()
    };
    eframe::run_native(
        "idf-visualizer",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, m, file_name, path, demo_dir)))),
    )
}
