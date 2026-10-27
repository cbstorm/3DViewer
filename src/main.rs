// Release builds on Windows are GUI apps: no console window. Fatal errors
// are shown in a message box instead.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod assets;
mod renderer;
mod scene;
mod ui;

use std::path::PathBuf;

use anyhow::Result;
use tracing_subscriber::EnvFilter;
use winit::event_loop::EventLoop;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let options = match parse_args(std::env::args_os().skip(1)) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("{msg}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let event_loop = EventLoop::<app::UserEvent>::with_user_event().build()?;
    let mut app = app::App::new(event_loop.create_proxy(), options);
    event_loop.run_app(&mut app)?;

    match app.fatal_error {
        Some(e) => {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("3D Viewer")
                .set_description(format!("{e:#}"))
                .show();
            Err(e)
        }
        None => Ok(()),
    }
}

const USAGE: &str = "\
Usage: viewer3d [MODEL] [--screenshot OUT.png]

  MODEL                 .gltf, .glb, .obj or .stl file to open
  --screenshot OUT.png  render the window once, save it as PNG and exit";

fn parse_args(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<app::Options, String> {
    let mut options = app::Options::default();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            Some("--screenshot") => {
                let path = args.next().ok_or("--screenshot needs an output path")?;
                options.screenshot = Some(PathBuf::from(path));
            }
            Some(flag) if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ if options.model.is_none() => options.model = Some(PathBuf::from(arg)),
            _ => return Err("only one model file can be opened".into()),
        }
    }
    Ok(options)
}
