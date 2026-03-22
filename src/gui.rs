use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui::{
    self, CentralPanel, Color32, Context, CursorIcon, Rect, RichText, Sense, SidePanel,
    TextEdit, TopBottomPanel, pos2,
};
use eframe::{App, Frame, NativeOptions};
use mlua::{Error, Result};
use rfd::FileDialog;

use crate::image;
use crate::project::{LoadedProject, ProjectFile, ProjectScript, ScriptKind};
use crate::rbxlx;

pub fn run() -> Result<()> {
    let options = NativeOptions::default();
    eframe::run_native(
        "RLE",
        options,
        Box::new(|cc| Box::new(RleGuiApp::new(cc))),
    )
    .map_err(|error| Error::RuntimeError(format!("Could not launch GUI: {error}")))
}

struct RleGuiApp {
    project: Option<OpenProject>,
    console: Vec<ConsoleEntry>,
    console_tx: Sender<ConsoleEntry>,
    console_rx: Receiver<ConsoleEntry>,
    running: Option<RunningProcess>,
    bottom_tab: BottomPanelTab,
    bottom_panel_height: f32,
    theme: ThemeMode,
    add_script_dialog: Option<AddScriptDialog>,
}

impl RleGuiApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (console_tx, console_rx) = mpsc::channel();
        Self {
            project: None,
            console: Vec::new(),
            console_tx,
            console_rx,
            running: None,
            bottom_tab: BottomPanelTab::Console,
            bottom_panel_height: 220.0,
            theme: ThemeMode::Dark,
            add_script_dialog: None,
        }
    }

    fn drain_console(&mut self) {
        while let Ok(entry) = self.console_rx.try_recv() {
            self.console.push(entry);
            if self.console.len() > 2_000 {
                let overflow = self.console.len() - 2_000;
                self.console.drain(0..overflow);
            }
        }
    }

    fn log_system(&self, text: impl Into<String>) {
        let _ = self
            .console_tx
            .send(ConsoleEntry::new(ConsoleStream::System, text.into()));
    }

    fn apply_theme(&self, ctx: &Context) {
        let visuals = match self.theme {
            ThemeMode::Dark => egui::Visuals::dark(),
            ThemeMode::Light => egui::Visuals::light(),
        };
        ctx.set_visuals(visuals);
        let mut style = (*ctx.style()).clone();
        style.visuals.window_rounding = egui::Rounding::same(12.0);
        style.visuals.menu_rounding = egui::Rounding::same(12.0);
        style.visuals.widgets.noninteractive.rounding = egui::Rounding::same(10.0);
        style.visuals.widgets.inactive.rounding = egui::Rounding::same(10.0);
        style.visuals.widgets.hovered.rounding = egui::Rounding::same(10.0);
        style.visuals.widgets.active.rounding = egui::Rounding::same(10.0);
        style.visuals.widgets.open.rounding = egui::Rounding::same(10.0);
        ctx.set_style(style);
    }

    fn open_folder(&mut self) {
        let Some(path) = FileDialog::new().pick_folder() else {
            return;
        };
        match OpenProject::from_directory(path.clone()) {
            Ok(project) => {
                self.stop_process();
                self.project = Some(project);
                self.log_system(format!("Opened project folder {}", path.display()));
            }
            Err(error) => {
                self.log_system(format!("Could not open folder {}: {error}", path.display()))
            }
        }
    }

    fn open_image(&mut self) {
        let Some(path) = FileDialog::new()
            .add_filter("RLE Images", &["rleimg"])
            .pick_file()
        else {
            return;
        };
        match OpenProject::from_image(path.clone()) {
            Ok(project) => {
                self.stop_process();
                self.project = Some(project);
                self.log_system(format!(
                    "Opened image {} in-memory without unpacking it.",
                    path.display()
                ));
            }
            Err(error) => {
                self.log_system(format!("Could not open image {}: {error}", path.display()))
            }
        }
    }

    fn save_project(&mut self) {
        let Some(project) = self.project.as_mut() else {
            self.log_system("No project is open.");
            return;
        };
        project.sync_tabs_into_project();
        match project.save() {
            Ok(message) => self.log_system(message),
            Err(error) => self.log_system(format!("Save failed: {error}")),
        }
    }

    fn save_image_as(&mut self) {
        let Some(project) = self.project.as_mut() else {
            self.log_system("No project is open.");
            return;
        };
        project.sync_tabs_into_project();
        let default_name = project.default_image_name();
        let Some(path) = FileDialog::new()
            .add_filter("RLE Images", &["rleimg"])
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };
        match image::write_project_image(&project.project, &path) {
            Ok(()) => self.log_system(format!("Wrote image {}", path.display())),
            Err(error) => {
                self.log_system(format!("Could not write image {}: {error}", path.display()))
            }
        }
    }

    fn export_rbxlx(&mut self) {
        let Some(project) = self.project.as_mut() else {
            self.log_system("No project is open.");
            return;
        };
        project.sync_tabs_into_project();
        let Some(path) = FileDialog::new()
            .add_filter("Roblox Place", &["rbxlx"])
            .set_file_name(&project.default_rbxlx_name())
            .save_file()
        else {
            return;
        };
        match rbxlx::write_project_to_rbxlx(&project.project, &path) {
            Ok(()) => self.log_system(format!("Exported RBXLX to {}", path.display())),
            Err(error) => self.log_system(format!("Could not export RBXLX: {error}")),
        }
    }

    fn open_active_tab_in_vscode(&mut self) {
        let Some(project) = self.project.as_ref() else {
            self.log_system("No project is open.");
            return;
        };
        let Some(tab) = project.active_tab() else {
            self.log_system("Open a script tab first.");
            return;
        };
        let Some(path) = &tab.absolute_path else {
            self.log_system("Open in VS Code is only available for folder-backed scripts.");
            return;
        };
        match Command::new("code").arg(path).spawn() {
            Ok(_) => self.log_system(format!("Opened {} in VS Code.", path.display())),
            Err(error) => self.log_system(format!("Could not launch VS Code: {error}")),
        }
    }

    fn show_add_script_dialog(&mut self) {
        let Some(project) = self.project.as_ref() else {
            self.log_system("Open a project first.");
            return;
        };
        let target = project.default_add_target();
        self.add_script_dialog = Some(AddScriptDialog {
            target_label: target.label,
            container_path: target.container_path,
            kind: ScriptKind::ModuleScript,
            name: "NewScript".to_string(),
        });
    }

    fn confirm_add_script(&mut self) {
        let Some(dialog) = self.add_script_dialog.take() else {
            return;
        };
        let result = if let Some(project) = self.project.as_mut() {
            project.add_script(dialog.container_path, dialog.kind, dialog.name.clone())
        } else {
            return;
        };
        match result {
            Ok(path) => {
                self.log_system(format!("Added script {}", path.display()));
                if let Some(project) = self.project.as_mut() {
                    project.open_tab(&path);
                }
            }
            Err(error) => self.log_system(format!("Could not add script {}: {error}", dialog.name)),
        }
    }

    fn run_project(&mut self, mode: RunMode) {
        if self.project.is_none() {
            self.log_system("Open a project or image first.");
            return;
        }
        if let Some(project) = self.project.as_mut() {
            project.sync_tabs_into_project();
        }
        self.stop_process();

        let temp_path = match temp_image_path(mode) {
            Ok(path) => path,
            Err(error) => {
                self.log_system(format!("Could not prepare a run image: {error}"));
                return;
            }
        };
        let write_result = if let Some(project) = self.project.as_ref() {
            image::write_project_image(&project.project, &temp_path)
        } else {
            return;
        };
        if let Err(error) = write_result {
            self.log_system(format!("Could not stage run image: {error}"));
            return;
        }

        let exe = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                self.log_system(format!("Could not resolve current executable: {error}"));
                return;
            }
        };

        let command_name = match mode {
            RunMode::Server => "run-server",
            RunMode::Client => "emulate-client",
        };

        let mut child = match Command::new(exe)
            .arg(command_name)
            .arg(&temp_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_file(&temp_path);
                self.log_system(format!("Could not start {command_name}: {error}"));
                return;
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let child = Arc::new(Mutex::new(child));
        if let Some(stdout) = stdout {
            spawn_console_reader(stdout, ConsoleStream::Stdout, self.console_tx.clone());
        }
        if let Some(stderr) = stderr {
            spawn_console_reader(stderr, ConsoleStream::Stderr, self.console_tx.clone());
        }

        self.log_system(match mode {
            RunMode::Server => "Started server session.",
            RunMode::Client => "Started client emulation session.",
        });
        self.running = Some(RunningProcess {
            child,
            mode,
            temp_path,
        });
    }

    fn stop_process(&mut self) {
        let Some(mut running) = self.running.take() else {
            return;
        };
        running.stop();
        self.log_system("Stopped running session.");
    }

    fn poll_process(&mut self) {
        let Some((label, status, temp_path)) = self.running.as_mut().and_then(|running| {
            let status = match running.child.lock() {
                Ok(mut child) => child.try_wait().ok().flatten(),
                Err(_) => None,
            }?;
            let label = match running.mode {
                RunMode::Server => "server",
                RunMode::Client => "client",
            };
            Some((label, status, running.temp_path.clone()))
        }) else {
            return;
        };

        self.log_system(format!("{label} session exited with {status}."));
        if self.running.is_some() {
            self.running = None;
            let _ = fs::remove_file(temp_path);
        }
    }

    fn render_top_bar(&mut self, ctx: &Context) {
        TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open Folder").clicked() {
                    self.open_folder();
                }
                if ui.button("Open Image").clicked() {
                    self.open_image();
                }
                if ui.button("Save").clicked() {
                    self.save_project();
                }
                if ui.button("Save Image As").clicked() {
                    self.save_image_as();
                }
                if ui.button("Export RBXLX").clicked() {
                    self.export_rbxlx();
                }
                ui.separator();
                if ui.button("Run Server").clicked() {
                    self.run_project(RunMode::Server);
                }
                if ui.button("Emulate Client").clicked() {
                    self.run_project(RunMode::Client);
                }
                if ui.button("Stop").clicked() {
                    self.stop_process();
                }
                ui.separator();
                if ui.button("Add Instance").clicked() {
                    self.show_add_script_dialog();
                }
                if ui.button("Open in VS Code").clicked() {
                    self.open_active_tab_in_vscode();
                }
                ui.separator();
                ui.label("Theme");
                ui.selectable_value(&mut self.theme, ThemeMode::Dark, "Dark");
                ui.selectable_value(&mut self.theme, ThemeMode::Light, "Light");
            });
        });
    }

    fn render_explorer(&mut self, ctx: &Context) {
        SidePanel::left("explorer")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.heading("Explorer");
                ui.separator();
                if let Some(project) = self.project.as_mut() {
                    ui.label(RichText::new(&project.title).strong());
                    ui.small(project.subtitle());
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for node in project.explorer.clone() {
                            render_explorer_node(ui, project, &node);
                        }
                    });
                } else {
                    ui.label("Open a folder or .rleimg image to start.");
                }
            });
    }

    fn render_editor_and_bottom(&mut self, ctx: &Context) {
        CentralPanel::default().show(ctx, |ui| {
            let available = ui.available_rect_before_wrap();
            let splitter_height = 8.0;
            let min_bottom_height = 120.0;
            let min_editor_height = 180.0;
            let max_bottom_height =
                (available.height() - min_editor_height - splitter_height).max(min_bottom_height);

            self.bottom_panel_height = self
                .bottom_panel_height
                .clamp(min_bottom_height, max_bottom_height);

            let editor_height =
                (available.height() - self.bottom_panel_height - splitter_height).max(min_editor_height);
            let bottom_height = (available.height() - editor_height - splitter_height)
                .clamp(min_bottom_height, max_bottom_height);
            self.bottom_panel_height = bottom_height;

            let editor_rect = Rect::from_min_max(
                available.min,
                pos2(available.max.x, available.min.y + editor_height),
            );
            let splitter_rect = Rect::from_min_max(
                pos2(available.min.x, editor_rect.max.y),
                pos2(available.max.x, editor_rect.max.y + splitter_height),
            );
            let bottom_rect = Rect::from_min_max(
                pos2(available.min.x, splitter_rect.max.y),
                available.max,
            );

            let splitter_response = ui
                .interact(
                    splitter_rect,
                    ui.id().with("editor_bottom_splitter"),
                    Sense::click_and_drag(),
                )
                .on_hover_cursor(CursorIcon::ResizeVertical);

            if splitter_response.dragged() {
                let pointer_delta_y = ctx.input(|input| input.pointer.delta().y);
                self.bottom_panel_height =
                    (self.bottom_panel_height - pointer_delta_y).clamp(min_bottom_height, max_bottom_height);
            }

            ui.painter()
                .rect_filled(splitter_rect.shrink2(egui::vec2(0.0, 2.0)), 4.0, ui.visuals().widgets.inactive.bg_fill);

            ui.allocate_ui_at_rect(editor_rect, |ui| {
                if let Some(project) = self.project.as_mut() {
                    render_editor(ui, project, &self.console_tx);
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.heading("RLE");
                        ui.label("Explorer on the left, code tabs here, console below.");
                    });
                }
            });

            ui.allocate_ui_at_rect(bottom_rect, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_size(bottom_rect.size());
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.bottom_tab, BottomPanelTab::Console, "Console");
                        ui.selectable_value(
                            &mut self.bottom_tab,
                            BottomPanelTab::ScreenGui,
                            "ScreenGui",
                        );
                    });
                    ui.separator();
                    match self.bottom_tab {
                        BottomPanelTab::Console => self.render_console(ui),
                        BottomPanelTab::ScreenGui => {
                            ui.heading("ScreenGui");
                            ui.label("Reserved for future ScreenGui rendering.");
                        }
                    }
                });
            });
        });
    }

    fn render_console(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for entry in &self.console {
                    let color = match entry.stream {
                        ConsoleStream::Stdout => Color32::from_rgb(170, 225, 170),
                        ConsoleStream::Stderr => Color32::from_rgb(255, 170, 170),
                        ConsoleStream::System => ui.visuals().text_color(),
                    };
                    ui.label(
                        RichText::new(format!("[{}] {}", entry.stream.label(), entry.text))
                            .monospace()
                            .color(color),
                    );
                }
            });
    }

    fn render_add_script_window(&mut self, ctx: &Context) {
        let mut open = self.add_script_dialog.is_some();
        if !open {
            return;
        }

        let mut create_requested = false;
        let mut cancel_requested = false;
        egui::Window::new("Add Instance")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                let Some(dialog) = self.add_script_dialog.as_mut() else {
                    return;
                };
                ui.label(format!("Add under {}", dialog.target_label));
                ui.separator();
                ui.label("Type");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut dialog.kind, ScriptKind::ModuleScript, "ModuleScript");
                    ui.selectable_value(&mut dialog.kind, ScriptKind::ServerScript, "Script");
                    ui.selectable_value(&mut dialog.kind, ScriptKind::LocalScript, "LocalScript");
                });
                ui.label("Name");
                ui.text_edit_singleline(&mut dialog.name);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Create").clicked() {
                        create_requested = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_requested = true;
                    }
                });
            });

        if create_requested {
            self.confirm_add_script();
            open = false;
        }
        if cancel_requested || !open {
            self.add_script_dialog = None;
        }
    }
}

impl App for RleGuiApp {
    fn update(&mut self, ctx: &Context, _frame: &mut Frame) {
        self.apply_theme(ctx);
        self.drain_console();
        self.poll_process();
        self.render_top_bar(ctx);
        self.render_explorer(ctx);
        self.render_editor_and_bottom(ctx);
        self.render_add_script_window(ctx);
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

impl Drop for RleGuiApp {
    fn drop(&mut self) {
        self.stop_process();
    }
}

#[derive(Clone)]
struct OpenProject {
    title: String,
    source: ProjectSource,
    project: LoadedProject,
    explorer: Vec<ExplorerNode>,
    selected_key: Option<String>,
    open_tabs: Vec<EditorTab>,
    active_tab: Option<usize>,
}

impl OpenProject {
    fn from_directory(root: PathBuf) -> Result<Self> {
        let project = LoadedProject::from_path(&root)?;
        let title = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_string();
        let source = ProjectSource::Directory { root };
        let explorer = build_explorer(&project, &source)?;
        Ok(Self {
            title,
            source,
            project,
            explorer,
            selected_key: None,
            open_tabs: Vec::new(),
            active_tab: None,
        })
    }

    fn from_image(path: PathBuf) -> Result<Self> {
        let project = image::read_project_image(&path)?;
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("image")
            .to_string();
        let source = ProjectSource::Image { path };
        let explorer = build_explorer(&project, &source)?;
        Ok(Self {
            title,
            source,
            project,
            explorer,
            selected_key: None,
            open_tabs: Vec::new(),
            active_tab: None,
        })
    }

    fn subtitle(&self) -> String {
        match &self.source {
            ProjectSource::Directory { root } => format!("Folder-backed: {}", root.display()),
            ProjectSource::Image { path } => {
                format!("Image-backed: {} (opened in-memory)", path.display())
            }
        }
    }

    fn active_tab(&self) -> Option<&EditorTab> {
        self.active_tab.and_then(|index| self.open_tabs.get(index))
    }

    fn open_tab(&mut self, relative_path: &Path) {
        if let Some(index) = self
            .open_tabs
            .iter()
            .position(|tab| tab.relative_path == relative_path)
        {
            self.active_tab = Some(index);
            return;
        }

        let Some(file) = self
            .project
            .files
            .iter()
            .find(|file| file.relative_path == relative_path)
        else {
            return;
        };

        let buffer = String::from_utf8_lossy(&file.bytes).to_string();
        let absolute_path = self.absolute_path(relative_path);
        self.open_tabs.push(EditorTab {
            title: file
                .relative_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("script")
                .to_string(),
            relative_path: file.relative_path.clone(),
            absolute_path,
            buffer,
            dirty: false,
        });
        self.active_tab = Some(self.open_tabs.len() - 1);
    }

    fn absolute_path(&self, relative_path: &Path) -> Option<PathBuf> {
        match &self.source {
            ProjectSource::Directory { root } => Some(root.join(relative_path)),
            ProjectSource::Image { .. } => None,
        }
    }

    fn sync_tabs_into_project(&mut self) {
        for tab in &mut self.open_tabs {
            upsert_project_file(
                &mut self.project,
                tab.relative_path.clone(),
                tab.buffer.clone().into_bytes(),
            );
            tab.dirty = false;
        }
    }

    fn save(&mut self) -> Result<String> {
        self.sync_tabs_into_project();
        match &self.source {
            ProjectSource::Directory { root } => {
                for file in &self.project.files {
                    let destination = root.join(&file.relative_path);
                    if let Some(parent) = destination.parent() {
                        fs::create_dir_all(parent).map_err(io_error)?;
                    }
                    fs::write(&destination, &file.bytes).map_err(io_error)?;
                }
                Ok(format!("Saved project files to {}", root.display()))
            }
            ProjectSource::Image { path } => {
                image::write_project_image(&self.project, path)?;
                Ok(format!("Saved image {}", path.display()))
            }
        }
    }

    fn default_image_name(&self) -> String {
        match &self.source {
            ProjectSource::Directory { root } => format!(
                "{}.rleimg",
                root.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("project")
            ),
            ProjectSource::Image { path } => path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("project.rleimg")
                .to_string(),
        }
    }

    fn default_rbxlx_name(&self) -> String {
        match &self.source {
            ProjectSource::Directory { root } => format!(
                "{}.rbxlx",
                root.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("project")
            ),
            ProjectSource::Image { path } => format!(
                "{}.rbxlx",
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("project")
            ),
        }
    }

    fn default_add_target(&self) -> AddTarget {
        if let Some(key) = &self.selected_key {
            if let Some(node) = find_node_by_key(&self.explorer, key) {
                return AddTarget {
                    label: node.tree_label(),
                    container_path: node.container_path.clone(),
                };
            }
        }

        AddTarget {
            label: "Workspace".to_string(),
            container_path: vec!["Workspace".to_string()],
        }
    }

    fn add_script(
        &mut self,
        container_path: Vec<String>,
        kind: ScriptKind,
        name: String,
    ) -> Result<PathBuf> {
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            return Err(Error::RuntimeError("Script name cannot be empty".to_string()));
        }

        let filename = match kind {
            ScriptKind::ModuleScript => format!("{trimmed_name}.luau"),
            ScriptKind::ServerScript => format!("{trimmed_name}.server.luau"),
            ScriptKind::LocalScript => format!("{trimmed_name}.client.luau"),
        };
        let mut relative_path = PathBuf::new();
        for segment in &container_path {
            relative_path.push(segment);
        }
        relative_path.push(filename);

        if self
            .project
            .files
            .iter()
            .any(|file| file.relative_path == relative_path)
        {
            return Err(Error::RuntimeError(format!(
                "{} already exists",
                relative_path.display()
            )));
        }

        let template = default_script_template(kind);
        upsert_project_file(
            &mut self.project,
            relative_path.clone(),
            template.as_bytes().to_vec(),
        );
        if let ProjectSource::Directory { root } = &self.source {
            let destination = root.join(&relative_path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            fs::write(&destination, template.as_bytes()).map_err(io_error)?;
        }

        self.explorer = build_explorer(&self.project, &self.source)?;
        self.selected_key = Some(format!("script:{}", relative_path.to_string_lossy()));
        Ok(relative_path)
    }
}

#[derive(Clone)]
enum ProjectSource {
    Directory { root: PathBuf },
    Image { path: PathBuf },
}

#[derive(Clone)]
struct EditorTab {
    title: String,
    relative_path: PathBuf,
    absolute_path: Option<PathBuf>,
    buffer: String,
    dirty: bool,
}

#[derive(Clone)]
struct ExplorerNode {
    key: String,
    title: String,
    kind: ExplorerNodeKind,
    container_path: Vec<String>,
    relative_path: Option<PathBuf>,
    children: Vec<ExplorerNode>,
}

impl ExplorerNode {
    fn tree_label(&self) -> String {
        match self.kind {
            ExplorerNodeKind::Service => format!("{} [Service]", self.title),
            ExplorerNodeKind::Folder => format!("{} [Folder]", self.title),
            ExplorerNodeKind::ModuleFolder => format!("{} [ModuleScript]", self.title),
            ExplorerNodeKind::Script(kind) => format!("{} [{}]", self.title, script_kind_name(kind)),
        }
    }
}

#[derive(Clone, Copy)]
enum ExplorerNodeKind {
    Service,
    Folder,
    ModuleFolder,
    Script(ScriptKind),
}

#[derive(Clone)]
struct AddTarget {
    label: String,
    container_path: Vec<String>,
}

struct AddScriptDialog {
    target_label: String,
    container_path: Vec<String>,
    kind: ScriptKind,
    name: String,
}

#[derive(Clone)]
struct RunningProcess {
    child: Arc<Mutex<Child>>,
    mode: RunMode,
    temp_path: PathBuf,
}

impl RunningProcess {
    fn stop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_file(&self.temp_path);
    }
}

#[derive(Clone, Copy)]
enum RunMode {
    Server,
    Client,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BottomPanelTab {
    Console,
    ScreenGui,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ThemeMode {
    Dark,
    Light,
}

#[derive(Clone)]
struct ConsoleEntry {
    stream: ConsoleStream,
    text: String,
}

impl ConsoleEntry {
    fn new(stream: ConsoleStream, text: String) -> Self {
        Self { stream, text }
    }
}

#[derive(Clone, Copy)]
enum ConsoleStream {
    Stdout,
    Stderr,
    System,
}

impl ConsoleStream {
    fn label(&self) -> &'static str {
        match self {
            ConsoleStream::Stdout => "out",
            ConsoleStream::Stderr => "err",
            ConsoleStream::System => "rle",
        }
    }
}

#[derive(Clone, Default)]
struct ExplorerDir {
    directories: BTreeMap<String, ExplorerDir>,
    scripts: Vec<ProjectScript>,
}

fn render_explorer_node(ui: &mut egui::Ui, project: &mut OpenProject, node: &ExplorerNode) {
    if node.children.is_empty() {
        let selected = project.selected_key.as_ref() == Some(&node.key);
        if ui.selectable_label(selected, node.tree_label()).clicked() {
            project.selected_key = Some(node.key.clone());
            if let Some(relative_path) = &node.relative_path {
                project.open_tab(relative_path);
            }
        }
        return;
    }

    let response = egui::CollapsingHeader::new(node.tree_label())
        .default_open(true)
        .show(ui, |ui| {
            for child in &node.children {
                render_explorer_node(ui, project, child);
            }
        });

    if response.header_response.clicked() {
        project.selected_key = Some(node.key.clone());
        if let Some(relative_path) = &node.relative_path {
            project.open_tab(relative_path);
        }
    }
}

fn render_editor(ui: &mut egui::Ui, project: &mut OpenProject, console_tx: &Sender<ConsoleEntry>) {
    ui.heading("Editor");
    ui.separator();

    if project.open_tabs.is_empty() {
        ui.label("Select a script from the explorer to open it.");
        return;
    }

    ui.horizontal_wrapped(|ui| {
        let mut close_index = None;
        for index in 0..project.open_tabs.len() {
            let selected = project.active_tab == Some(index);
            let title = if project.open_tabs[index].dirty {
                format!("{} *", project.open_tabs[index].title)
            } else {
                project.open_tabs[index].title.clone()
            };
            if ui.selectable_label(selected, title).clicked() {
                project.active_tab = Some(index);
            }
            if ui.small_button("x").clicked() {
                close_index = Some(index);
            }
        }
        if let Some(index) = close_index {
            project.open_tabs.remove(index);
            project.active_tab = match project.active_tab {
                Some(_) if project.open_tabs.is_empty() => None,
                Some(active) if active == index => Some(index.min(project.open_tabs.len() - 1)),
                Some(active) if active > index => Some(active - 1),
                other => other,
            };
        }
    });
    ui.separator();

    let Some(active_index) = project.active_tab else {
        ui.label("Open a tab to edit.");
        return;
    };

    let path_label = project.open_tabs[active_index]
        .relative_path
        .display()
        .to_string();
    let absolute_path = project.open_tabs[active_index].absolute_path.clone();
    ui.horizontal(|ui| {
        ui.label(RichText::new(path_label).monospace());
        if let Some(path) = &absolute_path {
            ui.small(path.display().to_string());
        } else {
            ui.small("image-backed");
        }
    });
    ui.separator();

    let edit = {
        let tab = &mut project.open_tabs[active_index];
        TextEdit::multiline(&mut tab.buffer)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY)
            .desired_rows(24)
    };
    if ui.add(edit).changed() {
        project.open_tabs[active_index].dirty = true;
    }

    ui.separator();
    if ui.button("Save Tab").clicked() {
        let relative_path = project.open_tabs[active_index].relative_path.clone();
        let bytes = project.open_tabs[active_index].buffer.clone().into_bytes();
        let absolute_path = project.open_tabs[active_index].absolute_path.clone();
        upsert_project_file(&mut project.project, relative_path.clone(), bytes.clone());
        match absolute_path {
            Some(path) => match fs::write(&path, &bytes) {
                Ok(()) => {
                    project.open_tabs[active_index].dirty = false;
                    let _ = console_tx.send(ConsoleEntry::new(
                        ConsoleStream::System,
                        format!("Saved {}", path.display()),
                    ));
                }
                Err(error) => {
                    let _ = console_tx.send(ConsoleEntry::new(
                        ConsoleStream::System,
                        format!("Could not save {}: {error}", path.display()),
                    ));
                }
            },
            None => {
                project.open_tabs[active_index].dirty = false;
                let _ = console_tx.send(ConsoleEntry::new(
                    ConsoleStream::System,
                    format!("Updated {} in-memory.", relative_path.display()),
                ));
            }
        }
    }
}

fn build_explorer(project: &LoadedProject, source: &ProjectSource) -> Result<Vec<ExplorerNode>> {
    let mut root = ExplorerDir::default();
    for script in project.scripts()? {
        let container_path = script.container_path.clone();
        insert_script(&mut root, &container_path, script);
    }

    let mut nodes = Vec::new();
    for script in root
        .scripts
        .iter()
        .filter(|script| !is_init_module(script))
        .cloned()
    {
        nodes.push(script_to_node(&script, source));
    }

    for (name, directory) in root.directories {
        let mut path = vec![name.clone()];
        let node = if is_service_name(&name) {
            Some(ExplorerNode {
                key: format!("service:{name}"),
                title: name.clone(),
                kind: ExplorerNodeKind::Service,
                container_path: vec![name.clone()],
                relative_path: None,
                children: build_directory_children(&directory, source, &mut path, true),
            })
        } else {
            build_directory_node(name, directory, source, &mut path)
        };
        if let Some(node) = node {
            nodes.push(node);
        }
    }

    nodes.sort_by(|left, right| left.title.cmp(&right.title));
    Ok(nodes)
}

fn insert_script(root: &mut ExplorerDir, path: &[String], script: ProjectScript) {
    let mut current = root;
    for segment in path {
        current = current.directories.entry(segment.clone()).or_default();
    }
    current.scripts.push(script);
}

fn build_directory_node(
    name: String,
    directory: ExplorerDir,
    source: &ProjectSource,
    path: &mut Vec<String>,
) -> Option<ExplorerNode> {
    if let Some(init_script) = directory
        .scripts
        .iter()
        .find(|script| is_init_module(script))
        .cloned()
    {
        let children = build_directory_children(&directory, source, path, false);
        return Some(ExplorerNode {
            key: format!("script:{}", init_script.relative_path.to_string_lossy()),
            title: name,
            kind: ExplorerNodeKind::ModuleFolder,
            container_path: path.clone(),
            relative_path: Some(init_script.relative_path.clone()),
            children,
        });
    }

    let children = build_directory_children(&directory, source, path, false);
    if children.is_empty() {
        return None;
    }
    Some(ExplorerNode {
        key: format!("folder:{}", path_to_string(path)),
        title: name,
        kind: ExplorerNodeKind::Folder,
        container_path: path.clone(),
        relative_path: None,
        children,
    })
}

fn build_directory_children(
    directory: &ExplorerDir,
    source: &ProjectSource,
    path: &mut Vec<String>,
    skip_init_replacement: bool,
) -> Vec<ExplorerNode> {
    let mut children = Vec::new();

    for script in &directory.scripts {
        if is_init_module(script) && !skip_init_replacement {
            continue;
        }
        children.push(script_to_node(script, source));
    }

    for (name, child_dir) in &directory.directories {
        path.push(name.clone());
        if let Some(node) = build_directory_node(name.clone(), child_dir.clone(), source, path) {
            children.push(node);
        }
        path.pop();
    }

    children.sort_by(|left, right| left.title.cmp(&right.title));
    children
}

fn script_to_node(script: &ProjectScript, _source: &ProjectSource) -> ExplorerNode {
    ExplorerNode {
        key: format!("script:{}", script.relative_path.to_string_lossy()),
        title: script.name.clone(),
        kind: ExplorerNodeKind::Script(script.kind),
        container_path: script.container_path.clone(),
        relative_path: Some(script.relative_path.clone()),
        children: Vec::new(),
    }
}

fn find_node_by_key<'a>(nodes: &'a [ExplorerNode], key: &str) -> Option<&'a ExplorerNode> {
    for node in nodes {
        if node.key == key {
            return Some(node);
        }
        if let Some(found) = find_node_by_key(&node.children, key) {
            return Some(found);
        }
    }
    None
}

fn path_to_string(path: &[String]) -> String {
    path.join("/")
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

fn script_kind_name(kind: ScriptKind) -> &'static str {
    match kind {
        ScriptKind::ModuleScript => "ModuleScript",
        ScriptKind::ServerScript => "Script",
        ScriptKind::LocalScript => "LocalScript",
    }
}

fn default_script_template(kind: ScriptKind) -> &'static str {
    match kind {
        ScriptKind::ModuleScript => "local module = {}\n\nreturn module\n",
        ScriptKind::ServerScript => "print(\"server script booted\")\n",
        ScriptKind::LocalScript => "print(\"client script booted\")\n",
    }
}

fn upsert_project_file(project: &mut LoadedProject, relative_path: PathBuf, bytes: Vec<u8>) {
    if let Some(file) = project
        .files
        .iter_mut()
        .find(|file| file.relative_path == relative_path)
    {
        file.bytes = bytes;
        return;
    }
    project.files.push(ProjectFile {
        relative_path,
        bytes,
    });
    project
        .files
        .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
}

fn spawn_console_reader<T: std::io::Read + Send + 'static>(
    stream: T,
    kind: ConsoleStream,
    tx: Sender<ConsoleEntry>,
) {
    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    let _ = tx.send(ConsoleEntry::new(kind, line));
                }
                Err(error) => {
                    let _ = tx.send(ConsoleEntry::new(
                        ConsoleStream::System,
                        format!("Console reader error: {error}"),
                    ));
                    break;
                }
            }
        }
    });
}

fn temp_image_path(mode: RunMode) -> Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::RuntimeError(format!("System clock error: {error}")))?
        .as_millis();
    let label = match mode {
        RunMode::Server => "server",
        RunMode::Client => "client",
    };
    Ok(std::env::temp_dir().join(format!("rle-gui-{label}-{stamp}.rleimg")))
}

fn io_error(error: std::io::Error) -> Error {
    Error::RuntimeError(format!("I/O error: {error}"))
}
