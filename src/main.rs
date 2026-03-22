use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use clap::Parser;
use mlua::{Error, Result};
use roblox_luau_env::{Cli, Command, RobloxEnvironment, RuntimeMode, gui, image, rbxlx, sync};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Gui => {
            gui::run()?;
        }
        Command::RunServer { path } => {
            let environment = RobloxEnvironment::new(RuntimeMode::Server)?;
            environment.run_project_path(&path)?;
            wait_until_ctrl_c("server")?;
        }
        Command::EmulateClient { path } => {
            let environment = RobloxEnvironment::new(RuntimeMode::Client)?;
            environment.run_project_path(&path)?;
            wait_until_ctrl_c("client emulation")?;
        }
        Command::Pack {
            project_dir,
            output_image,
        } => {
            let project = roblox_luau_env::project::LoadedProject::from_path(&project_dir)?;
            image::write_project_image(&project, &output_image)?;
            println!(
                "Packed {} into {}",
                project_dir.display(),
                output_image.display()
            );
        }
        Command::Unpack {
            image: input,
            output_dir,
        } => {
            image::unpack_project_image(&input, &output_dir)?;
            println!("Unpacked {} into {}", input.display(), output_dir.display());
        }
        Command::ExportRbxlx {
            input,
            output_rbxlx,
        } => {
            rbxlx::export_path_to_rbxlx(&input, &output_rbxlx)?;
            println!("Exported {} to {}", input.display(), output_rbxlx.display());
        }
        Command::ExportRbxmx {
            input,
            output_rbxmx,
        } => {
            rbxlx::export_path_to_rbxmx(&input, &output_rbxmx)?;
            println!("Exported {} to {}", input.display(), output_rbxmx.display());
        }
        Command::Sync { folder, port } => {
            println!("[FileSync] Starting file sync server for folder: {}", folder.display());
            sync::start_sync_server(&folder, port)?;
        }
    }

    Ok(())
}

fn wait_until_ctrl_c(label: &str) -> Result<()> {
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let signal_flag = shutdown_requested.clone();

    ctrlc::set_handler(move || {
        signal_flag.store(true, Ordering::SeqCst);
    })
    .map_err(|error| Error::RuntimeError(format!("Could not install Ctrl+C handler: {error}")))?;

    println!("{label} is running. Press Ctrl+C to exit.");

    while !shutdown_requested.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(100));
    }

    println!("Shutting down {label}.");
    Ok(())
}
