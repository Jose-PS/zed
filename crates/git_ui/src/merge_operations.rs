use askpass::AskPassDelegate;
use git::repository::{CherryPickOutcome, InProgressOperation, MergeOutcome};
use git_ui_core::{askpass_modal::AskPassModal, notifications::show_error_toast};
use gpui::{App, Entity, PromptLevel, SharedString, WeakEntity, Window};
use notifications::status_toast::StatusToast;
use project::git_store::Repository;
use ui::prelude::*;
use workspace::Workspace;

fn operation_askpass_delegate(
    workspace: WeakEntity<Workspace>,
    operation: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> AskPassDelegate {
    let operation = operation.into();
    let window = window.window_handle();
    AskPassDelegate::new_with_cancellation(
        &mut cx.to_async(),
        move |prompt, tx, cancellation, cx| {
            window
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        workspace.toggle_modal(window, cx, |window, cx| {
                            AskPassModal::new(
                                operation.clone(),
                                prompt.into(),
                                tx,
                                cancellation,
                                window,
                                cx,
                            )
                        });
                    })
                })
                .ok();
        },
    )
}

fn show_operation_toast(
    workspace: &WeakEntity<Workspace>,
    message: String,
    has_conflicts: bool,
    cx: &mut App,
) {
    let Some(workspace) = workspace.upgrade() else {
        return;
    };
    workspace.update(cx, |workspace, cx| {
        let toast = StatusToast::new(message, cx, |this, _cx| {
            let (icon, color) = if has_conflicts {
                (IconName::Warning, Color::Warning)
            } else {
                (IconName::GitBranch, Color::Muted)
            };
            this.icon(Icon::new(icon).size(IconSize::Small).color(color))
        });
        workspace.toggle_status_toast(toast, cx);
    });
}

fn current_branch_name(repository: &Entity<Repository>, cx: &App) -> String {
    repository
        .read(cx)
        .branch
        .as_ref()
        .map(|branch| branch.name().to_string())
        .unwrap_or_else(|| "HEAD".to_string())
}

pub(crate) fn merge_message(outcome: MergeOutcome, branch: &str, current_branch: &str) -> String {
    match outcome {
        MergeOutcome::AlreadyUpToDate => {
            format!("{current_branch} is already up to date with {branch}")
        }
        MergeOutcome::FastForward => format!("Fast-forwarded {current_branch} to {branch}"),
        MergeOutcome::Merged => format!("Merged {branch} into {current_branch}"),
        MergeOutcome::Conflicts => {
            format!("Merging {branch} has conflicts. Resolve them and commit to finish")
        }
    }
}

pub(crate) fn cherry_pick_message(
    outcome: CherryPickOutcome,
    short_sha: &str,
    current_branch: &str,
) -> String {
    match outcome {
        CherryPickOutcome::Applied => format!("Cherry-picked {short_sha} into {current_branch}"),
        CherryPickOutcome::AlreadyApplied => {
            format!("{short_sha} is already in {current_branch}")
        }
        CherryPickOutcome::Conflicts => {
            format!("Cherry-picking {short_sha} has conflicts. Resolve them and commit to finish")
        }
    }
}

pub(crate) fn merge_branch(
    workspace: WeakEntity<Workspace>,
    repository: Entity<Repository>,
    branch: String,
    window: &mut Window,
    cx: &mut App,
) {
    let askpass = operation_askpass_delegate(workspace.clone(), "git merge", window, cx);
    let current_branch = current_branch_name(&repository, cx);
    let receiver = repository.update(cx, |repository, cx| {
        repository.merge(branch.clone(), askpass, cx)
    });
    cx.spawn(async move |cx| {
        let result = match receiver.await {
            Ok(result) => result,
            Err(canceled) => Err(canceled.into()),
        };
        cx.update(|cx| match result {
            Ok(outcome) => show_operation_toast(
                &workspace,
                merge_message(outcome, &branch, &current_branch),
                outcome == MergeOutcome::Conflicts,
                cx,
            ),
            Err(error) => {
                if let Some(workspace) = workspace.upgrade() {
                    show_error_toast(workspace, "merge", error, cx);
                }
            }
        });
    })
    .detach();
}

pub(crate) fn cherry_pick_commit(
    workspace: WeakEntity<Workspace>,
    repository: Entity<Repository>,
    sha: String,
    short_sha: String,
    window: &mut Window,
    cx: &mut App,
) {
    let askpass = operation_askpass_delegate(workspace.clone(), "git cherry-pick", window, cx);
    let current_branch = current_branch_name(&repository, cx);
    let receiver = repository.update(cx, |repository, cx| {
        repository.cherry_pick(sha, askpass, cx)
    });
    cx.spawn(async move |cx| {
        let result = match receiver.await {
            Ok(result) => result,
            Err(canceled) => Err(canceled.into()),
        };
        cx.update(|cx| match result {
            Ok(outcome) => show_operation_toast(
                &workspace,
                cherry_pick_message(outcome, &short_sha, &current_branch),
                outcome == CherryPickOutcome::Conflicts,
                cx,
            ),
            Err(error) => {
                if let Some(workspace) = workspace.upgrade() {
                    show_error_toast(workspace, "cherry-pick", error, cx);
                }
            }
        });
    })
    .detach();
}

pub(crate) fn abort_in_progress_operation(
    workspace: WeakEntity<Workspace>,
    repository: Entity<Repository>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(operation) = repository.read(cx).snapshot().merge.in_progress else {
        return;
    };
    let subcommand = operation.subcommand();
    let prompt = window.prompt(
        PromptLevel::Warning,
        &format!("Abort the {subcommand} in progress?"),
        Some("Your working tree will be restored to the state it was in before it started."),
        &["Abort", "Cancel"],
        cx,
    );
    cx.spawn(async move |cx| {
        if !matches!(prompt.await, Ok(0)) {
            return;
        }
        let receiver = cx.update(|cx| {
            repository.update(cx, |repository, _| repository.abort_operation(operation))
        });
        let result = match receiver.await {
            Ok(result) => result,
            Err(canceled) => Err(canceled.into()),
        };
        if let Err(error) = result {
            cx.update(|cx| {
                if let Some(workspace) = workspace.upgrade() {
                    show_error_toast(workspace, format!("{subcommand} --abort"), error, cx);
                }
            });
        }
    })
    .detach();
}

pub(crate) fn in_progress_operation_label(operation: InProgressOperation) -> &'static str {
    match operation {
        InProgressOperation::Merge => "Merge",
        InProgressOperation::CherryPick => "Cherry-pick",
        InProgressOperation::Revert => "Revert",
        InProgressOperation::Rebase => "Rebase",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_message() {
        assert_eq!(
            merge_message(MergeOutcome::Merged, "dev", "main"),
            "Merged dev into main"
        );
        assert_eq!(
            merge_message(MergeOutcome::FastForward, "dev", "main"),
            "Fast-forwarded main to dev"
        );
        assert_eq!(
            merge_message(MergeOutcome::AlreadyUpToDate, "dev", "main"),
            "main is already up to date with dev"
        );
        assert_eq!(
            merge_message(MergeOutcome::Conflicts, "dev", "main"),
            "Merging dev has conflicts. Resolve them and commit to finish"
        );
    }

    #[test]
    fn test_cherry_pick_message() {
        assert_eq!(
            cherry_pick_message(CherryPickOutcome::Applied, "abc1234", "main"),
            "Cherry-picked abc1234 into main"
        );
        assert_eq!(
            cherry_pick_message(CherryPickOutcome::AlreadyApplied, "abc1234", "main"),
            "abc1234 is already in main"
        );
        assert_eq!(
            cherry_pick_message(CherryPickOutcome::Conflicts, "abc1234", "main"),
            "Cherry-picking abc1234 has conflicts. Resolve them and commit to finish"
        );
    }
}
