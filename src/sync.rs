use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::SystemTime;

use mlua::{Error, Result};
use serde::{Deserialize, Serialize};
use tiny_http::{Request, Response, Server, Header};

#[derive(Debug, Serialize, Deserialize)]
struct FileInfo {
    path: String,
    modified_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileContent {
    content: String,
}

struct SyncState {
    watch_folder: PathBuf,
    file_modifications: std::collections::HashMap<String, u64>,
}

pub fn start_sync_server(folder: &Path, port: u16) -> Result<()> {
    let folder_path = folder.to_path_buf();
    
    if !folder_path.exists() {
        return Err(Error::RuntimeError(format!(
            "Folder does not exist: {}",
            folder_path.display()
        )));
    }

    if !folder_path.is_dir() {
        return Err(Error::RuntimeError(format!(
            "Path is not a directory: {}",
            folder_path.display()
        )));
    }

    let addr = format!("127.0.0.1:{}", port);
    let server = Server::http(&addr).map_err(|e| {
        Error::RuntimeError(format!("Failed to start server on {}: {}", addr, e))
    })?;

    println!("[FileSync] Server listening on http://{}", addr);

    let state = Arc::new(Mutex::new(SyncState {
        watch_folder: folder_path.clone(),
        file_modifications: std::collections::HashMap::new(),
    }));

    // Start file monitoring in a separate thread
    let state_clone = state.clone();
    let folder_clone = folder_path.clone();
    thread::spawn(move || {
        monitor_folder(&folder_clone, &state_clone);
    });

    // Handle incoming requests
    for request in server.incoming_requests() {
        let state_clone = state.clone();
        thread::spawn(move || {
            handle_request(request, &state_clone);
        });
    }

    Ok(())
}

fn handle_request(request: Request, state: &Arc<Mutex<SyncState>>) {
    let method = request.method().to_string();
    let path = request.url().to_string();

    match (method.as_str(), path.as_str()) {
        ("GET", "/ping") => {
            let response = Response::from_string("{\"status\": \"ok\"}");
            let _ = request.respond(response);
        }
        ("GET", "/files") => {
            let state = state.lock().unwrap();
            let files = list_files(&state.watch_folder);
            match serde_json::to_string(&files) {
                Ok(json) => {
                    let response = Response::from_string(json)
                        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
                    let _ = request.respond(response);
                }
                Err(_) => {
                    let _ = request.respond(Response::from_string("{\"error\": \"Failed to serialize files\"}").with_status_code(500));
                }
            }
        }
        _ if path.starts_with("/file/") => {
            let file_path = &path[6..]; // Remove "/file/" prefix
            let state = state.lock().unwrap();
            handle_file_request(request, &state.watch_folder, file_path);
        }
        ("POST", "/file-deleted") => {
            // Acknowledge file deletion notification
            println!("[FileSync] File deletion notification received");
            let _ = request.respond(Response::from_string("{\"status\": \"received\"}"));
        }
        _ => {
            let _ = request.respond(Response::from_string("{\"error\": \"Not found\"}").with_status_code(404));
        }
    }
}

fn handle_file_request(request: Request, base_dir: &Path, file_path: &str) {
    let full_path = base_dir.join(file_path);

    if !full_path.starts_with(base_dir) {
        let _ = request.respond(Response::from_string("{\"error\": \"Path traversal not allowed\"}").with_status_code(403));
        return;
    }

    match fs::read_to_string(&full_path) {
        Ok(content) => {
            match serde_json::to_string(&FileContent { content }) {
                Ok(json) => {
                    let response = Response::from_string(json)
                        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
                    let _ = request.respond(response);
                }
                Err(_) => {
                    let _ = request.respond(Response::from_string("{\"error\": \"Failed to serialize content\"}").with_status_code(500));
                }
            }
        }
        Err(_) => {
            let _ = request.respond(Response::from_string("{\"error\": \"File not found\"}").with_status_code(404));
        }
    }
}

fn list_files(base_dir: &Path) -> Vec<FileInfo> {
    let mut files = Vec::new();

    if let Ok(entries) = fs::read_dir(base_dir) {
        for entry in entries.flatten() {
            if let Ok(path) = entry.path().strip_prefix(base_dir) {
                if let Ok(metadata) = entry.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        let modified_at = modified
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);

                        files.push(FileInfo {
                            path: path.to_string_lossy().to_string(),
                            modified_at,
                        });
                    }
                }
            }
        }
    }

    files
}

fn monitor_folder(folder: &Path, state: &Arc<Mutex<SyncState>>) {
    loop {
        let files = list_files(folder);
        let mut state = state.lock().unwrap();

        for file in files {
            let prev_time = state.file_modifications.get(&file.path).copied().unwrap_or(0);
            if file.modified_at > prev_time {
                println!("[FileSync] File changed: {}", file.path);
                state.file_modifications.insert(file.path, file.modified_at);
            }
        }

        drop(state);
        thread::sleep(std::time::Duration::from_millis(500));
    }
}
