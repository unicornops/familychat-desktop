//! The Settings window's sections, drawn from the window's state (ADR
//! 0015). Every control is a named button with its role (a check box for
//! an on-off setting, radio buttons for a choice), so a screen reader says
//! what it is and whether it is on.
//!
//! Adapted from Zmail's `zmail-ui` (`src/settings/*.rs`, unicornops/zmail,
//! GPL-3.0-only like this project): the sections' layout and rows.

use familychat_core::{
    encryption::{DeviceTrust, RecoveryState},
    identity::{APP_NAME, PANEL_URL},
    settings::{
        AppearanceSettings, ContrastChoice, MotionChoice, NotificationSettings, PrivacySettings,
        TEXT_SIZES, ThemeChoice,
    },
};
use gpui::{
    AnyElement, App, ClipboardItem, Context, IntoElement, Role, SharedString, Window, div,
    prelude::*, px, rgb,
};

use super::{SettingsWindow, group_heading, row_value, section_heading, setting_row};
use crate::a11y::{AccessibleExt, readable};
use crate::components::{Banner, BannerTone, Button, ButtonSize, ComponentTheme, Toast, ToastKind};
use crate::encryption::EncryptionLaunch;
use crate::styles::{
    ICON_SIZE_MD, RADIUS_LG, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TYPE_BODY_STRONG, TYPE_CAPTION,
    TYPE_HEADLINE, TypeStyleExt, UiColors,
};
use crate::widgets::icon;

/// What signing out does, under its question.
pub(super) const SIGN_OUT_DETAIL: &str = "Family Chat tells your server, then removes the \
     account from this computer: its keys, its stored messages and pictures, its drafts and its \
     settings here. Your account, your rooms and your other devices stay as they are.";
/// When nothing else could bring the encrypted history back.
pub(super) const SIGN_OUT_UNPROTECTED: &str = "You haven't set up recovery and have no other \
     confirmed device, so after signing out your encrypted messages can't be read again. Set up \
     recovery first, in Security & privacy.";
/// A Family Chat account's guidance (PLAN.md, section 6.9).
pub(super) const FAMILY_CHAT_GUIDANCE: &str = "A parent looks after this account in the Family \
     Chat panel. Ask them if you forget your password (they can reset it) or need to sign in \
     somewhere new (they can give you a sign-in code). Accounts are only deactivated from the \
     panel, never from this app.";
/// Another server's account.
pub(super) const OTHER_SERVER_GUIDANCE: &str = "This account is on another Matrix server. Change \
     its password or deactivate it with that server's own tools; Family Chat only signs in.";
/// What leaves the computer.
pub(super) const PRIVACY_PROMISE: &str = "Family Chat talks only to your homeserver. It sends no \
     analytics or crash reports, fetches no link previews, and uses no other services.";
/// Diagnostics: nothing is uploaded.
pub(super) const NOTHING_UPLOADED: &str = "Nothing is uploaded. The logs stay on this computer \
     and never hold message text, passwords, tokens or keys. To report a problem, open an issue \
     on GitHub and attach only what you choose.";

/// An on-off setting's control: a check box button.
fn toggle(
    id: &'static str,
    label: &'static str,
    on: bool,
    disabled: bool,
    handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id, label)
        .role(Role::CheckBox)
        .icon(if on { "check" } else { "square" })
        .selected(on)
        .disabled(disabled)
        .on_click(handler)
}

/// A choice's control: a group of radio buttons.
fn choice<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: &'static str,
    options: &[(T, String)],
    current: T,
    on_pick: impl Fn(T, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    div()
        .id(id)
        .accessible(Role::RadioGroup, label)
        .flex()
        .flex_wrap()
        .gap(px(SPACE_1))
        .children(options.iter().enumerate().map(|(index, (value, name))| {
            let value = *value;
            let on_pick = on_pick.clone();
            Button::new(
                SharedString::from(format!("{id}-{index}")),
                SharedString::from(name.clone()),
            )
            .role(Role::RadioButton)
            .size(ButtonSize::Small)
            // The chosen option is filled, so it shows at a glance and in
            // every contrast setting, not only by a shade.
            .when(value == current, Button::primary)
            .selected(value == current)
            .on_click(move |_, window, cx| on_pick(value, window, cx))
        }))
        .into_any_element()
}

impl SettingsWindow {
    /// Runs `update` on this window from a click handler.
    fn listener_for<E>(
        cx: &Context<Self>,
        update: impl Fn(&mut Self, &E, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl Fn(&E, &mut Window, &mut App) + 'static {
        cx.listener(update)
    }

    // -- Account -------------------------------------------------------------

    pub(super) fn render_account(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = ComponentTheme::get(cx).colors;
        let mut page = div()
            .flex()
            .flex_col()
            .child(section_heading(super::SettingsSection::Account.label()));
        let Some(account) = &self.account else {
            return page
                .child(
                    div()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(readable(
                            "settings-account-none",
                            "Nobody is signed in. Sign in from the main window.",
                        )),
                )
                .into_any_element();
        };
        let details = self
            .details
            .as_ref()
            .and_then(|details| details.as_ref().ok());
        let name = details
            .and_then(|details| details.display_name.clone())
            .unwrap_or_else(|| account.user_id.localpart().to_owned());
        let homeserver = details
            .map(|details| details.homeserver_url.clone())
            .unwrap_or_else(|| account.homeserver_url.trim_end_matches('/').to_owned());
        let initial = name
            .chars()
            .next()
            .map(|initial| initial.to_uppercase().collect::<String>())
            .unwrap_or_default();
        page = page.child(
            div()
                .id("settings-account-card")
                .accessible(
                    Role::Group,
                    format!("Signed in as {name}, {}", account.user_id),
                )
                .flex()
                .items_center()
                .gap(px(SPACE_4))
                .p(px(SPACE_4))
                .rounded(px(RADIUS_LG))
                .border_1()
                .border_color(rgb(colors.border))
                .bg(rgb(colors.surface))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(48.0))
                        .rounded_full()
                        .bg(rgb(colors.accent))
                        .text_color(rgb(colors.accent_foreground))
                        .type_style(TYPE_HEADLINE)
                        .child(initial),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .gap(px(SPACE_1))
                        .child(
                            div()
                                .type_style(TYPE_BODY_STRONG)
                                .truncate()
                                .child(readable("settings-account-name", name)),
                        )
                        .child(
                            div()
                                .type_style(TYPE_CAPTION)
                                .text_color(rgb(colors.muted))
                                .truncate()
                                .child(readable(
                                    "settings-account-user-id",
                                    account.user_id.to_string(),
                                )),
                        ),
                ),
        );
        page = page
            .child(group_heading("This account", &colors))
            .child(setting_row(
                "Homeserver",
                None,
                row_value("settings-account-homeserver", homeserver, &colors),
                &colors,
            ));
        if let Some(details) = details {
            let device = match &details.device_name {
                Some(name) => format!("{name} ({})", details.device_id),
                None => details.device_id.clone(),
            };
            page = page.child(setting_row(
                "This device",
                Some("The name and id your other devices see.".into()),
                row_value("settings-account-device", device, &colors),
                &colors,
            ));
        }
        if let Some(Err(problem)) = &self.details {
            page = page.child(
                Banner::new("settings-account-problem", problem.clone()).tone(BannerTone::Caution),
            );
        }
        let family_chat = details.is_some_and(|details| details.family_chat);
        page = if family_chat {
            page.child(group_heading("Managed by your family", &colors))
                .child(setting_row(
                    "Password, sign-in codes and deactivation",
                    Some(FAMILY_CHAT_GUIDANCE.into()),
                    Button::new("settings-open-panel", "Open the Family Chat panel…")
                        .icon("external-link")
                        .tooltip(PANEL_URL)
                        .on_click(Self::listener_for(cx, |this, _, window, cx| {
                            this.open_panel(window, cx)
                        })),
                    &colors,
                ))
        } else {
            page.child(group_heading("Your server", &colors)).child(
                div()
                    .py(px(SPACE_3))
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(readable(
                        "settings-account-other-server",
                        OTHER_SERVER_GUIDANCE,
                    )),
            )
        };
        page.child(group_heading("Sign out", &colors))
            .child(setting_row(
                "Sign out of this computer",
                Some(
                    "Removes the account and everything it keeps here. You can sign in again \
                     later."
                        .into(),
                ),
                Button::new("settings-sign-out", "Sign out…")
                    .danger()
                    .icon("log-out")
                    .on_click(Self::listener_for(cx, |this, _, window, cx| {
                        this.ask_to_sign_out(window, cx)
                    })),
                &colors,
            ))
            .into_any_element()
    }

    // -- Sessions ------------------------------------------------------------

    pub(super) fn render_sessions(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = ComponentTheme::get(cx).colors;
        let page = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .child(section_heading(super::SettingsSection::Sessions.label()));
        let Some(encryption) = self.encryption.clone() else {
            return page
                .child(no_account("settings-sessions-none", &colors))
                .into_any_element();
        };
        let list = encryption.update(cx, |view, cx| view.render_device_list(cx));
        page.child(
            div()
                .type_style(TYPE_CAPTION)
                .text_color(rgb(colors.muted))
                .pb(px(SPACE_2))
                .child(readable(
                    "settings-sessions-detail",
                    "Every device signed in to your account. Verify the ones you use, and sign \
                     out the ones you don't.",
                )),
        )
        .child(list)
        .child(
            div().pt(px(SPACE_2)).child(
                Button::new("settings-sessions-refresh", "Refresh")
                    .icon("refresh-cw")
                    .size(ButtonSize::Small)
                    .on_click(move |_, _, cx| {
                        encryption.update(cx, |view, cx| view.refresh_devices(cx));
                    }),
            ),
        )
        .into_any_element()
    }

    // -- Security & privacy --------------------------------------------------

    pub(super) fn render_privacy(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = ComponentTheme::get(cx).colors;
        let mut page = div()
            .flex()
            .flex_col()
            .child(section_heading(super::SettingsSection::Privacy.label()));
        if let Some(encryption) = self.encryption.clone() {
            let status = encryption.read(cx).status().clone();
            let confirmed = status.device == DeviceTrust::Verified;
            let (recovery_text, recovery_button): (&str, Button) = match status.recovery {
                RecoveryState::Enabled => (
                    "Set up. Your recovery key brings your messages back on a new device.",
                    Button::new("settings-recovery-change", "Change recovery key…")
                        .icon("key-round")
                        .on_click({
                            let encryption = encryption.clone();
                            move |_, window, cx| {
                                encryption.update(cx, |view, cx| {
                                    view.launch(EncryptionLaunch::ResetRecoveryKey, window, cx)
                                });
                            }
                        }),
                ),
                RecoveryState::Incomplete => (
                    "Set up, but this device doesn't have every key. Confirm it's you to fix \
                     that, or make a new recovery key.",
                    Button::new("settings-recovery-change", "Change recovery key…")
                        .icon("key-round")
                        .on_click({
                            let encryption = encryption.clone();
                            move |_, window, cx| {
                                encryption.update(cx, |view, cx| {
                                    view.launch(EncryptionLaunch::ResetRecoveryKey, window, cx)
                                });
                            }
                        }),
                ),
                RecoveryState::Disabled | RecoveryState::Unknown => (
                    "Not set up. Without it, a new device can't read your old messages if you \
                     lose this one.",
                    Button::new("settings-recovery-set-up", "Set up recovery…")
                        .primary()
                        .icon("key-round")
                        .disabled(!confirmed)
                        .on_click({
                            let encryption = encryption.clone();
                            move |_, window, cx| {
                                encryption.update(cx, |view, cx| {
                                    view.launch(EncryptionLaunch::SetUpRecovery, window, cx)
                                });
                            }
                        }),
                ),
            };
            let room_keys = div()
                .flex()
                .gap(px(SPACE_2))
                .child(
                    Button::new("settings-keys-export", "Export…")
                        .icon("download")
                        .on_click({
                            let encryption = encryption.clone();
                            move |_, window, cx| {
                                encryption.update(cx, |view, cx| {
                                    view.launch(EncryptionLaunch::Export, window, cx)
                                });
                            }
                        }),
                )
                .child(
                    Button::new("settings-keys-import", "Import…")
                        .icon("upload")
                        .on_click({
                            let encryption = encryption.clone();
                            move |_, window, cx| {
                                encryption.update(cx, |view, cx| view.import_keys(window, cx));
                            }
                        }),
                );
            page = page
                .child(group_heading("Encryption", &colors))
                .child(setting_row(
                    "Recovery",
                    Some(recovery_text.into()),
                    recovery_button,
                    &colors,
                ))
                .child(setting_row(
                    "Room keys",
                    Some(
                        "Save the keys to your encrypted messages in a file protected by a \
                         passphrase, or read them from one."
                            .into(),
                    ),
                    room_keys,
                    &colors,
                ))
                .child(setting_row(
                    "Reset your identity",
                    Some(
                        "Only if you have no other confirmed device and no recovery key. Your \
                         family sees that your identity changed."
                            .into(),
                    ),
                    Button::new("settings-reset-identity", "Reset identity…")
                        .danger()
                        .icon("triangle-alert")
                        .on_click(move |_, window, cx| {
                            encryption.update(cx, |view, cx| {
                                view.launch(EncryptionLaunch::ResetIdentity, window, cx)
                            });
                        }),
                    &colors,
                ));
        }
        let privacy = self.privacy;
        let notifications = self.notifications;
        page.child(group_heading("Privacy", &colors))
            .child(setting_row(
                "Read receipts",
                Some(
                    "Let others see which messages you've read. When off, your own devices \
                     still keep your place."
                        .into(),
                ),
                toggle(
                    "settings-read-receipts",
                    "Send read receipts",
                    privacy.read_receipts,
                    false,
                    Self::listener_for(cx, move |this, _, window, cx| {
                        this.set_privacy(
                            PrivacySettings {
                                read_receipts: !privacy.read_receipts,
                                ..privacy
                            },
                            window,
                            cx,
                        );
                    }),
                ),
                &colors,
            ))
            .child(setting_row(
                "Typing notices",
                Some("Let a room see while you're typing.".into()),
                toggle(
                    "settings-typing-notices",
                    "Send typing notices",
                    privacy.typing_notices,
                    false,
                    Self::listener_for(cx, move |this, _, window, cx| {
                        this.set_privacy(
                            PrivacySettings {
                                typing_notices: !privacy.typing_notices,
                                ..privacy
                            },
                            window,
                            cx,
                        );
                    }),
                ),
                &colors,
            ))
            .child(setting_row(
                "Message text in notifications",
                Some(
                    "When off, a notification says who wrote and where, and \u{201c}New \
                     message\u{201d} instead of what they wrote."
                        .into(),
                ),
                toggle(
                    "settings-privacy-notification-text",
                    "Show message text",
                    notifications.show_content,
                    false,
                    Self::listener_for(cx, move |this, _, window, cx| {
                        this.set_notifications(
                            NotificationSettings {
                                show_content: !notifications.show_content,
                                ..notifications
                            },
                            window,
                            cx,
                        );
                    }),
                ),
                &colors,
            ))
            .child(
                div()
                    .pt(px(SPACE_4))
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(readable("settings-privacy-promise", PRIVACY_PROMISE)),
            )
            .into_any_element()
    }

    // -- Appearance ----------------------------------------------------------

    pub(super) fn render_appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = ComponentTheme::get(cx).colors;
        let appearance = self.appearance;
        let root = cx.entity().downgrade();
        let pick =
            move |change: fn(AppearanceSettings, AppearanceSettings) -> AppearanceSettings| {
                let root = root.clone();
                move |value: AppearanceSettings, window: &mut Window, cx: &mut App| {
                    let _ = root.update(cx, |this, cx| {
                        let next = change(this.appearance, value);
                        this.set_appearance(next, window, cx);
                    });
                }
            };
        let theme_pick = pick(|current, value| AppearanceSettings {
            theme: value.theme,
            ..current
        });
        let size_pick = pick(|current, value| AppearanceSettings {
            text_size: value.text_size,
            ..current
        });
        let motion_pick = pick(|current, value| AppearanceSettings {
            motion: value.motion,
            ..current
        });
        let contrast_pick = pick(|current, value| AppearanceSettings {
            contrast: value.contrast,
            ..current
        });
        let with = |change: AppearanceSettings| change;
        div()
            .flex()
            .flex_col()
            .child(section_heading(super::SettingsSection::Appearance.label()))
            .child(setting_row(
                "Theme",
                Some("Light, dark, or your computer's appearance.".into()),
                choice(
                    "settings-theme",
                    "Theme",
                    &ThemeChoice::ALL.map(|theme| (theme, theme.label().to_owned())),
                    appearance.theme,
                    move |theme, window, cx| {
                        theme_pick(
                            with(AppearanceSettings {
                                theme,
                                ..appearance
                            }),
                            window,
                            cx,
                        )
                    },
                ),
                &colors,
            ))
            .child(setting_row(
                "Text size",
                Some("Every window's text, from 100% to 200%.".into()),
                choice(
                    "settings-text-size",
                    "Text size",
                    &TEXT_SIZES.map(|size| (size, format!("{size}%"))),
                    appearance.text_size,
                    move |text_size, window, cx| {
                        size_pick(
                            AppearanceSettings {
                                text_size,
                                ..appearance
                            },
                            window,
                            cx,
                        )
                    },
                ),
                &colors,
            ))
            .child(setting_row(
                "Motion",
                Some(
                    "Reduce stops the spinners and animated pictures. Match system follows your \
                     computer's reduce-motion setting."
                        .into(),
                ),
                choice(
                    "settings-motion",
                    "Motion",
                    &MotionChoice::ALL.map(|motion| (motion, motion.label().to_owned())),
                    appearance.motion,
                    move |motion, window, cx| {
                        motion_pick(
                            AppearanceSettings {
                                motion,
                                ..appearance
                            },
                            window,
                            cx,
                        )
                    },
                ),
                &colors,
            ))
            .child(setting_row(
                "Contrast",
                Some(
                    "Increased makes secondary text and borders stronger. Match system follows \
                     macOS Increase Contrast and Windows contrast themes."
                        .into(),
                ),
                choice(
                    "settings-contrast",
                    "Contrast",
                    &ContrastChoice::ALL.map(|contrast| (contrast, contrast.label().to_owned())),
                    appearance.contrast,
                    move |contrast, window, cx| {
                        contrast_pick(
                            AppearanceSettings {
                                contrast,
                                ..appearance
                            },
                            window,
                            cx,
                        )
                    },
                ),
                &colors,
            ))
            .into_any_element()
    }

    // -- Notifications -------------------------------------------------------

    pub(super) fn render_notifications(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = ComponentTheme::get(cx).colors;
        let notifications = self.notifications;
        let explanation: SharedString = if !notifications.enabled {
            "Family Chat posts no notifications. The window's title still counts your unread \
             messages."
                .into()
        } else if notifications.show_content {
            "Notifications show who wrote, in which room, and what they wrote.".into()
        } else {
            "Notifications show who wrote and in which room, and say \u{201c}New message\u{201d} \
             instead of what they wrote."
                .into()
        };
        div()
            .flex()
            .flex_col()
            .child(section_heading(
                super::SettingsSection::Notifications.label(),
            ))
            .child(
                div()
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .pb(px(SPACE_2))
                    .child(readable(
                        "settings-notifications-detail",
                        "Family Chat tells you about new messages while it is open, through this \
                         computer's notifications. Nothing is sent anywhere else, and the room \
                         in front of you never notifies.",
                    )),
            )
            .child(setting_row(
                "Notifications",
                Some("New messages in the rooms you aren't looking at.".into()),
                toggle(
                    "settings-notifications-enabled",
                    "Show notifications",
                    notifications.enabled,
                    false,
                    Self::listener_for(cx, move |this, _, window, cx| {
                        this.set_notifications(
                            NotificationSettings {
                                enabled: !notifications.enabled,
                                ..notifications
                            },
                            window,
                            cx,
                        );
                    }),
                ),
                &colors,
            ))
            .child(setting_row(
                "Message text",
                Some(explanation.clone()),
                toggle(
                    "settings-notifications-text",
                    "Show message text in notifications",
                    notifications.show_content,
                    !notifications.enabled,
                    Self::listener_for(cx, move |this, _, window, cx| {
                        this.set_notifications(
                            NotificationSettings {
                                show_content: !notifications.show_content,
                                ..notifications
                            },
                            window,
                            cx,
                        );
                    }),
                ),
                &colors,
            ))
            .child(
                div()
                    .pt(px(SPACE_4))
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(readable(
                        "settings-notifications-system",
                        "Your computer's own notification settings decide whether they make a \
                         sound or show while you're busy. When messages arrive quickly, Family \
                         Chat shows one summary instead of many.",
                    )),
            )
            .into_any_element()
    }

    // -- Diagnostics ---------------------------------------------------------

    pub(super) fn render_diagnostics(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = ComponentTheme::get(cx).colors;
        let bootstrap = &self.bootstrap;
        let version = bootstrap.app_version.clone();
        let short_commit: String = bootstrap.upstream.zed_commit.chars().take(12).collect();
        let gpui = format!("Zed {} ({short_commit})", bootstrap.upstream.zed_tag);
        let log_dir = bootstrap.log_dir.clone();
        let logs = log_dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "No log files are kept for this window.".to_owned());
        let diagnostics = format!(
            "{APP_NAME} {version}\nGPUI: {gpui}\nOS: {} {}\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        let open_logs = log_dir.map(|dir| {
            Button::new("settings-open-logs", "Open logs folder")
                .icon("folder")
                .on_click(Self::listener_for(cx, move |this, _, _, cx| {
                    if dir.exists() {
                        cx.open_with_system(&dir);
                    } else {
                        this.toast(
                            Toast::new("There are no log files yet.").kind(ToastKind::Info),
                            cx,
                        );
                    }
                }))
        });
        div()
            .flex()
            .flex_col()
            .child(section_heading(super::SettingsSection::Diagnostics.label()))
            .child(setting_row(
                "Version",
                None,
                row_value("settings-version", version, &colors),
                &colors,
            ))
            .child(setting_row(
                "GPUI",
                Some("The Zed release the window is built on.".into()),
                row_value("settings-gpui", gpui, &colors),
                &colors,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .py(px(SPACE_3))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(super::setting_text(
                        "Logs",
                        Some(
                            "Family Chat's log and crash log (crash.log), on this computer only."
                                .into(),
                        ),
                        &colors,
                    ))
                    .child(
                        div()
                            .id("settings-logs-box")
                            .p(px(SPACE_2))
                            .rounded(px(RADIUS_LG))
                            .bg(rgb(colors.surface))
                            .border_1()
                            .border_color(rgb(colors.border))
                            .font_family(crate::rooms::rich_text::MONO_FONT)
                            .text_sm()
                            .child(readable("settings-logs-path", logs)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(SPACE_2))
                            .children(open_logs)
                            .child(
                                Button::new("settings-copy-diagnostics", "Copy version details")
                                    .icon("copy")
                                    .on_click(Self::listener_for(cx, move |this, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            diagnostics.clone(),
                                        ));
                                        this.toast(
                                            Toast::new("Copied the version details.")
                                                .kind(ToastKind::Success),
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .id("settings-nothing-uploaded")
                    .mt(px(SPACE_4))
                    .flex()
                    .items_start()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_3))
                    .rounded(px(RADIUS_LG))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .child(icon("shield-check", ICON_SIZE_MD, colors.success))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .type_style(TYPE_CAPTION)
                            .child(readable("settings-nothing-uploaded-text", NOTHING_UPLOADED)),
                    ),
            )
            .into_any_element()
    }
}

/// What a page that needs an account says without one.
fn no_account(id: &'static str, colors: &UiColors) -> AnyElement {
    div()
        .type_style(TYPE_CAPTION)
        .text_color(rgb(colors.muted))
        .child(readable(
            id,
            "Nobody is signed in. Sign in from the main window.",
        ))
        .into_any_element()
}
