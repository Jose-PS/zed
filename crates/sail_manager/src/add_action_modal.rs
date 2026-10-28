use gpui::{
    DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Styled as _, Task,
};
use project::Project;
use ui::{ContextMenu, DropdownMenu, DropdownStyle, prelude::*, v_flex};
use ui_input::InputField;
use workspace::ModalView;
use worktree::WorktreeId;

use crate::compose::{self, CustomAction, CustomActionMode};
use crate::project_settings;

pub struct AddActionModal {
    name: Entity<InputField>,
    command: Entity<InputField>,
    working_directory: Entity<InputField>,
    services: Vec<String>,
    service: String,
    mode: CustomActionMode,
    existing_names: Vec<String>,
    project: Entity<Project>,
    worktree_id: WorktreeId,
    error: Option<SharedString>,
    saving: Option<Task<()>>,
}

pub fn build_action(
    name: &str,
    service: &str,
    command_line: &str,
    working_directory: &str,
    mode: CustomActionMode,
    existing_names: &[String],
) -> anyhow::Result<CustomAction> {
    let command = compose::split_command_line(command_line)?;
    let action = CustomAction::validated(
        name,
        Some(service),
        command,
        Some(working_directory),
        mode,
        false,
    )?;
    anyhow::ensure!(
        !existing_names
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&action.name)),
        "an action named {:?} already exists",
        action.name
    );
    Ok(action)
}

impl AddActionModal {
    pub fn new(
        project: Entity<Project>,
        worktree_id: WorktreeId,
        services: Vec<String>,
        app_service: String,
        existing_names: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputField::new(window, cx, "Run seeders")
                .label("Name")
                .tab_stop(true)
                .tab_index(1)
        });
        let command = cx.new(|cx| {
            InputField::new(window, cx, "php artisan db:seed")
                .label("Command")
                .tab_stop(true)
                .tab_index(2)
        });
        let working_directory = cx.new(|cx| {
            InputField::new(window, cx, "/var/www/html")
                .label("Working directory (optional)")
                .tab_stop(true)
                .tab_index(3)
        });
        Self {
            name,
            command,
            working_directory,
            services,
            service: app_service,
            mode: CustomActionMode::Terminal,
            existing_names,
            project,
            worktree_id,
            error: None,
            saving: None,
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn focus_next(&mut self, _: &menu::SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next(cx);
    }

    fn focus_previous(
        &mut self,
        _: &menu::SelectPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus_prev(cx);
    }

    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            return;
        }
        let action = build_action(
            &self.name.read(cx).text(cx),
            &self.service,
            &self.command.read(cx).text(cx),
            &self.working_directory.read(cx).text(cx),
            self.mode,
            &self.existing_names,
        );
        let action = match action {
            Ok(action) => action,
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        self.error = None;
        let save =
            project_settings::add_custom_action(self.project.clone(), self.worktree_id, action, cx);
        self.saving = Some(cx.spawn(async move |this, cx| {
            let result = save.await;
            this.update(cx, |this, cx| {
                this.saving = None;
                match result {
                    Ok(()) => cx.emit(DismissEvent),
                    Err(error) => {
                        this.error = Some(format!("Could not save the action: {error:#}").into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    fn render_service_menu(&self, window: &mut Window, cx: &mut Context<Self>) -> DropdownMenu {
        let modal = cx.weak_entity();
        let services = self.services.clone();
        DropdownMenu::new(
            "sail-manager-action-service",
            self.service.clone(),
            ContextMenu::build(window, cx, move |mut menu, _, _| {
                for service in services {
                    let modal = modal.clone();
                    let selected = service.clone();
                    menu = menu.entry(service, None, move |_, cx| {
                        modal
                            .update(cx, |this, cx| {
                                this.service = selected.clone();
                                cx.notify();
                            })
                            .ok();
                    });
                }
                menu
            }),
        )
        .style(DropdownStyle::Outlined)
        .trigger_size(ButtonSize::Compact)
    }

    fn render_mode_menu(&self, window: &mut Window, cx: &mut Context<Self>) -> DropdownMenu {
        let modal = cx.weak_entity();
        let label = match self.mode {
            CustomActionMode::Terminal => "Terminal",
            CustomActionMode::Background => "Background",
        };
        DropdownMenu::new(
            "sail-manager-action-mode",
            label,
            ContextMenu::build(window, cx, move |mut menu, _, _| {
                for (label, mode) in [
                    ("Terminal", CustomActionMode::Terminal),
                    ("Background", CustomActionMode::Background),
                ] {
                    let modal = modal.clone();
                    menu = menu.entry(label, None, move |_, cx| {
                        modal
                            .update(cx, |this, cx| {
                                this.mode = mode;
                                cx.notify();
                            })
                            .ok();
                    });
                }
                menu
            }),
        )
        .style(DropdownStyle::Outlined)
        .trigger_size(ButtonSize::Compact)
    }
}

impl EventEmitter<DismissEvent> for AddActionModal {}
impl ModalView for AddActionModal {}

impl Focusable for AddActionModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name.focus_handle(cx)
    }
}

impl Render for AddActionModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let service_menu = self.render_service_menu(window, cx);
        let mode_menu = self.render_mode_menu(window, cx);
        v_flex()
            .key_context("SailManagerAddActionModal")
            .tab_group()
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .elevation_2(cx)
            .w(rems(34.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .w_full()
                    .gap_1p5()
                    .child(Icon::new(IconName::Plus).size(IconSize::XSmall))
                    .child(Headline::new("Add Action").size(HeadlineSize::XSmall)),
            )
            .child(
                v_flex()
                    .px_3()
                    .pb_2()
                    .w_full()
                    .gap_2()
                    .child(self.name.clone())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Label::new("Service").size(LabelSize::Small))
                            .child(service_menu)
                            .child(Label::new("Mode").size(LabelSize::Small))
                            .child(mode_menu),
                    )
                    .child(self.command.clone())
                    .child(self.working_directory.clone()),
            )
            .child(
                h_flex()
                    .px_3()
                    .pb_2()
                    .w_full()
                    .gap_1()
                    .justify_between()
                    .child(match self.error.clone() {
                        Some(error) => Label::new(error).size(LabelSize::Small).color(Color::Error),
                        None => Label::new("Saved in the project settings.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("sail-manager-action-cancel", "Cancel")
                                    .label_size(LabelSize::Small)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                            )
                            .child(
                                Button::new("sail-manager-action-save", "Save")
                                    .style(ButtonStyle::Filled)
                                    .label_size(LabelSize::Small)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm(&menu::Confirm, window, cx)
                                    })),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_action() {
        let existing = vec!["Seed".to_string()];
        let action = build_action(
            " Reset ",
            "laravel.test",
            "php artisan migrate:fresh --seed",
            " /var/www/html ",
            CustomActionMode::Background,
            &existing,
        )
        .unwrap();
        assert_eq!(action.name, "Reset");
        assert_eq!(
            action.command,
            ["php", "artisan", "migrate:fresh", "--seed"]
        );
        assert_eq!(action.working_directory.as_deref(), Some("/var/www/html"));
        assert_eq!(action.mode, CustomActionMode::Background);

        let build = |name: &str, service: &str, command: &str| {
            build_action(
                name,
                service,
                command,
                "",
                CustomActionMode::Terminal,
                &existing,
            )
        };
        assert!(build("", "laravel.test", "ls").is_err());
        assert!(build("seed", "laravel.test", "ls").is_err());
        assert!(build("Other", "laravel.test", "").is_err());
        assert!(build("Other", "laravel.test", "echo 'open").is_err());
        assert!(build("Other", "bad name", "ls").is_err());
        assert_eq!(
            build("Other", "laravel.test", "ls")
                .unwrap()
                .working_directory,
            None
        );
    }
}
