use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "rle",
    about = "Roblox Lua Environment CLI",
    version,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Open the RLE desktop GUI")]
    Gui,
    #[command(about = "Run a project or .rleimg image in server mode")]
    RunServer {
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
    #[command(about = "Run a project or .rleimg image in client emulation mode")]
    EmulateClient {
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
    #[command(about = "Pack a project directory into a .rleimg image")]
    Pack {
        #[arg(value_name = "PROJECT_DIR")]
        project_dir: PathBuf,
        #[arg(value_name = "OUTPUT_IMAGE")]
        output_image: PathBuf,
    },
    #[command(about = "Unpack a .rleimg image into a directory")]
    Unpack {
        #[arg(value_name = "IMAGE")]
        image: PathBuf,
        #[arg(value_name = "OUTPUT_DIR")]
        output_dir: PathBuf,
    },
    #[command(about = "Export a project or .rleimg image to Roblox XML (.rbxlx)")]
    ExportRbxlx {
        #[arg(value_name = "INPUT")]
        input: PathBuf,
        #[arg(value_name = "OUTPUT_RBXLX")]
        output_rbxlx: PathBuf,
    },
    #[command(about = "Export a project or .rleimg image to Roblox model XML (.rbxmx)")]
    ExportRbxmx {
        #[arg(value_name = "INPUT")]
        input: PathBuf,
        #[arg(value_name = "OUTPUT_RBXMX")]
        output_rbxmx: PathBuf,
    },
    #[command(about = "Sync a folder to Roblox Studio via web socket on port 57163")]
    Sync {
        #[arg(value_name = "FOLDER")]
        folder: PathBuf,
        #[arg(value_name = "PORT", default_value = "57163")]
        port: u16,
    },
}
