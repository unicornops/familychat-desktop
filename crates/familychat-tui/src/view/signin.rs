//! The sign-in screens, drawn from the current [`SigninStep`]. Every
//! sentence a step carries is the sign-in's (`familychat-signin`'s
//! `wording`), filtered for the terminal like any server text, since a
//! step can name a server or a Matrix ID taken from a link.

use familychat_core::{
    identity::APP_NAME,
    signin::{BrowserKind, BrowserPurpose, PasswordKind, SigninStep},
};
use familychat_security::terminal::{sanitize, sanitize_line};
use ratatui::{Frame, layout::Rect, style::Style, text::Span};

use super::{Theme, centred, draw_pane, paragraph};
use crate::{
    app::{App, Screen},
    pane::{PaneId, PaneLine},
    signin::{Field, OPTIONS, Page, SigninState},
    text::{self, truncate_start},
};

/// The title bar's name for the page.
pub(super) fn title(signin: &SigninState) -> &'static str {
    match signin.page {
        Page::Welcome => "WELCOME",
        Page::Code => "SIGN IN WITH A CODE",
        Page::Flow => match &signin.step {
            SigninStep::Confirm(_) => "CONFIRM",
            SigninStep::Password(form) if form.kind == PasswordKind::OtherServer => {
                "ANOTHER MATRIX SERVER"
            }
            SigninStep::Password(_) => "SIGN IN WITH YOUR PASSWORD",
            SigninStep::Server(_) => "ANOTHER MATRIX SERVER",
            SigninStep::ConfirmBrowser(_) => "CONFIRM",
            SigninStep::Browser(_) => "IN YOUR BROWSER",
            SigninStep::AlreadySignedIn(_) => "ALREADY SIGNED IN",
            _ => "SIGNING IN",
        },
    }
}

/// Lines of our own and the sign-in's text, `indent` cells in.
struct Card {
    lines: Vec<PaneLine>,
    indent: usize,
    width: usize,
    theme: Theme,
}

impl Card {
    fn line(&mut self, spans: Vec<Span<'static>>) -> &mut PaneLine {
        let mut all = vec![Span::raw(" ".repeat(self.indent))];
        all.extend(spans);
        self.lines.push(PaneLine {
            spans: all,
            prefix: self.indent,
            ..PaneLine::default()
        });
        self.lines
            .last_mut()
            .unwrap_or_else(|| unreachable!("just pushed"))
    }

    fn heading(&mut self, text: &str) {
        let text = self.theme.words(text);
        self.line(vec![Span::styled(text, self.theme.bold())]);
    }

    fn text(&mut self, text: &str, style: Style) {
        let text = self.theme.words(text);
        self.lines.extend(paragraph(
            &text,
            style,
            self.indent,
            self.indent + self.width,
        ));
    }

    fn blank(&mut self) {
        self.lines.push(PaneLine::default());
    }

    /// A key and what it does, as the command bar shows them.
    fn key(&mut self, key: &str, label: &str) {
        let theme = self.theme;
        self.line(vec![
            Span::styled(key.to_owned(), theme.reversed()),
            Span::raw(format!(" {}", theme.words(label))),
        ]);
    }

    /// A form field: its label, then its value (or what an empty one
    /// shows), with a block where the typing goes when it has focus.
    fn field(&mut self, label: &str, value: &str, placeholder: &str, focused: bool) {
        let theme = self.theme;
        let label = format!("{label:<22} ");
        let room = self.width.saturating_sub(text::width(&label) + 2);
        let mut spans = vec![Span::styled(label, theme.bold())];
        if value.is_empty() && !placeholder.is_empty() {
            spans.push(Span::styled(placeholder.to_owned(), theme.faint()));
        } else {
            spans.push(Span::raw(truncate_start(value, room, theme.ellipsis())));
        }
        if focused {
            spans.push(Span::styled(" ", theme.reversed()));
        }
        self.line(spans);
    }
}

pub(super) fn draw(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    let Screen::Signin(signin) = &app.screen else {
        return;
    };
    let width = usize::from(body.width.min(78)).saturating_sub(4);
    let indent = usize::from(body.width.saturating_sub(78) / 2) + 2;
    let mut card = Card {
        lines: Vec::new(),
        indent,
        width,
        theme,
    };
    match signin.page {
        Page::Welcome => welcome(&mut card, signin),
        Page::Code => code(&mut card, signin),
        Page::Flow => flow(&mut card, signin),
    }
    let lines = centred(card.lines, usize::from(body.height));
    draw_pane(app, frame, PaneId::Signin, body, lines, None);
}

fn welcome(card: &mut Card, signin: &SigninState) {
    let theme = card.theme;
    card.heading(&format!("Welcome to {APP_NAME}"));
    card.text(
        "Chat with your family. Choose how you'd like to sign in.",
        Style::new(),
    );
    card.blank();
    let details = [
        "Paste the sign-in link your parent gave you.",
        "Use your family's address, your username and your password.",
        "Sign in to any Matrix homeserver.",
    ];
    for (index, (cmd, detail)) in OPTIONS.iter().zip(details).enumerate() {
        let selected = index == signin.cursor;
        let pointer = if selected { theme.pointer() } else { " " };
        let name = cmd.name();
        let name_style = if selected {
            theme.reversed()
        } else {
            theme.bold()
        };
        card.line(vec![
            Span::raw(format!("{pointer} ")),
            Span::styled(cmd.key(), theme.reversed()),
            Span::raw(" "),
            Span::styled(name.to_owned(), name_style),
        ])
        .target = Some(index);
        card.line(vec![Span::styled(
            format!("    {}", theme.words(detail)),
            theme.faint(),
        )])
        .target = Some(index);
    }
    card.blank();
    card.text(
        "Paste a sign-in link here at any time to use it. This is alpha software.",
        theme.faint(),
    );
}

fn code(card: &mut Card, signin: &SigninState) {
    let theme = card.theme;
    card.heading("Sign in with a code from your parent");
    card.text(
        &format!(
            "Paste the sign-in link your parent gave you. {APP_NAME} asks you before it uses \
             the code."
        ),
        Style::new(),
    );
    card.blank();
    let masked: String = std::iter::repeat_n(theme.masked(), signin.link.len().min(24)).collect();
    let count = signin.link.len();
    let shown = if count == 0 {
        String::new()
    } else {
        format!("{masked} ({count} characters)")
    };
    card.field("Sign-in link", &shown, "Paste the sign-in link", true);
    if let Some(problem) = &signin.code_problem {
        card.blank();
        card.text(&sanitize(problem), theme.danger());
    }
    card.blank();
    card.text(
        "Paste with your terminal (Ctrl-Shift-V, or Cmd-V on a Mac), then press Enter. A QR \
         code image can be pasted or dropped in the Family Chat window.",
        theme.faint(),
    );
}

fn flow(card: &mut Card, signin: &SigninState) {
    let theme = card.theme;
    match &signin.step {
        SigninStep::Confirm(confirm) => {
            card.heading(&sanitize_line(&confirm.question));
            card.blank();
            card.text(&sanitize(&confirm.detail), Style::new());
            card.blank();
            card.key("Y", "Continue");
            card.key("P", "Use my password instead");
            card.key("N", "Cancel");
        }
        SigninStep::Starting => card.heading("Starting…"),
        SigninStep::Working(working) => {
            card.heading(working.message());
            card.text("This takes a few seconds.", theme.faint());
        }
        SigninStep::Password(form) => {
            let (heading, detail, label) = match (form.kind, &form.methods) {
                (PasswordKind::FamilyChat, _) => (
                    "Sign in with your password",
                    "Use your family's address, your username and your password.",
                    "Your family's address",
                ),
                (PasswordKind::OtherServer, None) => (
                    "Use another Matrix server",
                    "Sign in to any Matrix homeserver with your username and password.",
                    "Homeserver",
                ),
                (PasswordKind::OtherServer, Some(_)) => (
                    "Use another Matrix server",
                    "Choose how to sign in. Only the ways this server offers are shown.",
                    "Homeserver",
                ),
            };
            card.heading(heading);
            card.text(detail, theme.faint());
            if let Some(notice) = &form.notice {
                card.blank();
                card.text(&sanitize(notice), theme.info());
            }
            card.blank();
            if form.server_is_fixed() {
                card.field(label, &sanitize_line(&form.server), "", false);
            } else {
                card.field(
                    label,
                    &sanitize_line(&signin.server),
                    &sanitize_line(&form.server_placeholder),
                    signin.focus == Field::Server,
                );
            }
            if let Some(methods) = &form.methods {
                if let Some(browser) = &methods.browser {
                    card.blank();
                    card.key("Ctrl-B", "Continue in your browser");
                    card.text(
                        &match browser.kind {
                            BrowserKind::OAuth => {
                                format!("You sign in at {}", sanitize_line(&browser.address))
                            }
                            BrowserKind::Sso => format!(
                                "Your server's single sign-on, at {}",
                                sanitize_line(&browser.address)
                            ),
                        },
                        theme.faint(),
                    );
                }
                if methods.qr_code {
                    card.blank();
                    card.text(
                        "This server also signs in with a QR code: use the Family Chat window                          for that.",
                        theme.faint(),
                    );
                }
                if form.server_is_fixed() {
                    card.key("Ctrl-E", "Change server");
                }
                if !form.shows_password() {
                    if let Some(problem) = &form.problem {
                        card.blank();
                        card.text(&sanitize(&problem.message), theme.danger());
                    }
                    return;
                }
                card.blank();
            }
            card.field(
                "Username or Matrix ID",
                &sanitize_line(&signin.username),
                "",
                signin.focus == Field::Username,
            );
            let masked: String =
                std::iter::repeat_n(theme.masked(), signin.password.len().min(32)).collect();
            card.field("Password", &masked, "", signin.focus == Field::Password);
            if let Some(problem) = &form.problem {
                card.blank();
                card.text(&sanitize(&problem.message), theme.danger());
            }
            if let Some(seconds) = signin.seconds_to_wait() {
                card.text(
                    &format!("You can sign in again in {seconds} seconds."),
                    theme.warning(),
                );
            }
            card.blank();
            card.text(
                "Tab moves between the fields. Enter signs in from the password.",
                theme.faint(),
            );
        }
        SigninStep::AlreadySignedIn(message) => {
            card.heading("Already signed in");
            card.blank();
            card.text(&sanitize(message), Style::new());
            card.blank();
            card.key("Enter", "Back to sign-in");
        }
        SigninStep::Server(form) => {
            card.heading("Use another Matrix server");
            card.text(
                "Type the server's address. Family Chat then shows the ways it offers to sign in.",
                theme.faint(),
            );
            card.blank();
            card.field(
                "Homeserver",
                &sanitize_line(&signin.server),
                &sanitize_line(&form.server_placeholder),
                true,
            );
            if let Some(problem) = &form.problem {
                card.blank();
                card.text(&sanitize(&problem.message), theme.danger());
            }
            card.blank();
            card.text("Enter finds the server.", theme.faint());
        }
        SigninStep::ConfirmBrowser(confirm) => {
            card.heading(&sanitize_line(&confirm.question));
            card.blank();
            // The whole address, never shortened.
            card.text(&sanitize_line(&confirm.address), theme.bold());
            card.blank();
            card.text(&sanitize(&confirm.detail), Style::new());
            card.blank();
            card.key("Y", "Continue");
            card.key("N", "Cancel");
        }
        SigninStep::Browser(browser) => {
            card.heading(match browser.purpose {
                BrowserPurpose::SignIn => "Waiting for your browser",
                BrowserPurpose::ApproveDevice => "Approve the new device",
            });
            card.blank();
            card.text(&sanitize(&browser.message), Style::new());
            card.text(&sanitize_line(&browser.address), theme.bold());
            card.blank();
            card.key("O", "Open the browser again");
            card.key("C", "Copy the address, for a browser elsewhere");
            card.key("N", "Cancel");
        }
        SigninStep::QrCode(_) | SigninStep::CheckCode(_) => {
            card.heading("Use the Family Chat window");
            card.text(
                "QR codes are drawn in the window only. Press Esc to go back.",
                theme.faint(),
            );
        }
        SigninStep::DeviceLinked(message) | SigninStep::Stopped(message) => {
            card.heading("Sign in a new device");
            card.blank();
            card.text(&sanitize(message), Style::new());
            card.blank();
            card.key("Enter", "OK");
        }
        SigninStep::Finished(_) => card.heading("Opening your account…"),
        SigninStep::NotALink(_) | SigninStep::Cancelled => {}
    }
}
