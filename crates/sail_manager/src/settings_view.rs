use anyhow::{Result, ensure};
use collections::HashMap;
use gpui::{
    AnyElement, Entity, FontWeight, ScrollHandle, SharedString, Subscription, Task, WeakEntity,
};
use project::Project;
use settings::{Settings, SettingsLocation, SettingsStore};
use ui::{
    Checkbox, ContextMenu, DropdownMenu, DropdownStyle, Tooltip, WithScrollbar, prelude::*, v_flex,
};
use ui_input::{ErasedEditorEvent, InputField};
use util::ResultExt as _;
use util::rel_path::RelPath;
use worktree::WorktreeId;

use crate::compose::{self, NodePackageManager, RefreshMode, ShellPreference};
use crate::project_settings;
use crate::sail_manager_panel::{PortRow, SailManagerPanel};
use crate::sail_manager_settings::SailManagerSettings;

pub fn validate_url_host(input: &str) -> Result<String> {
    let trimmed = input.trim();
    ensure!(!trimmed.is_empty(), "The host cannot be empty");
    ensure!(
        !trimmed
            .chars()
            .any(|character| character.is_whitespace() || character == '/'),
        "The host cannot contain spaces or \"/\""
    );
    Ok(trimmed.to_string())
}

pub fn validate_compose_file(input: &str, existing: &[String]) -> Result<String> {
    let trimmed = input.trim();
    ensure!(!trimmed.is_empty(), "The file name cannot be empty");
    ensure!(
        !existing.iter().any(|file| file == trimmed),
        "{trimmed:?} is already in the list"
    );
    Ok(trimmed.to_string())
}

pub fn validate_port_pin(input: &str) -> Result<Option<u16>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let port: u16 = trimmed
        .parse()
        .map_err(|_| anyhow::anyhow!("The port must be a number between 1 and 65535"))?;
    ensure!(port != 0, "The port must be a number between 1 and 65535");
    Ok(Some(port))
}

pub struct SettingsView {
    panel: WeakEntity<SailManagerPanel>,
    project: Entity<Project>,
    worktree_id: WorktreeId,
    compose_input: Entity<InputField>,
    host_input: Entity<InputField>,
    service_input: Entity<InputField>,
    pin_inputs: HashMap<String, Entity<InputField>>,
    scroll_handle: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(
        panel: WeakEntity<SailManagerPanel>,
        project: Entity<Project>,
        worktree_id: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let compose_input = cx.new(|cx| InputField::new(window, cx, "docker/compose.yaml"));
        let host_input = cx.new(|cx| InputField::new(window, cx, "localhost"));
        let service_input = cx.new(|cx| InputField::new(window, cx, "laravel.test"));
        let mut view = Self {
            panel,
            project,
            worktree_id,
            compose_input,
            host_input,
            service_input,
            pin_inputs: HashMap::default(),
            scroll_handle: ScrollHandle::new(),
            _subscriptions: Vec::new(),
        };
        let (url_host, app_service) = {
            let settings = view.settings(cx);
            (settings.url_host.clone(), settings.app_service.clone())
        };
        let host_editor = view.host_input.read(cx).editor().clone();
        let service_editor = view.service_input.read(cx).editor().clone();
        host_editor.set_text(&url_host, window, cx);
        service_editor.set_text(&app_service, window, cx);

        let weak_view = cx.weak_entity();
        let host_blur = host_editor.subscribe(
            Box::new({
                let weak_view = weak_view.clone();
                move |event, _, cx| {
                    if event == ErasedEditorEvent::Blurred {
                        weak_view
                            .update(cx, |this, cx| this.save_url_host(cx))
                            .log_err();
                    }
                }
            }),
            window,
            cx,
        );
        let service_blur = service_editor.subscribe(
            Box::new(move |event, _, cx| {
                if event == ErasedEditorEvent::Blurred {
                    weak_view
                        .update(cx, |this, cx| this.save_app_service(cx))
                        .log_err();
                }
            }),
            window,
            cx,
        );
        let settings_subscription =
            cx.observe_global_in::<SettingsStore>(window, |this, window, cx| {
                this.sync_text_fields(window, cx);
                cx.notify();
            });
        view._subscriptions = vec![host_blur, service_blur, settings_subscription];
        view
    }

    #[cfg(test)]
    pub(crate) fn field_texts(&self, cx: &App) -> (String, String) {
        (
            self.host_input.read(cx).text(cx),
            self.service_input.read(cx).text(cx),
        )
    }

    fn sync_text_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (url_host, app_service) = {
            let settings = self.settings(cx);
            (settings.url_host.clone(), settings.app_service.clone())
        };
        for (input, value) in [
            (self.host_input.clone(), url_host),
            (self.service_input.clone(), app_service),
        ] {
            let editor = input.read(cx).editor().clone();
            if editor.focus_handle(cx).contains_focused(window, cx) || editor.text(cx) == value {
                continue;
            }
            editor.set_text(&value, window, cx);
            Self::set_field_error(&input, None, cx);
        }
    }

    fn settings<'a>(&self, cx: &'a App) -> &'a SailManagerSettings {
        SailManagerSettings::get(
            Some(SettingsLocation {
                worktree_id: self.worktree_id,
                path: RelPath::empty(),
            }),
            cx,
        )
    }

    fn save(&self, what: &'static str, task: Task<Result<()>>, cx: &mut Context<Self>) {
        let panel = self.panel.clone();
        cx.spawn(async move |_, cx| {
            if let Err(error) = task.await {
                panel
                    .update(cx, |panel, cx| panel.show_save_error(what, &error, cx))
                    .log_err();
            }
        })
        .detach();
    }

    fn set_field_error(input: &Entity<InputField>, error: Option<String>, cx: &mut Context<Self>) {
        input.update(cx, |input, cx| {
            input.set_error(error.map(SharedString::from), cx)
        });
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.panel
            .update(cx, |panel, cx| panel.close_settings(cx))
            .log_err();
    }

    fn save_url_host(&mut self, cx: &mut Context<Self>) {
        let text = self.host_input.read(cx).text(cx);
        if text == self.settings(cx).url_host {
            Self::set_field_error(&self.host_input, None, cx);
            return;
        }
        match validate_url_host(&text) {
            Err(error) => Self::set_field_error(&self.host_input, Some(error.to_string()), cx),
            Ok(host) => {
                Self::set_field_error(&self.host_input, None, cx);
                let task = project_settings::set_url_host(
                    self.project.clone(),
                    self.worktree_id,
                    host,
                    cx,
                );
                self.save("the URL host", task, cx);
            }
        }
    }

    fn save_app_service(&mut self, cx: &mut Context<Self>) {
        let text = self.service_input.read(cx).text(cx);
        let service = text.trim().to_string();
        if service == self.settings(cx).app_service {
            Self::set_field_error(&self.service_input, None, cx);
            return;
        }
        match compose::validate_service_name(&service) {
            Err(error) => Self::set_field_error(&self.service_input, Some(error.to_string()), cx),
            Ok(()) => {
                Self::set_field_error(&self.service_input, None, cx);
                self.set_app_service(service, cx);
            }
        }
    }

    fn set_app_service(&mut self, service: String, cx: &mut Context<Self>) {
        let task =
            project_settings::set_app_service(self.project.clone(), self.worktree_id, service, cx);
        self.save("the main service", task, cx);
    }

    fn add_compose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.settings(cx).compose_files.clone();
        let text = self.compose_input.read(cx).text(cx);
        match validate_compose_file(&text, &existing) {
            Err(error) => Self::set_field_error(&self.compose_input, Some(error.to_string()), cx),
            Ok(file) => {
                Self::set_field_error(&self.compose_input, None, cx);
                let editor = self.compose_input.read(cx).editor().clone();
                editor.clear(window, cx);
                let mut files = existing;
                files.push(file);
                self.save_compose_files(files, cx);
            }
        }
    }

    fn remove_compose_file(&mut self, index: usize, cx: &mut Context<Self>) {
        let mut files = self.settings(cx).compose_files.clone();
        if index >= files.len() {
            return;
        }
        files.remove(index);
        self.save_compose_files(files, cx);
    }

    fn save_compose_files(&mut self, files: Vec<String>, cx: &mut Context<Self>) {
        let task =
            project_settings::set_compose_files(self.project.clone(), self.worktree_id, files, cx);
        self.save("the compose files", task, cx);
    }

    fn set_shell(&mut self, shell: ShellPreference, cx: &mut Context<Self>) {
        let task = project_settings::set_shell(self.project.clone(), self.worktree_id, shell, cx);
        self.save("the shell", task, cx);
    }

    fn set_node_package_manager(
        &mut self,
        package_manager: Option<NodePackageManager>,
        cx: &mut Context<Self>,
    ) {
        let task = project_settings::set_node_package_manager(
            self.project.clone(),
            self.worktree_id,
            package_manager,
            cx,
        );
        self.save("the package manager", task, cx);
    }

    fn set_refresh_mode(&mut self, refresh_mode: RefreshMode, cx: &mut Context<Self>) {
        let task = project_settings::set_refresh_mode(
            self.project.clone(),
            self.worktree_id,
            refresh_mode,
            cx,
        );
        self.save("the refresh mode", task, cx);
    }

    fn set_auto_assign_ports(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let task = project_settings::set_auto_assign_ports(
            self.project.clone(),
            self.worktree_id,
            enabled,
            cx,
        );
        self.save("the port assignment", task, cx);
    }

    fn save_port_pin(&mut self, key: String, cx: &mut Context<Self>) {
        let Some(input) = self.pin_inputs.get(&key).cloned() else {
            return;
        };
        let text = input.read(cx).text(cx);
        match validate_port_pin(&text) {
            Ok(port) => {
                Self::set_field_error(&input, None, cx);
                if self.settings(cx).port_pins.get(&key).copied() == port {
                    return;
                }
                let task = project_settings::set_port_pin(
                    self.project.clone(),
                    self.worktree_id,
                    key,
                    port,
                    cx,
                );
                self.save("the port", task, cx);
            }
            Err(error) => Self::set_field_error(&input, Some(error.to_string()), cx),
        }
    }

    fn ensure_pin_inputs(&mut self, rows: &[PortRow], window: &mut Window, cx: &mut Context<Self>) {
        for row in rows {
            if self.pin_inputs.contains_key(&row.key) {
                continue;
            }
            let input = cx.new(|cx| InputField::new(window, cx, "auto").label_min_width(px(64.)));
            let editor = input.read(cx).editor().clone();
            if let Some(port) = self.settings(cx).port_pins.get(&row.key) {
                editor.set_text(&port.to_string(), window, cx);
            }
            let key = row.key.clone();
            let weak_view = cx.weak_entity();
            let subscription = editor.subscribe(
                Box::new(move |event, _, cx| {
                    if event == ErasedEditorEvent::Blurred {
                        let key = key.clone();
                        weak_view
                            .update(cx, |this, cx| this.save_port_pin(key, cx))
                            .log_err();
                    }
                }),
                window,
                cx,
            );
            self._subscriptions.push(subscription);
            self.pin_inputs.insert(row.key.clone(), input);
        }
    }

    fn render_port_assignments(
        &mut self,
        rows: Vec<PortRow>,
        supported: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_pin_inputs(&rows, window, cx);
        let auto_assign = self.settings(cx).auto_assign_ports;
        let header_cell =
            |text: &'static str| Label::new(text).size(LabelSize::XSmall).color(Color::Muted);
        let table = if rows.is_empty() {
            v_flex()
                .child(
                    Label::new("No published ports")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element()
        } else {
            let body = rows.into_iter().filter_map(|row| {
                let input = self.pin_inputs.get(&row.key)?.clone();
                Some(
                    h_flex()
                        .gap_2()
                        .child(
                            div().flex_1().min_w_0().child(
                                Label::new(row.service.clone())
                                    .size(LabelSize::Small)
                                    .truncate(),
                            ),
                        )
                        .child(
                            div().w(px(64.)).child(
                                Label::new(format!("{}/{}", row.container_port, row.protocol))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                        )
                        .child(
                            div().w(px(48.)).child(
                                Label::new(row.original_host_port.to_string())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                        )
                        .child(
                            h_flex()
                                .w(px(112.))
                                .gap_1()
                                .child(div().w(px(64.)).child(input))
                                .when(row.pinned, |this| {
                                    this.child(
                                        Label::new("pinned")
                                            .size(LabelSize::XSmall)
                                            .color(Color::Accent),
                                    )
                                })
                                .when(
                                    !row.pinned && row.host_port != row.original_host_port,
                                    |this| {
                                        this.child(
                                            Label::new(row.host_port.to_string())
                                                .size(LabelSize::XSmall)
                                                .color(Color::Warning),
                                        )
                                    },
                                ),
                        ),
                )
            });
            v_flex()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(header_cell("service")))
                        .child(div().w(px(64.)).child(header_cell("container")))
                        .child(div().w(px(48.)).child(header_cell("original")))
                        .child(div().w(px(112.)).child(header_cell("host"))),
                )
                .children(body)
                .into_any_element()
        };
        Self::render_field(
            "Port Assignments",
            v_flex()
                .gap_1p5()
                .child(
                    Checkbox::new(
                        "sail-manager-settings-auto-ports",
                        if auto_assign {
                            ToggleState::Selected
                        } else {
                            ToggleState::Unselected
                        },
                    )
                    .label("Assign free ports automatically")
                    .disabled(!supported)
                    .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                        this.set_auto_assign_ports(state.selected(), cx)
                    })),
                )
                .when(!supported, |this| {
                    this.child(
                        Label::new("Port assignment needs Docker Compose 2.24.4 or newer")
                            .size(LabelSize::XSmall)
                            .color(Color::Warning),
                    )
                })
                .child(table)
                .child(
                    Button::new("sail-manager-settings-reset-ports", "Reset Assignments")
                        .label_size(LabelSize::Small)
                        .size(ButtonSize::Compact)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.panel
                                .update(cx, |panel, cx| panel.reset_port_assignments(cx))
                                .log_err();
                        })),
                )
                .child(
                    Label::new(
                        "A `sail up` run outside Zed uses the original ports. Leave a host port empty to assign it automatically.",
                    )
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
                ),
        )
        .into_any_element()
    }

    fn render_field(label: &'static str, control: impl IntoElement) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(control)
    }

    fn render_choice_menu<T: Clone + 'static>(
        &self,
        id: &'static str,
        current_label: String,
        choices: Vec<(String, T)>,
        on_select: fn(&mut Self, T, &mut Context<Self>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> DropdownMenu {
        let view = cx.weak_entity();
        DropdownMenu::new(
            id,
            current_label,
            ContextMenu::build(window, cx, move |mut menu, _, _| {
                for (label, choice) in choices {
                    let view = view.clone();
                    menu = menu.entry(label, None, move |_, cx| {
                        let choice = choice.clone();
                        view.update(cx, |this, cx| on_select(this, choice, cx))
                            .log_err();
                    });
                }
                menu
            }),
        )
        .style(DropdownStyle::Outlined)
        .trigger_size(ButtonSize::Compact)
        .full_width(true)
    }

    fn render_compose_files(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let files = self.settings(cx).compose_files.clone();
        let element_background = cx.theme().colors().element_background;
        let rows = files
            .into_iter()
            .enumerate()
            .map(|(index, file)| {
                h_flex()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .justify_between()
                    .rounded_md()
                    .bg(element_background)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(file).size(LabelSize::Small).truncate()),
                    )
                    .child(
                        IconButton::new(
                            ("sail-manager-compose-file-remove", index),
                            IconName::Close,
                        )
                        .icon_size(IconSize::XSmall)
                        .tooltip(Tooltip::text("Remove"))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.remove_compose_file(index, cx)),
                        ),
                    )
            })
            .collect::<Vec<_>>();
        Self::render_field(
            "Compose Files",
            v_flex()
                .gap_1()
                .children(rows)
                .child(
                    h_flex()
                        .gap_1()
                        .items_start()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                                    this.add_compose_file(window, cx)
                                }))
                                .child(self.compose_input.clone()),
                        )
                        .child(
                            Button::new("sail-manager-compose-file-add", "Add")
                                .style(ButtonStyle::Outlined)
                                .label_size(LabelSize::Small)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_compose_file(window, cx)
                                })),
                        ),
                )
                .child(
                    Label::new("Leave empty to let Sail detect compose.yaml and its overrides.")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
    }

    fn render_main_service(
        &self,
        services: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let current = self.settings(cx).app_service.clone();
        let control = if services.is_empty() {
            div()
                .on_action(cx.listener(|this, _: &menu::Confirm, _, cx| this.save_app_service(cx)))
                .child(self.service_input.clone())
                .into_any_element()
        } else {
            let choices = services
                .into_iter()
                .map(|service| (service.clone(), service))
                .collect();
            self.render_choice_menu(
                "sail-manager-settings-main-service",
                current,
                choices,
                Self::set_app_service,
                window,
                cx,
            )
            .into_any_element()
        };
        Self::render_field("Main Service", control)
    }

    fn render_url_host(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_field(
            "URL Host",
            div()
                .on_action(cx.listener(|this, _: &menu::Confirm, _, cx| this.save_url_host(cx)))
                .child(self.host_input.clone()),
        )
    }

    fn render_shell(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = match self.settings(cx).shell {
            ShellPreference::Auto => "Auto",
            ShellPreference::Zsh => "zsh",
            ShellPreference::Bash => "bash",
            ShellPreference::Sh => "sh",
        };
        let choices = vec![
            ("Auto".to_string(), ShellPreference::Auto),
            ("zsh".to_string(), ShellPreference::Zsh),
            ("bash".to_string(), ShellPreference::Bash),
            ("sh".to_string(), ShellPreference::Sh),
        ];
        Self::render_field(
            "Preferred Shell",
            self.render_choice_menu(
                "sail-manager-settings-shell",
                current.to_string(),
                choices,
                Self::set_shell,
                window,
                cx,
            ),
        )
    }

    fn render_manual_urls(
        &self,
        services: Vec<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let manual_urls = self.settings(cx).manual_urls.clone();
        let rows = services
            .into_iter()
            .enumerate()
            .map(|(index, service)| {
                let url = manual_urls.get(&service).cloned();
                let service_for_click = service.clone();
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(Label::new(service).size(LabelSize::Small))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .justify_end()
                            .child(match url {
                                Some(url) => Label::new(url).size(LabelSize::Small).truncate(),
                                None => Label::new("(not set)")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            })
                            .child(
                                IconButton::new(
                                    ("sail-manager-manual-url-edit", index),
                                    IconName::Pencil,
                                )
                                .icon_size(IconSize::XSmall)
                                .tooltip(Tooltip::text("Configure URL"))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        let service = service_for_click.clone();
                                        this.panel
                                            .update(cx, |panel, cx| {
                                                panel.configure_url(&service, window, cx)
                                            })
                                            .log_err();
                                    },
                                )),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        let body = if rows.is_empty() {
            v_flex()
                .child(
                    Label::new("No services detected")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element()
        } else {
            v_flex().gap_0p5().children(rows).into_any_element()
        };
        Self::render_field("Manual URLs", body)
    }

    fn render_node_package_manager(
        &self,
        detected: Option<NodePackageManager>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let detected_label = detected.map_or("none", |manager| manager.program());
        let current = match self.settings(cx).node_package_manager {
            Some(manager) => manager.program().to_string(),
            None => format!("Auto ({detected_label})"),
        };
        let choices = vec![
            (format!("Auto ({detected_label})"), None),
            ("npm".to_string(), Some(NodePackageManager::Npm)),
            ("pnpm".to_string(), Some(NodePackageManager::Pnpm)),
            ("yarn".to_string(), Some(NodePackageManager::Yarn)),
            ("bun".to_string(), Some(NodePackageManager::Bun)),
        ];
        Self::render_field(
            "Node Package Manager",
            self.render_choice_menu(
                "sail-manager-settings-node-manager",
                current,
                choices,
                Self::set_node_package_manager,
                window,
                cx,
            ),
        )
    }

    fn render_refresh_mode(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label_for = |mode: RefreshMode| match mode {
            RefreshMode::EventsAndPoll => "Docker events + every 30 s",
            RefreshMode::Events => "Docker events only",
            RefreshMode::Manual => "Manual",
        };
        let current = label_for(self.settings(cx).refresh_mode);
        let choices = [
            RefreshMode::EventsAndPoll,
            RefreshMode::Events,
            RefreshMode::Manual,
        ]
        .into_iter()
        .map(|mode| (label_for(mode).to_string(), mode))
        .collect();
        Self::render_field(
            "Status Updates",
            self.render_choice_menu(
                "sail-manager-settings-refresh-mode",
                current.to_string(),
                choices,
                Self::set_refresh_mode,
                window,
                cx,
            ),
        )
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let context = self
            .panel
            .read_with(cx, |panel, cx| panel.settings_context(cx))
            .ok();
        let (services, detected, has_node, port_rows, ports_supported) = match context {
            Some(context) => (
                context.services,
                context.detected_package_manager,
                context.has_node,
                context.port_rows,
                context.ports_supported,
            ),
            None => (Vec::new(), None, false, Vec::new(), true),
        };
        let port_assignments = self.render_port_assignments(port_rows, ports_supported, window, cx);

        v_flex()
            .flex_1()
            .min_h_0()
            .size_full()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        Button::new("sail-manager-settings-back", "Back")
                            .start_icon(Icon::new(IconName::ArrowLeft).size(IconSize::Small))
                            .label_size(LabelSize::Small)
                            .size(ButtonSize::Compact)
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    )
                    .child(Label::new("Project Settings").weight(FontWeight::MEDIUM)),
            )
            .child(
                div()
                    .id("sail-manager-settings-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .child(
                        v_flex()
                            .p_2()
                            .gap_3()
                            .child(self.render_compose_files(cx))
                            .child(self.render_main_service(services.clone(), window, cx))
                            .child(self.render_url_host(cx))
                            .child(self.render_shell(window, cx))
                            .child(self.render_manual_urls(services, cx))
                            .when(has_node, |this| {
                                this.child(self.render_node_package_manager(detected, window, cx))
                            })
                            .child(self.render_refresh_mode(window, cx))
                            .child(port_assignments)
                            .child(
                                Label::new("Saved in .zed/settings.json of this project")
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            ),
                    ),
            )
            .vertical_scrollbar_for(&self.scroll_handle, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_url_host() {
        assert_eq!(validate_url_host("  blog.test ").unwrap(), "blog.test");
        assert_eq!(validate_url_host("127.0.0.1").unwrap(), "127.0.0.1");
        for invalid in ["", "   ", "blog test", "blog.test/app", "http://blog.test"] {
            assert!(validate_url_host(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn test_validate_port_pin() {
        assert_eq!(validate_port_pin("").unwrap(), None);
        assert_eq!(validate_port_pin("  ").unwrap(), None);
        assert_eq!(validate_port_pin(" 8085 ").unwrap(), Some(8085));
        assert_eq!(validate_port_pin("65535").unwrap(), Some(65535));
        for invalid in ["0", "65536", "-1", "abc", "80a", "8.5"] {
            assert!(validate_port_pin(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn test_validate_compose_file() {
        let existing = vec!["compose.yaml".to_string()];
        assert_eq!(
            validate_compose_file(" docker/compose.yaml ", &existing).unwrap(),
            "docker/compose.yaml"
        );
        assert!(validate_compose_file("", &existing).is_err());
        assert!(validate_compose_file("  ", &existing).is_err());
        assert!(validate_compose_file("compose.yaml", &existing).is_err());
        assert!(validate_compose_file(" compose.yaml ", &existing).is_err());
    }
}
