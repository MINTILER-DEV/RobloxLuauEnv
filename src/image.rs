use std::fs;
use std::path::{Path, PathBuf};
use std::io::{Read, Write};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use mlua::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::project::{LoadedProject, ProjectFile};

const IMAGE_MAGIC: &str = "RLEIMG2\n"; // Version 2 with compression
const IMAGE_MAGIC_V1: &str = "RLEIMG1\n"; // Version 1 without compression

#[derive(Debug, Serialize, Deserialize)]
struct ProjectImage {
    format: String,
    version: u32,
    files: Vec<ProjectImageFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProjectImageFile {
    path: String,
    content_base64: String,
}

pub fn write_project_image(project: &LoadedProject, output_path: &Path) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }

    let image = ProjectImage {
        format: "RobloxLuaEnvironment".to_string(),
        version: 2,
        files: project
            .files
            .iter()
            .map(|file| ProjectImageFile {
                path: normalize_rel_path(&file.relative_path),
                content_base64: STANDARD.encode(&file.bytes),
            })
            .collect(),
    };

    let json_data = serde_json::to_vec_pretty(&image)
        .map_err(|error| Error::RuntimeError(format!("Could not encode image: {error}")))?;

    // Compress the JSON data using gzip
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&json_data)
        .map_err(|error| Error::RuntimeError(format!("Compression error: {error}")))?;
    let compressed = encoder.finish()
        .map_err(|error| Error::RuntimeError(format!("Compression finish error: {error}")))?;

    let mut encoded = IMAGE_MAGIC.as_bytes().to_vec();
    encoded.extend(compressed);
    fs::write(output_path, encoded).map_err(io_error)
}

pub fn read_project_image(path: &Path) -> Result<LoadedProject> {
    let bytes = fs::read(path).map_err(io_error)?;
    decode_project_image(&bytes)
}

pub fn decode_project_image(bytes: &[u8]) -> Result<LoadedProject> {
    // Try to detect version
    let (payload, is_compressed) = if let Some(rest) = bytes.strip_prefix(IMAGE_MAGIC.as_bytes()) {
        (rest, true)
    } else if let Some(rest) = bytes.strip_prefix(IMAGE_MAGIC_V1.as_bytes()) {
        (rest, false)
    } else {
        return Err(Error::RuntimeError("Invalid .rleimg header".to_string()));
    };

    // Decompress if needed
    let json_bytes = if is_compressed {
        let mut decoder = GzDecoder::new(payload);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)
            .map_err(|error| Error::RuntimeError(format!("Decompression error: {error}")))?;
        decompressed
    } else {
        payload.to_vec()
    };

    let image: ProjectImage = serde_json::from_slice(&json_bytes)
        .map_err(|error| Error::RuntimeError(format!("Could not decode image: {error}")))?;

    if image.format != "RobloxLuaEnvironment" {
        return Err(Error::RuntimeError(format!(
            "Unsupported image format '{}'",
            image.format
        )));
    }

    let files = image
        .files
        .into_iter()
        .map(|file| {
            let bytes = STANDARD.decode(file.content_base64).map_err(|error| {
                Error::RuntimeError(format!(
                    "Could not decode image entry '{}': {error}",
                    file.path
                ))
            })?;
            Ok(ProjectFile {
                relative_path: PathBuf::from(file.path),
                bytes,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(LoadedProject { files })
}

pub fn unpack_project_image(image_path: &Path, output_dir: &Path) -> Result<()> {
    let project = read_project_image(image_path)?;
    if output_dir.exists() && !output_dir.is_dir() {
        return Err(Error::RuntimeError(format!(
            "{} exists and is not a directory",
            output_dir.display()
        )));
    }

    fs::create_dir_all(output_dir).map_err(io_error)?;
    for file in project.files {
        let destination = output_dir.join(&file.relative_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        fs::write(destination, file.bytes).map_err(io_error)?;
    }

    Ok(())
}

fn normalize_rel_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn io_error(error: std::io::Error) -> Error {
    Error::RuntimeError(format!("I/O error: {error}"))
}
