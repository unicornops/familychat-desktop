//! The terminal UI's state and its event handling (ADR 0008).
//!
//! [`App`] opens the profile through `familychat-core`'s
//! [`StartupApi`], as the window does, then shows the sign-in while
//! nobody is signed in and the rooms once someone is. Every answer from
//! the session and the sign-in arrives on a `tokio::sync` channel that
//! [`App::tick`] polls with `try_recv` (ADR 0005): nothing here waits on
//! the network, the disk or the keychain.
//!
//! The mouse, the selection and the copy follow Zmail's `zmail-tui`
//! (`src/app.rs`, Copyright UnicornOps, GPL-3.0-only like this project):
//! Zellij's model, where a drag selects inside one pane and copies on
//! release, Shift leaves the mouse to the terminal, and a paste only ever
//! goes into a text field.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use familychat_core::{
    identity::APP_NAME,
    links::LinkPrompt,
    media::MediaInfo,
    messaging::MessageRef,
    rooms::{RoomListFilter, RoomSummary},
    session::{AccountView, IncomingVerification},
    startup::{OpenState, Opened, StartupApi},
    timeline,
};
use familychat_security::terminal::sanitize_line;
use ratatui::layout::Rect;
use tokio::sync::{mpsc as tokio_mpsc, oneshot, watch};

use crate::{
    clipboard::CopySettings,
    commands::Cmd,
    palette::Palette,
    pane::{DrawnPane, PaneId, Point, Selection, logical_line, selected_text, word_at},
    rooms::{Focus, RoomsState},
    signin::SigninState,
    tasks::Tasks,
    writing::QUICK_REACTIONS,
};

/// The quick reactions' names on the command bar.
const QUICK_REACTION_LABELS: [&str; 6] = ["ThumbsUp", "Heart", "Laugh", "Wow", "Sad", "Party"];

/// Clicks closer together than this count as a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
/// Lines the mouse wheel scrolls.
pub(crate) const WHEEL_LINES: usize = 3;
/// The narrowest terminal that shows the room list and the room side by
/// side (PLAN.md, section 13.10).
pub const SIDE_BY_SIDE_FROM: u16 = 120;

/// What the terminal and the session can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Environment {
    /// Colours are allowed (`NO_COLOR` is not set).
    pub color: bool,
    /// The terminal takes UTF-8, so box drawing can be used.
    pub unicode: bool,
    /// The session runs over SSH.
    pub over_ssh: bool,
    /// Running on macOS.
    pub macos: bool,
}

impl Environment {
    /// Reads the environment: `NO_COLOR`, the locale's character set, and
    /// `SSH_CONNECTION`/`SSH_TTY` (as Zmail's terminal UI does).
    pub fn detect() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty());
        let unicode = cfg!(windows)
            || locale.is_none_or(|locale| {
                let locale = locale.to_ascii_lowercase();
                locale.contains("utf-8") || locale.contains("utf8")
            });
        Self {
            color: !set("NO_COLOR"),
            unicode,
            over_ssh: crate::clipboard::over_ssh(),
            macos: cfg!(target_os = "macos"),
        }
    }
}

/// The clock and how times read: `familychat-core`'s wording in the local
/// time zone for the app, a fixed one for the snapshot tests.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    /// Now, in milliseconds since the Unix epoch.
    pub now_ms: fn() -> u64,
    /// A message's time ("14:05").
    pub time: fn(u64) -> String,
    /// A day divider ("Today"), for a time and now.
    pub day: fn(u64, u64) -> String,
}

impl Clock {
    /// The system clock and the local time zone.
    pub fn local() -> Self {
        Self {
            now_ms: system_now_ms,
            time: timeline::time_label,
            day: timeline::day_label,
        }
    }
}

fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The person's choices for this run (`familychat tui --bell --ascii
/// --no-mouse --editor`, and `FAMILYCHAT_TUI_COPY_COMMAND`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Ring the terminal bell for new messages.
    pub bell: bool,
    /// Capture the mouse (clicks, the wheel, the selection).
    pub mouse: bool,
    /// Draw with ASCII only, even in a UTF-8 terminal.
    pub ascii: bool,
    /// Where copies go.
    pub copy: CopySettings,
    /// The composer's `^_` opens `$VISUAL` or `$EDITOR` (off unless asked
    /// for: the message is written unencrypted to a temporary file while
    /// the editor is open).
    pub external_editor: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bell: false,
            mouse: true,
            ascii: false,
            copy: CopySettings::default(),
            external_editor: false,
        }
    }
}

/// Something the app asks of the terminal or the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Put this text on the clipboard.
    Copy(String),
    /// Ring the terminal bell.
    Bell,
    /// Turn mouse capture on or off.
    MouseCapture(bool),
    /// Open this address, which the person confirmed, with the system's
    /// opener.
    Open(String),
    /// Clear and redraw the whole screen.
    Redraw,
    /// Hand the terminal to the external editor on this text, then give
    /// what it saved to [`App::external_edit_done`].
    Edit {
        /// The editor and its arguments.
        command: Vec<String>,
        /// The text.
        text: String,
    },
}

/// Where the terminal UI starts (the fixtures; the app starts on
/// [`InitialView::Default`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum InitialView {
    /// Wherever the profile leads.
    #[default]
    Default,
    /// The command palette, open over the first screen.
    Palette,
    /// The code page.
    CodeEntry,
    /// The code page with this text typed into the link field.
    TypedLink(String),
    /// This text pasted as a sign-in link.
    PastedLink(String),
    /// "Sign in with your password".
    Password,
    /// "Use another Matrix server".
    OtherServer,
    /// The rooms, opened like this.
    Rooms(RoomsLaunch),
}

/// How the rooms open (the fixtures).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomsLaunch {
    /// The filter.
    pub filter: RoomListFilter,
    /// The room to open, by id.
    pub room: Option<String>,
    /// Show its members.
    pub members: bool,
    /// Ask about its link or file with this number.
    pub reference: Option<usize>,
    /// Ask where to save the file with this number.
    pub save: Option<usize>,
    /// Show its spoilers.
    pub spoilers: bool,
    /// Select the account's newest message.
    pub select_own: bool,
    /// A file of this name and size waiting in the composer.
    pub file: Option<(String, usize)>,
    /// Run these commands, in turn, once everything asked for has arrived.
    pub commands: Vec<crate::commands::Cmd>,
    /// Then type this, key by key (`\n` is `^J`, `\r` is Enter).
    pub typed: Option<String>,
}

/// What [`App::new`] needs.
pub struct AppOptions {
    /// Opens the profile.
    pub startup: Arc<dyn StartupApi>,
    /// What the terminal can do.
    pub env: Environment,
    /// The clock.
    pub clock: Clock,
    /// The person's choices.
    pub settings: Settings,
    /// The app's version, for the title bar.
    pub version: String,
    /// Where it starts.
    pub initial: InitialView,
    /// Where the save question starts: the Downloads folder, else home.
    pub downloads: PathBuf,
}

/// The profile screens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProfileScreen {
    /// Opening (the keychain and the store, off the UI thread).
    Opening,
    /// The keychain is locked or can't be reached: Try again.
    Keychain(String),
    /// The data's key is gone: Start over.
    LostKey(String),
    /// Something else failed: Try again.
    Failed(String),
}

/// What the terminal shows.
pub(crate) enum Screen {
    /// The profile is opening or not ready.
    Profile(ProfileScreen),
    /// Nobody is signed in.
    Signin(Box<SigninState>),
    /// An account is signed in.
    Rooms(Box<RoomsState>),
}

/// A question or a field on the status line, or a dialog over the body.
#[derive(Debug, Clone)]
pub(crate) enum Prompt {
    /// The command palette.
    Palette(Palette),
    /// "Open this link?", with the whole address.
    Link(LinkPrompt),
    /// "Link or file number:".
    RefNumber(String),
    /// "Save NAME to:", with the path being typed.
    Save {
        /// The file.
        media: MediaInfo,
        /// The path field.
        input: String,
    },
    /// "NAME is already there. Replace it?"
    Replace {
        /// The file.
        media: MediaInfo,
        /// Where it would go.
        path: PathBuf,
    },
    /// "Start over?"
    StartOver,
    /// "Attach a file:", with the path being typed and the names Tab
    /// found.
    Attach {
        /// The path field.
        input: String,
        /// The names that match, when more than one does.
        choices: Vec<String>,
    },
    /// The quick reaction picker for this message.
    React(MessageRef),
    /// "Delete this message?"
    Delete {
        /// The message.
        target: MessageRef,
        /// One line of it.
        preview: String,
    },
    /// "Leave ROOM?"
    Leave(RoomSummary),
}

/// A command bar entry: its key as shown, its label and its command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEntry {
    /// The key shown.
    pub key: String,
    /// What it does, in a word or two.
    pub label: &'static str,
    /// The command.
    pub cmd: Cmd,
}

impl CommandEntry {
    /// `cmd` on its own key.
    pub(crate) fn of(cmd: Cmd) -> Self {
        Self {
            key: cmd.key(),
            label: cmd.label(),
            cmd,
        }
    }

    /// `cmd` shown on `key` with `label` (an answer, or Quit on `^C` in a
    /// text field).
    pub(crate) fn with(key: &str, label: &'static str, cmd: Cmd) -> Self {
        Self {
            key: key.to_owned(),
            label,
            cmd,
        }
    }
}

/// The mouse button's state.
#[derive(Debug, Default)]
struct MouseState {
    press: Option<(PaneId, Point, u8)>,
    dragged: bool,
    last_press: Option<(Instant, u16, u16, u8)>,
    position: (u16, u16),
}

/// The terminal UI.
pub struct App {
    startup: Arc<dyn StartupApi>,
    pub(crate) env: Environment,
    pub(crate) clock: Clock,
    pub(crate) settings: Settings,
    pub(crate) version: String,
    pub(crate) downloads: PathBuf,

    pub(crate) screen: Screen,
    pub(crate) prompt: Option<Prompt>,
    pub(crate) status: Option<String>,
    pub(crate) command_page: usize,
    opening: Option<oneshot::Receiver<OpenState>>,
    pub(crate) opened: Option<Opened>,
    accounts: Option<watch::Receiver<Vec<AccountView>>>,
    pub(crate) initial: Option<InitialView>,

    /// The panes of the last frame.
    pub(crate) panes: Vec<DrawnPane>,
    /// The command bar's entries on screen, for clicks.
    pub(crate) command_targets: Vec<(Rect, Cmd)>,
    /// Each pane's first line shown.
    pub(crate) scroll: HashMap<PaneId, usize>,
    /// Panes whose cursor moved and must be scrolled into view.
    pub(crate) reveal: HashSet<PaneId>,
    pub(crate) selection: Option<Selection>,
    mouse: MouseState,
    /// The terminal size at the last frame.
    pub(crate) size: (u16, u16),
    pub(crate) effects: Vec<Effect>,
    /// Messages from work running elsewhere (a copy command).
    notices: (mpsc::Sender<String>, mpsc::Receiver<String>),
    /// Ticks so far, for the spinner.
    pub(crate) ticks: u64,
    /// The actions running off the UI thread.
    pub(crate) tasks: Tasks,
    /// The verification requests other devices send.
    verification_requests: Option<tokio_mpsc::Receiver<IncomingVerification>>,
    /// Something changed in the last tick.
    pub(crate) changed: bool,
    quit: bool,
}

impl App {
    /// Starts opening the profile.
    pub fn new(options: AppOptions) -> Self {
        let opening = Some(options.startup.open());
        Self {
            startup: options.startup,
            env: options.env,
            clock: options.clock,
            settings: options.settings,
            version: options.version,
            downloads: options.downloads,
            screen: Screen::Profile(ProfileScreen::Opening),
            prompt: None,
            status: None,
            command_page: 0,
            opening,
            opened: None,
            accounts: None,
            initial: Some(options.initial),
            panes: Vec::new(),
            command_targets: Vec::new(),
            scroll: HashMap::new(),
            reveal: HashSet::new(),
            selection: None,
            mouse: MouseState::default(),
            size: (80, 24),
            effects: Vec::new(),
            notices: mpsc::channel(),
            ticks: 0,
            tasks: Tasks::default(),
            verification_requests: None,
            changed: false,
            quit: false,
        }
    }

    /// Whether the person asked to quit.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// What the app asks of the terminal, since the last call.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// The person's choices.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Where work running elsewhere reports to the status line.
    pub fn notice_sender(&self) -> mpsc::Sender<String> {
        self.notices.0.clone()
    }

    /// The signed-in account shown, if any.
    pub fn account(&self) -> Option<&AccountView> {
        match &self.screen {
            Screen::Rooms(rooms) => Some(&rooms.account),
            _ => None,
        }
    }

    /// The status line's message, if any.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Whether everything asked for so far has arrived: the profile is
    /// open or not ready, the fixture's first view has been reached, and
    /// the last tick changed nothing (the snapshot tests wait for this).
    pub fn is_settled(&self) -> bool {
        self.opening.is_none()
            && self.initial.is_none()
            && !self.changed
            && self.pending_work() == 0
    }

    /// How long the loop waits for input before ticking again.
    pub fn tick_interval(&self) -> Duration {
        Duration::from_millis(100)
    }

    /// The services, once the profile is open.
    pub(crate) fn opened(&self) -> Option<&Opened> {
        self.opened.as_ref()
    }

    // ------------------------------------------------------------------
    // Ticks: everything that arrived
    // ------------------------------------------------------------------

    /// Polls every channel: the profile, the accounts, the sign-in, the
    /// rooms and timelines, the saves; applies the first view; keeps a
    /// drag past a pane's edge scrolling.
    pub fn tick(&mut self) {
        self.ticks += 1;
        self.changed = false;
        if let Some(receiver) = self.opening.as_mut() {
            match receiver.try_recv() {
                Ok(state) => {
                    self.opening = None;
                    self.opened_as(state);
                    self.changed = true;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.opening = None;
                    self.screen = Screen::Profile(ProfileScreen::Failed(format!(
                        "{APP_NAME} stopped while opening its data."
                    )));
                    self.changed = true;
                }
            }
        }
        self.follow_accounts();
        let changed = match &mut self.screen {
            Screen::Signin(_) => self.signin_tick(),
            Screen::Rooms(_) => self.rooms_tick(),
            Screen::Profile(_) => false,
        };
        self.changed |= changed;
        let mut requests = Vec::new();
        if let Some(receiver) = self.verification_requests.as_mut() {
            while let Ok(request) = receiver.try_recv() {
                requests.push(request);
            }
        }
        for (account, flow) in requests {
            self.verification_request(account, flow);
            self.changed = true;
        }
        while let Ok(notice) = self.notices.1.try_recv() {
            self.status = Some(notice);
            self.changed = true;
        }
        self.apply_initial();
        self.drag_past_edge();
    }

    fn opened_as(&mut self, state: OpenState) {
        self.screen = match state {
            OpenState::Ready(opened) => {
                if let Some(notice) = &opened.notice {
                    self.status = Some(sanitize_line(notice));
                }
                let accounts = opened.session.accounts();
                // This frontend hears the verification requests now (the
                // session declines them while nobody listens).
                self.verification_requests = Some(opened.session.verification_requests());
                self.opened = Some(opened);
                self.accounts = Some(accounts);
                // The accounts decide the screen, at once.
                let first = self
                    .accounts
                    .as_mut()
                    .and_then(|accounts| accounts.borrow_and_update().first().cloned());
                match first {
                    Some(account) => self.rooms_screen(account),
                    None => self.signin_screen(),
                }
            }
            OpenState::KeychainUnavailable(message) => {
                Screen::Profile(ProfileScreen::Keychain(message))
            }
            OpenState::Unreadable(message) => Screen::Profile(ProfileScreen::LostKey(message)),
            OpenState::Failed(message) => Screen::Profile(ProfileScreen::Failed(message)),
        };
        self.prompt = self
            .prompt
            .take()
            .filter(|prompt| matches!(prompt, Prompt::Palette(_)));
    }

    /// Follows the accounts: the first account signed in shows its rooms.
    fn follow_accounts(&mut self) {
        let Some(accounts) = self.accounts.as_mut() else {
            return;
        };
        if !accounts.has_changed().unwrap_or(false) {
            return;
        }
        let list = accounts.borrow_and_update().clone();
        let shown = self.account().map(|account| account.key.clone());
        match (list.first(), shown) {
            (Some(first), None) => {
                // A sign-in finished (or another account arrived first).
                self.screen = self.rooms_screen(first.clone());
                self.changed = true;
            }
            (None, Some(_)) => {
                self.screen = self.signin_screen();
                self.changed = true;
            }
            _ => {}
        }
    }

    /// Work asked for whose answer has not arrived.
    pub(crate) fn pending_work(&self) -> usize {
        self.tasks.running()
            + match &self.screen {
                Screen::Rooms(rooms) => rooms.pending(),
                Screen::Signin(signin) => usize::from(
                    matches!(signin.step, familychat_core::signin::SigninStep::Starting)
                        && signin.flow.is_some(),
                ),
                Screen::Profile(_) => 0,
            }
    }

    /// The first view the fixture asked for, once the screen allows it.
    fn apply_initial(&mut self) {
        let Some(initial) = self.initial.take() else {
            return;
        };
        let done = match (&initial, &self.screen) {
            (InitialView::Default, _) => true,
            (_, Screen::Profile(ProfileScreen::Opening)) => false,
            (InitialView::Palette, _) => {
                self.run(Cmd::Palette);
                true
            }
            (InitialView::Rooms(launch), Screen::Rooms(_)) => {
                match self.launch_rooms(launch.clone()) {
                    None => true,
                    Some(rest) => {
                        self.initial = Some(InitialView::Rooms(rest));
                        return;
                    }
                }
            }
            (_, Screen::Signin(_)) => {
                self.launch_signin(initial.clone());
                true
            }
            _ => true,
        };
        if !done {
            self.initial = Some(initial);
        } else {
            self.changed = true;
        }
    }

    // ------------------------------------------------------------------
    // Events
    // ------------------------------------------------------------------

    /// Handles one terminal event.
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
            Event::Mouse(mouse) => self.on_mouse(mouse),
            Event::Paste(text) => self.on_paste(text),
            Event::Resize(width, height) => self.size = (width, height),
            _ => {}
        }
    }

    fn on_paste(&mut self, pasted: String) {
        // A paste is text for the focused field, never commands.
        match self.prompt.as_mut() {
            Some(Prompt::Palette(palette)) => {
                palette.query.push_str(&sanitize_line(&pasted));
                palette.filter();
                return;
            }
            Some(Prompt::Save { input, .. }) => {
                input.push_str(&sanitize_line(&pasted));
                return;
            }
            Some(Prompt::RefNumber(input)) => {
                input.extend(pasted.chars().filter(char::is_ascii_digit));
                return;
            }
            Some(Prompt::Attach { input, choices }) => {
                input.push_str(&sanitize_line(&pasted));
                choices.clear();
                return;
            }
            Some(_) => return,
            None => {}
        }
        if matches!(self.screen, Screen::Signin(_)) {
            self.signin_paste(pasted);
        } else if self.encryption_has_keys() {
            self.encryption_paste(pasted);
        } else if self.composer_paste(&pasted) {
        } else {
            self.status = Some("Pasting works in text fields only".to_owned());
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('l') {
            self.effects.push(Effect::Redraw);
            return;
        }
        if self.prompt.is_some() {
            self.prompt_key(key);
            return;
        }
        if control && key.code == KeyCode::Char('c') {
            if self.selection.is_some() {
                self.copy_selection();
            } else {
                self.quit = true;
            }
            return;
        }
        self.status = None;
        // A form takes letters as text.
        if let Screen::Signin(_) = self.screen
            && self.signin_takes_text()
        {
            self.signin_key(key);
            return;
        }
        if key.code == KeyCode::Esc && self.selection.take().is_some() {
            return;
        }
        // "Confirm it's you" and the encryption dialogs answer first.
        if self.encryption_has_keys() {
            if self.encryption_key(key) {
                return;
            }
            if !matches!(key.code, KeyCode::Char('q' | ':')) {
                return;
            }
        } else if self.composer_focused() {
            // Letters are text in the composer: only its own keys, the
            // table's Ctrl keys and the arrows' movement are commands.
            if self.composer_key(key) {
                return;
            }
            if !control {
                return;
            }
        }
        if let Some(cmd) = self.command_for(key) {
            self.run(cmd);
        }
    }

    /// The command `key` gives here.
    fn command_for(&self, key: KeyEvent) -> Option<Cmd> {
        let entries = self.commands();
        // The table's keys first, as the table defines them.
        if let Some(entry) = entries.iter().find(|entry| entry.cmd.matches(&key)) {
            return Some(entry.cmd);
        }
        let has = |cmd: Cmd| entries.iter().any(|entry| entry.cmd == cmd);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control {
            return None;
        }
        let cmd = match key.code {
            KeyCode::Enter | KeyCode::Right => Cmd::Select,
            KeyCode::Up => Cmd::Up,
            KeyCode::Down => Cmd::Down,
            KeyCode::PageUp => Cmd::PageUp,
            KeyCode::PageDown => Cmd::PageDown,
            KeyCode::Home => return self.home_end(true),
            KeyCode::End => return self.home_end(false),
            KeyCode::Tab => Cmd::NextRef,
            KeyCode::BackTab => Cmd::PrevRef,
            KeyCode::Left | KeyCode::Esc => Cmd::Back,
            KeyCode::Char(ch) => match ch {
                '<' | ',' => Cmd::Back,
                '>' | '.' => Cmd::Select,
                'k' => Cmd::Up,
                'j' => Cmd::Down,
                '-' => Cmd::PageUp,
                ' ' => Cmd::PageDown,
                'l' => Cmd::OpenRef,
                's' if has(Cmd::StartOver) => Cmd::StartOver,
                's' => Cmd::SaveFile,
                'a' => Cmd::Accept,
                'x' => Cmd::Decline,
                'S' => Cmd::SendAnyway,
                'w' => Cmd::Acknowledge,
                'y' => Cmd::Copy,
                't' => Cmd::TryAgain,
                'v' => Cmd::Spoilers,
                'b' => Cmd::Bell,
                'O' => Cmd::OtherCmds,
                _ => return None,
            },
            _ => return None,
        };
        // Movement works wherever there is something to move; the rest
        // only where the bar offers it.
        let movement = matches!(
            cmd,
            Cmd::Select
                | Cmd::Up
                | Cmd::Down
                | Cmd::PageUp
                | Cmd::PageDown
                | Cmd::NextRef
                | Cmd::PrevRef
                | Cmd::Back
                | Cmd::OtherCmds
                | Cmd::Copy
        );
        (movement || has(cmd)).then_some(cmd)
    }

    /// Whether the open room's composer has the keys.
    pub(crate) fn composer_focused(&self) -> bool {
        self.rooms_ref().is_some_and(|rooms| {
            rooms.focus == Focus::Composer
                && rooms
                    .open
                    .as_ref()
                    .is_some_and(|open| open.composer.is_some())
        })
    }

    fn home_end(&self, home: bool) -> Option<Cmd> {
        match &self.screen {
            Screen::Rooms(rooms) if rooms.room_focused() => {
                Some(if home { Cmd::PageUp } else { Cmd::JumpToLatest })
            }
            _ => None,
        }
    }

    /// The command bar's entries for this screen, every page.
    pub fn commands(&self) -> Vec<CommandEntry> {
        match &self.prompt {
            Some(Prompt::Palette(_)) => {
                return vec![
                    CommandEntry::with("Enter", "Run", Cmd::Select),
                    CommandEntry::with("Esc", "Close", Cmd::GoBack),
                ];
            }
            Some(Prompt::Link(prompt)) => {
                let open = if prompt.is_mailto() { "Write" } else { "Open" };
                return vec![
                    CommandEntry::with("Y", open, Cmd::Answer('y')),
                    CommandEntry::with("C", "Copy", Cmd::Answer('c')),
                    CommandEntry::with("N", "Cancel", Cmd::Answer('n')),
                ];
            }
            Some(Prompt::RefNumber(_) | Prompt::Save { .. }) => {
                return vec![
                    CommandEntry::with("Enter", "OK", Cmd::Select),
                    CommandEntry::with("Esc", "Cancel", Cmd::GoBack),
                ];
            }
            Some(Prompt::Replace { .. } | Prompt::StartOver) => {
                return vec![
                    CommandEntry::with("Y", "Yes", Cmd::Answer('y')),
                    CommandEntry::with("N", "No", Cmd::Answer('n')),
                ];
            }
            Some(Prompt::Attach { .. }) => {
                return vec![
                    CommandEntry::with("Enter", "Attach", Cmd::Select),
                    CommandEntry::with("Tab", "Complete", Cmd::NextField),
                    CommandEntry::with("Esc", "Cancel", Cmd::GoBack),
                ];
            }
            Some(Prompt::React(_)) => {
                let mut entries: Vec<CommandEntry> = (1..=QUICK_REACTIONS.len())
                    .map(|number| {
                        let digit = char::from_digit(number as u32, 10).unwrap_or('1');
                        CommandEntry::with(
                            &digit.to_string(),
                            QUICK_REACTION_LABELS[number - 1],
                            Cmd::Answer(digit),
                        )
                    })
                    .collect();
                entries.push(CommandEntry::with("Esc", "Cancel", Cmd::GoBack));
                return entries;
            }
            Some(Prompt::Delete { .. }) => {
                return vec![
                    CommandEntry::with("Y", "Delete", Cmd::Answer('y')),
                    CommandEntry::with("N", "Cancel", Cmd::Answer('n')),
                ];
            }
            Some(Prompt::Leave(_)) => {
                return vec![
                    CommandEntry::with("Y", "Leave", Cmd::Answer('y')),
                    CommandEntry::with("N", "Cancel", Cmd::Answer('n')),
                ];
            }
            None => {}
        }
        if let Some(entries) = self.encryption_commands() {
            return entries;
        }
        match &self.screen {
            Screen::Profile(ProfileScreen::Opening) => vec![CommandEntry::of(Cmd::Quit)],
            Screen::Profile(ProfileScreen::Keychain(_) | ProfileScreen::Failed(_)) => vec![
                CommandEntry::of(Cmd::TryAgain),
                CommandEntry::of(Cmd::Palette),
                CommandEntry::of(Cmd::Quit),
            ],
            Screen::Profile(ProfileScreen::LostKey(_)) => vec![
                CommandEntry::of(Cmd::StartOver),
                CommandEntry::of(Cmd::Palette),
                CommandEntry::of(Cmd::Quit),
            ],
            Screen::Signin(signin) => self.signin_commands(signin),
            Screen::Rooms(rooms) => self.rooms_commands(rooms),
        }
    }

    /// How many command-bar slots fit, and the commands on the current
    /// page (with `O OTHER CMDS` last when there are more pages).
    pub fn command_bar(&self, width: u16) -> Vec<CommandEntry> {
        let columns = usize::from(width / 13).clamp(2, 8);
        let slots = columns * 2;
        let all = self.commands();
        if all.len() <= slots {
            return all;
        }
        let per_page = slots - 1;
        let pages = all.len().div_ceil(per_page);
        let page = self.command_page % pages;
        let mut shown: Vec<CommandEntry> = all
            .into_iter()
            .skip(page * per_page)
            .take(per_page)
            .collect();
        shown.push(CommandEntry::of(Cmd::OtherCmds));
        shown
    }

    /// Runs a command on the current screen.
    pub fn run(&mut self, cmd: Cmd) {
        if self.prompt.is_some() {
            let key = match cmd {
                Cmd::Answer(ch) => KeyCode::Char(ch),
                Cmd::Select => KeyCode::Enter,
                Cmd::NextField => KeyCode::Tab,
                _ => KeyCode::Esc,
            };
            self.prompt_key(KeyEvent::new(key, KeyModifiers::NONE));
            return;
        }
        match cmd {
            Cmd::Quit => self.quit = true,
            Cmd::Palette => self.open_palette(),
            Cmd::OtherCmds => self.command_page += 1,
            Cmd::Copy => self.copy_selection(),
            Cmd::Bell => {
                self.settings.bell = !self.settings.bell;
                self.status = Some(
                    if self.settings.bell {
                        "The bell rings for new messages"
                    } else {
                        "The bell is off"
                    }
                    .to_owned(),
                );
            }
            Cmd::TryAgain if matches!(self.screen, Screen::Profile(_)) => self.try_again(),
            _ if self.encryption_has_keys() && self.encryption_run(cmd) => {}
            Cmd::StartOver => self.prompt = Some(Prompt::StartOver),
            _ => match &self.screen {
                Screen::Signin(_) => self.signin_run(cmd),
                Screen::Rooms(_) => self.rooms_run(cmd),
                Screen::Profile(_) => {}
            },
        }
    }

    /// Try again on a profile screen.
    fn try_again(&mut self) {
        if matches!(
            self.screen,
            Screen::Profile(ProfileScreen::Keychain(_) | ProfileScreen::Failed(_))
        ) {
            self.screen = Screen::Profile(ProfileScreen::Opening);
            self.opening = Some(self.startup.open());
        }
    }

    fn prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let answer = match key.code {
            KeyCode::Char(ch) if !control => Some(ch.to_ascii_lowercase()),
            _ => None,
        };
        let cancel =
            key.code == KeyCode::Esc || (control && matches!(key.code, KeyCode::Char('c' | 'g')));
        match prompt {
            Prompt::Palette(palette) => self.palette_key(palette, key),
            Prompt::Link(prompt) => match answer {
                Some('y') => {
                    self.effects.push(Effect::Open(prompt.target.clone()));
                    self.status = Some(format!("Opening {}", sanitize_line(&prompt.target)));
                }
                Some('c') => {
                    self.effects.push(Effect::Copy(prompt.target.clone()));
                    self.status = Some("Copied the link's address".to_owned());
                }
                Some('n') => {}
                _ if cancel => {}
                _ => self.prompt = Some(Prompt::Link(prompt)),
            },
            Prompt::RefNumber(mut input) => match key.code {
                KeyCode::Enter => {
                    if let Ok(number) = input.parse::<usize>() {
                        self.open_reference(number.saturating_sub(1));
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.prompt = Some(Prompt::RefNumber(input));
                }
                KeyCode::Char(ch) if ch.is_ascii_digit() && input.len() < 6 => {
                    input.push(ch);
                    self.prompt = Some(Prompt::RefNumber(input));
                }
                _ if cancel => {}
                _ => self.prompt = Some(Prompt::RefNumber(input)),
            },
            Prompt::Save { media, mut input } => match key.code {
                KeyCode::Enter => self.save_to(media, &input),
                KeyCode::Backspace => {
                    input.pop();
                    self.prompt = Some(Prompt::Save { media, input });
                }
                KeyCode::Char('u') if control => {
                    input.clear();
                    self.prompt = Some(Prompt::Save { media, input });
                }
                KeyCode::Char(ch) if !control && !ch.is_control() => {
                    input.push(ch);
                    self.prompt = Some(Prompt::Save { media, input });
                }
                _ if cancel => {}
                _ => self.prompt = Some(Prompt::Save { media, input }),
            },
            Prompt::Replace { media, path } => match answer {
                Some('y') => self.start_save(media, path),
                Some('n') => {
                    let input = path.display().to_string();
                    self.prompt = Some(Prompt::Save { media, input });
                }
                _ if cancel => {}
                _ => self.prompt = Some(Prompt::Replace { media, path }),
            },
            Prompt::Attach { mut input, choices } => match key.code {
                KeyCode::Enter => {
                    if !input.trim().is_empty() {
                        self.attach_path(&input);
                    }
                }
                KeyCode::Tab => {
                    let (completed, choices) = App::complete_attach(&input);
                    self.prompt = Some(Prompt::Attach {
                        input: completed,
                        choices,
                    });
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.prompt = Some(Prompt::Attach {
                        input,
                        choices: Vec::new(),
                    });
                }
                KeyCode::Char('u') if control => {
                    input.clear();
                    self.prompt = Some(Prompt::Attach {
                        input,
                        choices: Vec::new(),
                    });
                }
                KeyCode::Char(ch) if !control && !ch.is_control() => {
                    input.push(ch);
                    self.prompt = Some(Prompt::Attach {
                        input,
                        choices: Vec::new(),
                    });
                }
                _ if cancel => {}
                _ => self.prompt = Some(Prompt::Attach { input, choices }),
            },
            Prompt::React(target) => match answer.and_then(|ch| ch.to_digit(10)) {
                Some(number @ 1..=6) => {
                    let (key, _) = QUICK_REACTIONS[number as usize - 1];
                    self.react_with(target, key);
                }
                _ if cancel => {}
                _ => self.prompt = Some(Prompt::React(target)),
            },
            Prompt::Delete { target, preview } => match answer {
                Some('y') => self.redact(target),
                Some('n') => {}
                _ if cancel => {}
                _ => self.prompt = Some(Prompt::Delete { target, preview }),
            },
            Prompt::Leave(room) => match answer {
                Some('y') => self.leave(room),
                Some('n') => {}
                _ if cancel => {}
                _ => self.prompt = Some(Prompt::Leave(room)),
            },
            Prompt::StartOver => match answer {
                Some('y') => {
                    if matches!(self.screen, Screen::Profile(ProfileScreen::LostKey(_))) {
                        self.screen = Screen::Profile(ProfileScreen::Opening);
                        self.opening = Some(self.startup.start_over());
                    }
                }
                Some('n') => {}
                _ if cancel => {}
                _ => self.prompt = Some(Prompt::StartOver),
            },
        }
    }

    /// The question or field on the status line, as text.
    pub(crate) fn prompt_text(&self) -> Option<(String, bool)> {
        Some(match self.prompt.as_ref()? {
            Prompt::Palette(palette) => (format!(": {}", palette.query), true),
            Prompt::Link(prompt) => (
                format!(
                    "{} (y {}, c copy, n cancel)",
                    prompt.title(),
                    if prompt.is_mailto() { "write" } else { "open" }
                ),
                false,
            ),
            Prompt::RefNumber(input) => (format!("Link or file number: {input}"), true),
            Prompt::Save { media, input } => (
                format!("Save {} to: {input}", sanitize_line(&media.filename)),
                true,
            ),
            Prompt::Replace { path, .. } => (
                format!(
                    "{} is already there. Replace it? (y/n)",
                    sanitize_line(
                        &path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    )
                ),
                false,
            ),
            Prompt::StartOver => (
                "Start over? The data that can't be read moves to a folder beside the new one. \
                 (y/n)"
                    .to_owned(),
                false,
            ),
            Prompt::Attach { input, .. } => (format!("Attach a file: {input}"), true),
            Prompt::React(_) => {
                let choices: Vec<String> = QUICK_REACTIONS
                    .iter()
                    .enumerate()
                    .map(|(index, (emoji, name))| {
                        if self.env.unicode && !self.settings.ascii {
                            format!("{} {emoji}", index + 1)
                        } else {
                            format!("{} {name}", index + 1)
                        }
                    })
                    .collect();
                (
                    format!("React with: {}  (Esc cancels)", choices.join("  ")),
                    false,
                )
            }
            Prompt::Delete { .. } | Prompt::Leave(_) => return None,
        })
    }

    // ------------------------------------------------------------------
    // Copy
    // ------------------------------------------------------------------

    pub(crate) fn copy_selection(&mut self) {
        let Some(selection) = self.selection else {
            self.status = Some("Select text first: drag over it with the mouse".to_owned());
            return;
        };
        let Some(pane) = self.pane(selection.pane) else {
            return;
        };
        let (start, end) = selection.ordered();
        let text = selected_text(&pane.lines, start, end);
        if text.is_empty() {
            return;
        }
        let count = text.chars().count();
        self.status = Some(format!(
            "Copied {count} {}",
            if count == 1 {
                "character"
            } else {
                "characters"
            }
        ));
        self.effects.push(Effect::Copy(text));
    }

    // ------------------------------------------------------------------
    // The mouse
    // ------------------------------------------------------------------

    pub(crate) fn pane(&self, id: PaneId) -> Option<&DrawnPane> {
        self.panes.iter().find(|pane| pane.id == id)
    }

    fn on_mouse(&mut self, mouse: MouseEvent) {
        // Shift leaves the mouse to the terminal's own selection; most
        // terminals don't even report it.
        if mouse.modifiers.contains(KeyModifiers::SHIFT) {
            return;
        }
        if self.prompt.is_some() {
            if self.palette_mouse(mouse) {
                return;
            }
            if let MouseEventKind::Down(MouseButton::Left) = mouse.kind
                && let Some(cmd) = self.command_at(mouse.column, mouse.row)
            {
                self.run(cmd);
            }
            return;
        }
        let (x, y) = (mouse.column, mouse.row);
        self.mouse.position = (x, y);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(x, y),
            MouseEventKind::Drag(MouseButton::Left) => self.mouse_drag(x, y),
            MouseEventKind::Up(MouseButton::Left) => self.mouse_up(x, y),
            MouseEventKind::ScrollUp => self.wheel(x, y, -1),
            MouseEventKind::ScrollDown => self.wheel(x, y, 1),
            _ => {}
        }
    }

    pub(crate) fn command_at(&self, x: u16, y: u16) -> Option<Cmd> {
        self.command_targets
            .iter()
            .find(|(area, _)| area.contains(ratatui::layout::Position { x, y }))
            .map(|(_, cmd)| *cmd)
    }

    fn mouse_down(&mut self, x: u16, y: u16) {
        if let Some(cmd) = self.command_at(x, y) {
            self.status = None;
            self.run(cmd);
            return;
        }
        let Some(pane) = self.panes.iter().find(|pane| pane.contains(x, y)) else {
            return;
        };
        let (id, point) = (pane.id, pane.point_at(x, y));
        let now = Instant::now();
        let clicks = match self.mouse.last_press {
            Some((at, last_x, last_y, clicks))
                if now.duration_since(at) < MULTI_CLICK && (last_x, last_y) == (x, y) =>
            {
                clicks % 3 + 1
            }
            _ => 1,
        };
        self.mouse.last_press = Some((now, x, y, clicks));
        self.mouse.press = Some((id, point, clicks));
        self.mouse.dragged = false;
        self.selection = match clicks {
            2 => self.word_selection(id, point),
            3 => self.line_selection(id, point),
            _ => None,
        };
    }

    fn word_selection(&self, id: PaneId, point: Point) -> Option<Selection> {
        let text = self.pane(id)?.lines.get(point.line)?.text();
        let (start, end) = word_at(&text, point.column)?;
        Some(Selection {
            pane: id,
            anchor: Point {
                line: point.line,
                column: start,
            },
            head: Point {
                line: point.line,
                column: end,
            },
        })
    }

    fn line_selection(&self, id: PaneId, point: Point) -> Option<Selection> {
        let pane = self.pane(id)?;
        let (first, last) = logical_line(&pane.lines, point.line);
        Some(Selection {
            pane: id,
            anchor: Point {
                line: first,
                column: 0,
            },
            head: Point {
                line: last,
                column: usize::MAX / 2,
            },
        })
    }

    fn mouse_drag(&mut self, x: u16, y: u16) {
        let Some((id, anchor, _)) = self.mouse.press else {
            return;
        };
        let Some(pane) = self.pane(id) else {
            return;
        };
        let top = pane.area.y;
        let bottom = pane.area.y + pane.area.height;
        if y < top {
            self.scroll_pane(id, -1);
        } else if y >= bottom {
            self.scroll_pane(id, 1);
        }
        self.extend_drag(id, anchor, x, y);
    }

    fn extend_drag(&mut self, id: PaneId, anchor: Point, x: u16, y: u16) {
        let Some(pane) = self.pane(id) else {
            return;
        };
        let scroll = self.scroll.get(&id).copied().unwrap_or(pane.scroll);
        let shown = DrawnPane {
            scroll,
            ..pane.clone()
        };
        let head = shown.point_at(x, y);
        self.mouse.dragged = true;
        self.selection = Some(Selection {
            pane: id,
            anchor,
            head,
        });
    }

    fn drag_past_edge(&mut self) {
        let Some((id, anchor, _)) = self.mouse.press else {
            return;
        };
        if !self.mouse.dragged {
            return;
        }
        let (x, y) = self.mouse.position;
        let Some(pane) = self.pane(id) else {
            return;
        };
        if y < pane.area.y {
            self.scroll_pane(id, -1);
        } else if y >= pane.area.y + pane.area.height {
            self.scroll_pane(id, 1);
        } else {
            return;
        }
        self.extend_drag(id, anchor, x, y);
    }

    fn mouse_up(&mut self, x: u16, y: u16) {
        let Some((id, point, clicks)) = self.mouse.press.take() else {
            return;
        };
        if self.mouse.dragged || clicks > 1 {
            // Copy on select, as Zellij's `copy_on_select`.
            self.copy_selection();
            return;
        }
        if self.pane(id).is_none_or(|pane| !pane.contains(x, y)) {
            return;
        }
        self.selection = None;
        self.click(id, point);
    }

    /// A single click on `point` of pane `id`.
    fn click(&mut self, id: PaneId, point: Point) {
        let Some(line) = self.pane(id).and_then(|pane| pane.lines.get(point.line)) else {
            return;
        };
        let target = line.target;
        let reference = line
            .links
            .iter()
            .find(|(cells, _)| cells.contains(&point.column))
            .map(|(_, reference)| *reference);
        match id {
            PaneId::Signin => {
                if let Some(target) = target {
                    self.signin_click(target);
                }
            }
            PaneId::Rooms => {
                if let Some(target) = target {
                    self.rooms_click(target);
                }
            }
            PaneId::Timeline => {
                if let Some(reference) = reference {
                    self.open_reference(reference);
                }
            }
            PaneId::Profile | PaneId::Members | PaneId::Dialog | PaneId::Palette => {}
        }
    }

    pub(crate) fn scroll_pane(&mut self, id: PaneId, by: isize) {
        if id == PaneId::Timeline {
            self.scroll_timeline(-by);
            return;
        }
        let scroll = self.scroll.entry(id).or_default();
        *scroll = scroll.saturating_add_signed(by);
    }

    fn wheel(&mut self, x: u16, y: u16, direction: isize) {
        let Some(id) = self
            .panes
            .iter()
            .find(|pane| pane.contains(x, y))
            .map(|pane| pane.id)
        else {
            return;
        };
        self.scroll_pane(id, direction * WHEEL_LINES as isize);
    }
}

#[cfg(test)]
mod tests {
    use super::Environment;

    #[test]
    fn the_environment_reads_without_panicking() {
        let env = Environment::detect();
        assert_eq!(env.macos, cfg!(target_os = "macos"));
    }
}
