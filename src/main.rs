mod analysis;
mod app;
mod camera;
mod idf;
mod model;
mod scene;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result {
    let mut file: Option<String> = None;
    let mut demo_dir: Option<PathBuf> = None;
    let mut info_only = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--demo" => {
                demo_dir = Some(PathBuf::from(
                    args.next().expect("--demo requires an output directory"),
                ))
            }
            "--info" => info_only = true,
            _ => file = Some(a),
        }
    }
    let path = file.unwrap_or_else(|| "in.idf".to_string());

    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    let t0 = std::time::Instant::now();
    let objects = idf::parse(&src);
    let m = model::build(&objects);
    println!(
        "{}: {} objects, {} surfaces in {:.1} ms",
        path,
        objects.len(),
        m.surfaces.len(),
        t0.elapsed().as_secs_f64() * 1000.0
    );
    for w in &m.warnings {
        println!("warning: {w}");
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
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, m, file_name, demo_dir)))),
    )
}
