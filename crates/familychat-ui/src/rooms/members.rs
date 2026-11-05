//! The member list beside a room: who has joined and who is invited, with
//! their roles, from `SessionApi::members`.

use familychat_core::rooms::{Membership, RoomMember};
use gpui::{AnyElement, ElementId, FontWeight, IntoElement, Role, div, prelude::*, px, rgb};

use crate::a11y::{AccessibleExt, readable};
use crate::components::{Avatar, Badge, BadgeTone};
use crate::signin::spinner;
use crate::styles::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TYPE_BODY, TYPE_CAPTION, TypeStyleExt, UiColors,
};

/// The member panel's width.
pub(crate) const MEMBERS_WIDTH: f32 = 280.0;

/// What the member list has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MembersState {
    /// Being asked for.
    Loading,
    /// The members, sorted.
    Ready(Vec<RoomMember>),
    /// They couldn't be had.
    Failed(String),
}

fn member_row(index: usize, member: &RoomMember, colors: &UiColors) -> AnyElement {
    let name = member.name();
    let role = member.role.label();
    let mut label = name.clone();
    if member.is_own {
        label.push_str(" (you)");
    }
    if let Some(role) = role {
        label.push_str(&format!(", {role}"));
    }
    if member.membership == Membership::Invited {
        label.push_str(", invited");
    }
    div()
        .id(ElementId::NamedInteger("member-row".into(), index as u64))
        .accessible(Role::ListItem, label)
        .flex()
        .items_center()
        .gap(px(SPACE_3))
        .px(px(SPACE_4))
        .py(px(SPACE_1 + 2.0))
        .child(
            Avatar::new(name.clone())
                .address(member.user_id.to_string())
                .size(32.0),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SPACE_1))
                        .child(
                            div()
                                .truncate()
                                .type_style(TYPE_BODY)
                                .font_weight(FontWeight::MEDIUM)
                                .child(name),
                        )
                        .when(member.is_own, |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .type_style(TYPE_CAPTION)
                                    .text_color(rgb(colors.muted))
                                    .child("(you)"),
                            )
                        }),
                )
                .child(
                    div()
                        .truncate()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(member.user_id.to_string()),
                ),
        )
        .children(role.map(|role| Badge::label(role).tone(BadgeTone::Neutral)))
        .into_any_element()
}

fn section(title: &str, colors: &UiColors) -> AnyElement {
    div()
        .px(px(SPACE_4))
        .pt(px(SPACE_3))
        .pb(px(SPACE_1))
        .type_style(TYPE_CAPTION)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(colors.muted))
        .child(title.to_owned())
        .into_any_element()
}

/// The list's body: the members by section, or its loading or problem.
pub(crate) fn render_members(state: &MembersState, colors: &UiColors) -> AnyElement {
    match state {
        MembersState::Loading => div()
            .id("members-loading")
            .accessible(Role::Status, "Loading the members")
            .flex()
            .justify_center()
            .p(px(SPACE_4))
            .child(spinner("members-spinner", 20.0, colors.muted))
            .into_any_element(),
        MembersState::Failed(message) => div()
            .p(px(SPACE_4))
            .type_style(TYPE_BODY)
            .text_color(rgb(colors.danger))
            .child(readable("members-problem", message.clone()))
            .into_any_element(),
        MembersState::Ready(members) => {
            let joined: Vec<_> = members
                .iter()
                .filter(|member| member.membership != Membership::Invited)
                .collect();
            let invited: Vec<_> = members
                .iter()
                .filter(|member| member.membership == Membership::Invited)
                .collect();
            let mut index = 0;
            let mut rows = |list: &[&RoomMember]| {
                list.iter()
                    .map(|member| {
                        index += 1;
                        member_row(index, member, colors)
                    })
                    .collect::<Vec<_>>()
            };
            let joined_rows = rows(&joined);
            let invited_rows = rows(&invited);
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_2 / 2.0))
                .child(section(&format!("Joined, {}", joined.len()), colors))
                .children(joined_rows)
                .when(!invited.is_empty(), |this| {
                    this.child(section(&format!("Invited, {}", invited.len()), colors))
                        .children(invited_rows)
                })
                .into_any_element()
        }
    }
}
