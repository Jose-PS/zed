use super::{AcpSession, ClientContext, apply_subagent_update, session_thread};
use acp_thread::AcpThread;
use agent_client_protocol::{
    Agent, Handled, UntypedMessage,
    schema::MaybeUndefined,
    v1::{ConnectionTo, schema as acp},
    v2::schema as acp_v2,
};
use anyhow::Result;
use collections::HashMap;
use gpui::{AsyncApp, Entity, WeakEntity};
use serde::Deserialize;
use std::{cell::RefCell, rc::Rc};
use util::ResultExt as _;

pub const CHILD_SESSION_UPDATES_META_KEY: &str = "opencode/child-session-updates";
pub const CHILD_UPDATE_METHOD: &str = "opencode/session/child_update";
const CHILD_SESSION_META_KEY: &str = "opencode/child-session";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildUpdateNotification {
    pub root_session_id: acp_v2::SessionId,
    pub child_session_id: acp_v2::SessionId,
    pub parent_session_id: acp_v2::SessionId,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(flatten)]
    pub payload: ChildUpdatePayload,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChildUpdatePayload {
    Status { status: String },
    Update { update: acp::SessionUpdate },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChildSessionMarker {
    id: acp_v2::SessionId,
    #[serde(default)]
    title: Option<String>,
}

pub fn parse_child_update_notification(
    message: UntypedMessage,
    connection: ConnectionTo<Agent>,
) -> Result<
    Result<ChildUpdateNotification, (UntypedMessage, ConnectionTo<Agent>)>,
    agent_client_protocol::Error,
> {
    if message.method() != CHILD_UPDATE_METHOD {
        return Ok(Err((message, connection)));
    }
    let notification = serde_json::from_value::<ChildUpdateNotification>(message.params().clone())
        .map_err(|_| {
            agent_client_protocol::Error::invalid_params()
                .data("Malformed OpenCode child session update notification")
        })?;
    Ok(Ok(notification))
}

pub fn handle_child_update_notification(
    notification: ChildUpdateNotification,
    cx: &mut AsyncApp,
    context: &ClientContext,
    apply_session_notification: fn(acp::SessionNotification, &mut AsyncApp, &ClientContext),
) {
    let Some(parent) = resolve_parent_thread(
        &context.sessions,
        &notification.parent_session_id,
        &notification.root_session_id,
    ) else {
        log::warn!(
            "Ignoring OpenCode child session update for {:?}: neither parent {:?} nor root {:?} exists",
            notification.child_session_id,
            notification.parent_session_id,
            notification.root_session_id
        );
        return;
    };

    match notification.payload {
        ChildUpdatePayload::Status { status } => {
            let Some(state) = map_child_status(&status) else {
                log::warn!(
                    "Ignoring OpenCode child session status {status:?} for {:?}",
                    notification.child_session_id
                );
                return;
            };
            let subagent_update =
                build_subagent_update(&notification.child_session_id, notification.title.as_deref());
            cx.update(|cx| {
                let subagent =
                    apply_subagent_update(&parent, &subagent_update, &context.sessions, cx)?;
                subagent.update(cx, |subagent, cx| subagent.update_session_state(state, cx))
            })
            .log_err();
        }
        ChildUpdatePayload::Update { mut update } => {
            if !context
                .sessions
                .borrow()
                .contains_key(&notification.child_session_id)
            {
                let subagent_update = build_subagent_update(
                    &notification.child_session_id,
                    notification.title.as_deref(),
                );
                let created = cx
                    .update(|cx| {
                        let subagent = apply_subagent_update(
                            &parent,
                            &subagent_update,
                            &context.sessions,
                            cx,
                        )?;
                        subagent.update(cx, |subagent, cx| {
                            subagent.update_session_state(running_state(), cx)
                        })
                    })
                    .log_err();
                if created.is_none() {
                    return;
                }
            }
            let effective_title = notification
                .title
                .as_deref()
                .or_else(|| marker_title_from_session_update(&update));
            let title_prefix = effective_title.map(|title| format!("{title}: "));
            if let Some(prefix) = title_prefix.as_deref() {
                strip_session_update_title_prefix(&mut update, prefix);
            }
            apply_session_notification(
                acp::SessionNotification::new(
                    acp::SessionId::new(notification.child_session_id.0),
                    update,
                ),
                cx,
                context,
            );
        }
    }
}

pub fn resolve_permission_target(
    request: &mut acp::RequestPermissionRequest,
    context: &ClientContext,
    cx: &AsyncApp,
) -> Result<WeakEntity<AcpThread>, agent_client_protocol::Error> {
    if let Some(marker) = extract_child_session_marker(request.tool_call.meta.as_ref())
        && let Some((child_thread, is_subagent)) = context
            .sessions
            .borrow()
            .get(&marker.id)
            .map(|session| (session.thread.clone(), session.subagent_root.is_some()))
        && is_subagent
    {
        let marker_title = marker.title.or_else(|| {
            child_thread.upgrade().and_then(|thread| {
                thread
                    .read_with(cx, |thread, _| {
                        thread
                            .subagent_association()
                            .and_then(|association| association.title.clone())
                    })
                    .map(|title| title.to_string())
            })
        });
        if let Some(title) = marker_title {
            let prefix = format!("{title}: ");
            strip_tool_call_update_title_prefix(&mut request.tool_call, &prefix);
        }
        return Ok(child_thread);
    }

    session_thread(context, &acp_v2::SessionId::new(request.session_id.0.clone()))
}

fn resolve_parent_thread(
    sessions: &Rc<RefCell<HashMap<acp_v2::SessionId, AcpSession>>>,
    parent_session_id: &acp_v2::SessionId,
    root_session_id: &acp_v2::SessionId,
) -> Option<Entity<AcpThread>> {
    let sessions = sessions.borrow();
    sessions
        .get(parent_session_id)
        .and_then(|session| session.thread.upgrade())
        .or_else(|| {
            sessions
                .get(root_session_id)
                .and_then(|session| session.thread.upgrade())
        })
}

fn build_subagent_update(
    child_session_id: &acp_v2::SessionId,
    title: Option<&str>,
) -> acp_v2::SubagentUpdate {
    let mut update = acp_v2::SubagentUpdate::new(child_session_id.clone());
    if let Some(title) = title {
        update = update.title(MaybeUndefined::Value(title.to_owned()));
    }
    update
}

fn running_state() -> acp_v2::StateUpdate {
    acp_v2::StateUpdate::Running(acp_v2::RunningStateUpdate::new())
}

fn map_child_status(status: &str) -> Option<acp_v2::StateUpdate> {
    match status {
        "created" | "running" => Some(running_state()),
        "completed" => Some(acp_v2::StateUpdate::Idle(
            acp_v2::IdleStateUpdate::new().stop_reason(acp_v2::StopReason::EndTurn),
        )),
        _ => None,
    }
}

fn extract_child_session_marker(meta: Option<&acp::Meta>) -> Option<ChildSessionMarker> {
    let value = meta?.get(CHILD_SESSION_META_KEY)?;
    serde_json::from_value(value.clone()).ok()
}

fn marker_title_from_session_update(update: &acp::SessionUpdate) -> Option<&str> {
    let meta = match update {
        acp::SessionUpdate::ToolCall(tool_call) => tool_call.meta.as_ref(),
        acp::SessionUpdate::ToolCallUpdate(tool_call_update) => tool_call_update.meta.as_ref(),
        _ => None,
    }?;
    meta.get(CHILD_SESSION_META_KEY)?
        .get("title")?
        .as_str()
}

fn strip_session_update_title_prefix(update: &mut acp::SessionUpdate, prefix: &str) {
    match update {
        acp::SessionUpdate::ToolCall(tool_call) => {
            if let Some(stripped) = tool_call.title.strip_prefix(prefix) {
                tool_call.title = stripped.to_owned();
            }
        }
        acp::SessionUpdate::ToolCallUpdate(tool_call_update) => {
            strip_tool_call_update_title_prefix(tool_call_update, prefix);
        }
        _ => {}
    }
}

fn strip_tool_call_update_title_prefix(
    tool_call_update: &mut acp::ToolCallUpdate,
    prefix: &str,
) {
    if let Some(title) = tool_call_update.fields.title.as_mut()
        && let Some(stripped) = title.strip_prefix(prefix)
    {
        *title = stripped.to_owned();
    }
}

pub fn into_untyped_handled(
    parsed: Result<ChildUpdateNotification, (UntypedMessage, ConnectionTo<Agent>)>,
    enqueue: impl FnOnce(ChildUpdateNotification),
) -> Handled<(UntypedMessage, ConnectionTo<Agent>)> {
    match parsed {
        Ok(notification) => {
            enqueue(notification);
            Handled::Yes
        }
        Err((message, connection)) => Handled::No {
            message: (message, connection),
            retry: false,
        },
    }
}
