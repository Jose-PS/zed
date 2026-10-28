use editor::Editor;
use gpui::{
    DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Styled as _,
    WeakEntity,
};
use ui::{prelude::*, v_flex};
use workspace::ModalView;

use crate::compose;
use crate::sail_manager_panel::SailManagerPanel;

pub struct ComposerCommandModal {
    editor: Entity<Editor>,
    panel: WeakEntity<SailManagerPanel>,
    error: Option<SharedString>,
}

impl ComposerCommandModal {
    pub fn new(
        panel: WeakEntity<SailManagerPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("require laravel/horizon", window, cx);
            editor
        });
        Self {
            editor,
            panel,
            error: None,
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).text(cx);
        let arguments = match compose::split_command_line(&text) {
            Ok(arguments) => arguments,
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let panel = self.panel.clone();
        cx.emit(DismissEvent);
        panel
            .update(cx, |panel, cx| {
                panel.run_composer_command(arguments, window, cx)
            })
            .ok();
    }
}

impl EventEmitter<DismissEvent> for ComposerCommandModal {}
impl ModalView for ComposerCommandModal {}

impl Focusable for ComposerCommandModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for ComposerCommandModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SailManagerComposerCommandModal")
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
                    .child(Icon::new(IconName::Terminal).size(IconSize::XSmall))
                    .child(Headline::new("Composer Command").size(HeadlineSize::XSmall)),
            )
            .child(
                h_flex()
                    .px_3()
                    .pb_2()
                    .w_full()
                    .gap_1p5()
                    .child(Label::new("composer").color(Color::Muted).buffer_font(cx))
                    .child(div().flex_1().child(self.editor.clone())),
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
                        None => Label::new("Runs in the app container.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("sail-manager-composer-cancel", "Cancel")
                                    .label_size(LabelSize::Small)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                            )
                            .child(
                                Button::new("sail-manager-composer-run", "Run")
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
