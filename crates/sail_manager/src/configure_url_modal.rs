use editor::Editor;
use gpui::{
    DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Styled as _, Task,
};
use project::Project;
use ui::{prelude::*, v_flex};
use workspace::ModalView;
use worktree::WorktreeId;

use crate::compose;
use crate::project_settings;

pub struct ConfigureUrlModal {
    editor: Entity<Editor>,
    project: Entity<Project>,
    worktree_id: WorktreeId,
    service: String,
    error: Option<SharedString>,
    saving: Option<Task<()>>,
}

impl ConfigureUrlModal {
    pub fn new(
        project: Entity<Project>,
        worktree_id: WorktreeId,
        service: String,
        url_host: &str,
        current_url: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = format!("http://{url_host}:<port>");
        let current_url = current_url.map(str::to_string);
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(&placeholder, window, cx);
            if let Some(current_url) = current_url {
                editor.set_text(current_url, window, cx);
            }
            editor
        });
        Self {
            editor,
            project,
            worktree_id,
            service,
            error: None,
            saving: None,
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            return;
        }
        let text = self.editor.read(cx).text(cx);
        let url = match compose::validate_manual_url(&text) {
            Ok(url) => url,
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        self.error = None;
        let save = project_settings::set_manual_url(
            self.project.clone(),
            self.worktree_id,
            self.service.clone(),
            url,
            cx,
        );
        self.saving = Some(cx.spawn(async move |this, cx| {
            let result = save.await;
            this.update(cx, |this, cx| {
                this.saving = None;
                match result {
                    Ok(()) => cx.emit(DismissEvent),
                    Err(error) => {
                        this.error = Some(format!("Could not save the URL: {error:#}").into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }
}

impl EventEmitter<DismissEvent> for ConfigureUrlModal {}
impl ModalView for ConfigureUrlModal {}

impl Focusable for ConfigureUrlModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for ConfigureUrlModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SailManagerConfigureUrlModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(34.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .w_full()
                    .gap_1p5()
                    .child(Icon::new(IconName::Globe).size(IconSize::XSmall))
                    .child(
                        Headline::new(format!("Configure URL ({})", self.service))
                            .size(HeadlineSize::XSmall),
                    ),
            )
            .child(div().px_3().pb_2().w_full().child(self.editor.clone()))
            .child(
                h_flex()
                    .px_3()
                    .pb_2()
                    .w_full()
                    .gap_1()
                    .justify_between()
                    .child(match self.error.clone() {
                        Some(error) => Label::new(error).size(LabelSize::Small).color(Color::Error),
                        None => Label::new("Leave it empty to use the detected URL.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("sail-manager-url-cancel", "Cancel")
                                    .label_size(LabelSize::Small)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                            )
                            .child(
                                Button::new("sail-manager-url-confirm", "Save")
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
