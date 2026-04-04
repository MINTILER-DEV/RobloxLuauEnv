use std::fs;
use std::path::Path;

use mlua::{Error, Result};

use crate::image;
use crate::project::{LoadedProject, ProjectLayout, ProjectMount, ProjectNode, is_rleimg_path};

pub fn export_path_to_rbxlx(input: &Path, output: &Path) -> Result<()> {
    let project = if is_rleimg_path(input) {
        image::read_project_image(input)?
    } else {
        LoadedProject::from_path(input)?
    };
    write_project_to_rbxlx(&project, output)
}

pub fn write_project_to_rbxlx(project: &LoadedProject, output: &Path) -> Result<()> {
    let xml = project_to_rbxlx(project)?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    fs::write(output, xml).map_err(io_error)
}

pub fn project_to_rbxlx(project: &LoadedProject) -> Result<String> {
    let layout = project.layout()?;
    Ok(layout_to_rbxlx(&layout))
}

pub fn export_path_to_rbxmx(input: &Path, output: &Path) -> Result<()> {
    let project = if is_rleimg_path(input) {
        image::read_project_image(input)?
    } else {
        LoadedProject::from_path(input)?
    };
    write_project_to_rbxmx(&project, output)
}

pub fn write_project_to_rbxmx(project: &LoadedProject, output: &Path) -> Result<()> {
    let xml = project_to_rbxmx(project)?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    fs::write(output, xml).map_err(io_error)
}

pub fn project_to_rbxmx(project: &LoadedProject) -> Result<String> {
    let layout = project.layout()?;
    Ok(layout_to_rbxmx(&layout))
}

fn layout_to_rbxlx(layout: &ProjectLayout) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<roblox version=\"4\">\n");
    out.push_str("  <External>null</External>\n");
    out.push_str("  <External>null</External>\n");

    let mut referents = ReferentAllocator::default();
    for mount in &layout.top_level {
        match mount {
            ProjectMount::DataModelChild(node) => {
                write_node(&mut out, node, 1, &mut referents);
            }
            ProjectMount::ServiceContents {
                service_name,
                children,
            } => {
                let service_node = ProjectNode {
                    name: service_name.clone(),
                    class_name: service_name.clone(),
                    source: None,
                    run_context: None,
                    value: None,
                    script_path: None,
                    auto_run: false,
                    children: children.clone(),
                };
                write_node(&mut out, &service_node, 1, &mut referents);
            }
        }
    }

    out.push_str("</roblox>\n");
    out
}

fn layout_to_rbxmx(layout: &ProjectLayout) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<roblox version=\"4\">\n");
    out.push_str("  <External>null</External>\n");
    out.push_str("  <External>null</External>\n");

    let mut referents = ReferentAllocator::default();
    for mount in &layout.top_level {
        match mount {
            ProjectMount::DataModelChild(node) => {
                write_node(&mut out, node, 1, &mut referents);
            }
            ProjectMount::ServiceContents {
                service_name: _,
                children,
            } => {
                for child in children {
                    write_node(&mut out, child, 1, &mut referents);
                }
            }
        }
    }

    out.push_str("</roblox>\n");
    out
}

#[derive(Default)]
struct ReferentAllocator {
    next_id: usize,
}

impl ReferentAllocator {
    fn next(&mut self) -> String {
        self.next_id += 1;
        format!("RBX{}", self.next_id)
    }
}

fn write_node(
    out: &mut String,
    node: &ProjectNode,
    depth: usize,
    referents: &mut ReferentAllocator,
) {
    let indent = "  ".repeat(depth);
    let referent = referents.next();
    out.push_str(&format!(
        "{indent}<Item class=\"{}\" referent=\"{}\">\n",
        xml_escape(&node.class_name),
        referent
    ));
    out.push_str(&format!("{indent}  <Properties>\n"));
    write_string_property(out, depth + 2, "Name", &node.name);
    if let Some(source) = &node.source {
        write_protected_string_property(out, depth + 2, "Source", source);
    }
    if let Some(run_context) = &node.run_context {
        write_string_property(out, depth + 2, "RunContext", run_context);
    }
    if let Some(value) = &node.value {
        if let Ok(value_text) = std::str::from_utf8(value) {
            write_string_property(out, depth + 2, "Value", value_text);
        }
    }
    out.push_str(&format!("{indent}  </Properties>\n"));
    for child in &node.children {
        write_node(out, child, depth + 1, referents);
    }
    out.push_str(&format!("{indent}</Item>\n"));
}

fn write_string_property(out: &mut String, depth: usize, name: &str, value: &str) {
    let indent = "  ".repeat(depth);
    out.push_str(&format!(
        "{indent}<string name=\"{}\">{}</string>\n",
        xml_escape(name),
        xml_escape(value)
    ));
}

fn write_protected_string_property(out: &mut String, depth: usize, name: &str, value: &str) {
    let indent = "  ".repeat(depth);
    out.push_str(&format!(
        "{indent}<ProtectedString name=\"{}\"><![CDATA[{}]]></ProtectedString>\n",
        xml_escape(name),
        escape_cdata(value)
    ));
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn escape_cdata(value: &str) -> String {
    value.replace("]]>", "]]]]><![CDATA[>")
}

fn io_error(error: std::io::Error) -> Error {
    Error::RuntimeError(format!("I/O error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::project_to_rbxlx;
    use crate::project::{LoadedProject, ProjectFile};
    use std::path::PathBuf;

    #[test]
    fn exports_init_directory_as_module_script() {
        let project = LoadedProject {
            files: vec![
                ProjectFile {
                    relative_path: PathBuf::from("ReplicatedStorage/Foo/init.luau"),
                    bytes: b"return {}".to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("ReplicatedStorage/Foo/Child.luau"),
                    bytes: b"return 5".to_vec(),
                },
            ],
        };

        let xml = project_to_rbxlx(&project).expect("xml");
        assert!(xml.contains("<Item class=\"ReplicatedStorage\""));
        assert!(xml.contains("<Item class=\"ModuleScript\""));
        assert!(xml.contains("<string name=\"Name\">Foo</string>"));
        assert!(xml.contains("<string name=\"Name\">Child</string>"));
    }
}
