//! Reusable UI components shared by the Family Chat views, copied from
//! Zmail's `zmail-ui` (`src/components`) and adapted: its mail-only pieces
//! (the gallery, the split handles, the select, the skeleton rows) are left
//! out. Each component lives in its own module and is re-exported from here.

// The component set leads the views: some builders are unused until the
// room list and timeline (Phase 6) draw with them.
#![allow(dead_code, unused_imports)]

pub(crate) mod actions;
pub(crate) mod avatar;
pub(crate) mod badge;
pub(crate) mod banner;
pub(crate) mod button;
pub(crate) mod hit_target;
pub(crate) mod kbd;
pub(crate) mod menu;
pub(crate) mod modal;
pub(crate) mod popover;
pub(crate) mod scrollbar;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod text_input;
pub(crate) mod theme;
pub(crate) mod title_bar;
pub(crate) mod toast;
pub(crate) mod tooltip;
pub(crate) mod window_frame;

pub(crate) use avatar::Avatar;
pub(crate) use badge::{Badge, BadgeTone};
pub(crate) use banner::{Banner, BannerTone};
pub(crate) use button::{Button, ButtonSize, ButtonVariant, IconButton};
pub(crate) use hit_target::{HitTargetExt, MIN_TARGET_SIZE};
pub(crate) use kbd::Kbd;
pub(crate) use menu::{Menu, MenuEntry};
pub(crate) use modal::{Modal, ModalState};
pub(crate) use popover::{overlay_at, popover_below, popover_below_end};
pub(crate) use scrollbar::Scrollbar;
pub(crate) use text_input::{TextInput, TextInputEvent};
pub(crate) use theme::ComponentTheme;
pub(crate) use title_bar::TitleBar;
pub(crate) use toast::{Toast, ToastHost, ToastId, ToastKind};
pub(crate) use window_frame::window_frame;
