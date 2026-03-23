use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use mlua::{Error, Result};

#[derive(Clone, Debug)]
pub struct ProjectFile {
    pub relative_path: PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct LoadedProject {
    pub files: Vec<ProjectFile>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptKind {
    ModuleScript,
    ServerScript,
    LocalScript,
}

#[derive(Clone, Debug)]
pub struct ProjectScript {
    pub relative_path: PathBuf,
    pub container_path: Vec<String>,
    pub name: String,
    pub kind: ScriptKind,
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct ExternalFile {
    pub relative_path: PathBuf,
    pub container_path: Vec<String>,
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ProjectLayout {
    pub top_level: Vec<ProjectMount>,
}

#[derive(Clone, Debug)]
pub enum ProjectMount {
    DataModelChild(ProjectNode),
    ServiceContents {
        service_name: String,
        children: Vec<ProjectNode>,
    },
}

#[derive(Clone, Debug)]
pub struct ProjectNode {
    pub name: String,
    pub class_name: String,
    pub source: Option<String>,
    pub value: Option<Vec<u8>>,
    pub script_path: Option<PathBuf>,
    pub children: Vec<ProjectNode>,
}

#[derive(Clone, Debug, Default)]
struct DirNode {
    directories: BTreeMap<String, DirNode>,
    scripts: Vec<ProjectScript>,
    external_files: Vec<ExternalFile>,
}

impl LoadedProject {
    pub fn from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(Error::RuntimeError(format!(
                "Project path does not exist: {}",
                path.display()
            )));
        }
        if !path.is_dir() {
            return Err(Error::RuntimeError(format!(
                "Project path must be a directory: {}",
                path.display()
            )));
        }

        let mut files = Vec::new();
        collect_files(path, path, &mut files)?;
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok(Self { files })
    }

    pub fn scripts(&self) -> Result<Vec<ProjectScript>> {
        self.files
            .iter()
            .filter_map(|file| classify_script_file(file).transpose())
            .collect()
    }

    pub fn external_files(&self) -> Result<Vec<ExternalFile>> {
        self.files
            .iter()
            .filter_map(|file| classify_external_file(file).transpose())
            .collect()
    }

    pub fn layout(&self) -> Result<ProjectLayout> {
        let scripts = self.scripts()?;
        let external_files = self.external_files()?;
        let mut root = DirNode::default();
        
        for script in scripts {
            let container_path = script.container_path.clone();
            insert_script(&mut root, &container_path, script);
        }

        for external_file in external_files {
            let container_path = external_file.container_path.clone();
            insert_external_file(&mut root, &container_path, external_file);
        }

        let mut top_level = Vec::new();
        for script in root
            .scripts
            .iter()
            .filter(|script| !is_init_module(script))
            .cloned()
        {
            top_level.push(ProjectMount::DataModelChild(script_to_node(script)));
        }

        for external_file in root.external_files.clone() {
            top_level.push(ProjectMount::DataModelChild(external_file_to_node(external_file)));
        }

        for (name, directory) in root.directories {
            if is_service_name(&name) {
                let children = build_children_from_directory(&directory, true);
                if !children.is_empty() {
                    top_level.push(ProjectMount::ServiceContents {
                        service_name: name,
                        children,
                    });
                }
            } else if let Some(node) = build_directory_mount(name, directory) {
                top_level.push(ProjectMount::DataModelChild(node));
            }
        }

        Ok(ProjectLayout { top_level })
    }
}

pub fn is_rleimg_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("rleimg"))
        .unwrap_or(false)
}

pub fn path_segments(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect()
}

fn collect_files(root: &Path, current: &Path, files: &mut Vec<ProjectFile>) -> Result<()> {
    for entry in fs::read_dir(current).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(io_error)?;
        if file_type.is_dir() {
            collect_files(root, &path, files)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let relative_path = path.strip_prefix(root).map_err(|error| {
            Error::RuntimeError(format!(
                "Could not compute relative path for {}: {error}",
                path.display()
            ))
        })?;
        let bytes = fs::read(&path).map_err(io_error)?;
        files.push(ProjectFile {
            relative_path: relative_path.to_path_buf(),
            bytes,
        });
    }
    Ok(())
}

fn classify_script_file(file: &ProjectFile) -> Result<Option<ProjectScript>> {
    let Some(file_name) = file
        .relative_path
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return Ok(None);
    };

    let kind = if let Some(base_name) = file_name.strip_suffix(".server.luau") {
        Some((ScriptKind::ServerScript, base_name))
    } else if let Some(base_name) = file_name.strip_suffix(".server.lua") {
        Some((ScriptKind::ServerScript, base_name))
    } else if let Some(base_name) = file_name.strip_suffix(".client.luau") {
        Some((ScriptKind::LocalScript, base_name))
    } else if let Some(base_name) = file_name.strip_suffix(".client.lua") {
        Some((ScriptKind::LocalScript, base_name))
    } else if let Some(base_name) = file_name.strip_suffix(".luau") {
        Some((ScriptKind::ModuleScript, base_name))
    } else if let Some(base_name) = file_name.strip_suffix(".lua") {
        Some((ScriptKind::ModuleScript, base_name))
    } else {
        None
    };

    let Some((kind, base_name)) = kind else {
        return Ok(None);
    };

    let source = String::from_utf8(file.bytes.clone()).map_err(|error| {
        Error::RuntimeError(format!(
            "Script {} is not valid UTF-8: {error}",
            file.relative_path.display()
        ))
    })?;

    let mut container_path = path_segments(&file.relative_path);
    container_path.pop();

    Ok(Some(ProjectScript {
        relative_path: file.relative_path.clone(),
        container_path,
        name: base_name.to_string(),
        kind,
        source,
    }))
}

fn classify_external_file(file: &ProjectFile) -> Result<Option<ExternalFile>> {
    let segments = path_segments(&file.relative_path);
    
    // Check if the file is in an ExternalData directory
    if segments.is_empty() || segments[0] != "ExternalData" {
        return Ok(None);
    }

    // Skip if it's a script file (even in ExternalData)
    let Some(file_name) = file
        .relative_path
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return Ok(None);
    };

    if file_name.ends_with(".luau")
        || file_name.ends_with(".lua")
        || file_name.ends_with(".server.luau")
        || file_name.ends_with(".server.lua")
        || file_name.ends_with(".client.luau")
        || file_name.ends_with(".client.lua")
    {
        return Ok(None);
    }

    let mut container_path = segments.clone();
    container_path.remove(0); // Remove "ExternalData"
    container_path.pop(); // Remove filename

    Ok(Some(ExternalFile {
        relative_path: file.relative_path.clone(),
        container_path,
        name: file_name.to_string(),
        bytes: file.bytes.clone(),
    }))
}

fn insert_script(root: &mut DirNode, path: &[String], script: ProjectScript) {
    let mut current = root;
    for segment in path {
        current = current.directories.entry(segment.clone()).or_default();
    }
    current.scripts.push(script);
}

fn insert_external_file(root: &mut DirNode, path: &[String], external_file: ExternalFile) {
    let mut current = root;
    for segment in path {
        current = current.directories.entry(segment.clone()).or_default();
    }
    current.external_files.push(external_file);
}

fn build_directory_mount(name: String, directory: DirNode) -> Option<ProjectNode> {
    if let Some(init_script) = directory
        .scripts
        .iter()
        .find(|script| is_init_module(script))
        .cloned()
    {
        let mut children = build_children_from_directory(&directory, false);
        children.sort_by(|left, right| left.name.cmp(&right.name));
        return Some(ProjectNode {
            name,
            class_name: "ModuleScript".to_string(),
            source: Some(init_script.source),
            value: None,
            script_path: Some(init_script.relative_path),
            children,
        });
    }

    let mut children = build_children_from_directory(&directory, false);
    if children.is_empty() {
        return None;
    }
    children.sort_by(|left, right| left.name.cmp(&right.name));
    Some(ProjectNode {
        name,
        class_name: "Folder".to_string(),
        source: None,
        value: None,
        script_path: None,
        children,
    })
}

fn build_children_from_directory(
    directory: &DirNode,
    skip_directory_init_replacement: bool,
) -> Vec<ProjectNode> {
    let mut children = Vec::new();

    for script in &directory.scripts {
        if is_init_module(script) {
            if !skip_directory_init_replacement {
                continue;
            }
        }
        children.push(script_to_node(script.clone()));
    }

    for external_file in &directory.external_files {
        children.push(external_file_to_node(external_file.clone()));
    }

    for (name, child_directory) in &directory.directories {
        let node = build_directory_mount(name.clone(), child_directory.clone());
        if let Some(node) = node {
            children.push(node);
        }
    }

    children.sort_by(|left, right| left.name.cmp(&right.name));
    children
}

fn script_to_node(script: ProjectScript) -> ProjectNode {
    let class_name = match script.kind {
        ScriptKind::ModuleScript => "ModuleScript",
        ScriptKind::ServerScript => "Script",
        ScriptKind::LocalScript => "LocalScript",
    };

    ProjectNode {
        name: script.name,
        class_name: class_name.to_string(),
        source: Some(script.source),
        value: None,
        script_path: Some(script.relative_path),
        children: Vec::new(),
    }
}

fn external_file_to_node(external_file: ExternalFile) -> ProjectNode {
    ProjectNode {
        name: external_file.name,
        class_name: "StringValue".to_string(),
        source: None,
        value: Some(external_file.bytes),
        script_path: Some(external_file.relative_path),
        children: Vec::new(),
    }
}

fn is_init_module(script: &ProjectScript) -> bool {
    script.kind == ScriptKind::ModuleScript && script.name == "init"
}

fn is_service_name(name: &str) -> bool {
    matches!(
        name,
        "Workspace"
            | "ReplicatedStorage"
            | "ServerStorage"
            | "ServerScriptService"
            | "Lighting"
            | "Players"
            | "RunService"
            | "HttpService"
            | "TweenService"
    )
}

fn io_error(error: std::io::Error) -> Error {
    Error::RuntimeError(format!("I/O error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{LoadedProject, ProjectFile, ProjectMount};
    use std::path::PathBuf;

    #[test]
    fn directory_init_becomes_module_script_container() {
        let project = LoadedProject {
            files: vec![
                ProjectFile {
                    relative_path: PathBuf::from("ReplicatedStorage/Foo/init.luau"),
                    bytes: b"return {}".to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("ReplicatedStorage/Foo/Child.luau"),
                    bytes: b"return 123".to_vec(),
                },
            ],
        };

        let layout = project.layout().expect("layout");
        let ProjectMount::ServiceContents { children, .. } = &layout.top_level[0] else {
            panic!("expected service mount");
        };
        assert_eq!(children[0].class_name, "ModuleScript");
        assert_eq!(children[0].name, "Foo");
        assert_eq!(children[0].children[0].name, "Child");
    }

    #[test]
    fn external_files_keep_raw_bytes_on_string_value_nodes() {
        let project = LoadedProject {
            files: vec![ProjectFile {
                relative_path: PathBuf::from("ExternalData/hello.elf"),
                bytes: vec![0x7f, 0x45, 0x4c, 0x46, 0x00, 0xff],
            }],
        };

        let layout = project.layout().expect("layout");
        let ProjectMount::DataModelChild(node) = &layout.top_level[0] else {
            panic!("expected top-level external file");
        };
        assert_eq!(node.class_name, "StringValue");
        assert_eq!(node.name, "hello.elf");
        assert!(node.source.is_none());
        assert_eq!(node.value.as_deref(), Some(&[0x7f, 0x45, 0x4c, 0x46, 0x00, 0xff][..]));
    }
}
