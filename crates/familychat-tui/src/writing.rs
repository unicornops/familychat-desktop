//! Writing in the terminal (ADR 0008, "Writing"; ADR 0012; PLAN.md
//! decision 10): the composer's keys, sending, the message actions
//! (reply, edit, react, delete, send anyway), files by path, answering
//! invitations, leaving, identity warnings, read receipts, typing notices
//! and drafts.
//!
//! Every change to a room goes through the account's
//! [`familychat_actions::Actions`]; its tasks run off the UI thread
//! ([`crate::tasks`]) and what they report arrives on its event channel,
//! polled on each tick. Nothing here builds a `RoomRequest`.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use familychat_actions::{Action, ActionEvent, ComposerState, FileSource, SendTarget};
use familychat_core::{
    messaging::{Draft, MessageRef},
    rooms::RoomSummary,
    timeline::{
        MessageKind, SendProblem, SendState, TimelineContent, TimelineEvent, TimelineItemKind,
    },
};
use familychat_security::terminal::sanitize_line;

use crate::{
    app::{App, Effect, Prompt},
    commands::Cmd,
    composer::{self, KeyOutcome, Target},
    rooms::{Focus, OpenRoom, RoomsState},
};

/// The reactions the quick picker offers, the window's six, with the
/// names an ASCII terminal shows instead.
pub(crate) const QUICK_REACTIONS: [(&str, &str); 6] = [
    ("👍", "thumbs up"),
    ("❤️", "heart"),
    ("😂", "laughing"),
    ("😮", "surprised"),
    ("😢", "sad"),
    ("🎉", "party"),
];

/// How often the read receipt moves at most.
const READ_EVERY: Duration = Duration::from_secs(2);

/// The most file names the attach question lists under its field.
const CHOICES_SHOWN: usize = 6;

/// A message the room's commands can be about.
#[derive(Debug, Clone)]
pub(crate) struct MessageChoice {
    pub(crate) target: MessageRef,
    pub(crate) sender: String,
    pub(crate) text: String,
    pub(crate) own: bool,
    pub(crate) editable: bool,
    pub(crate) problem: Option<SendProblem>,
}

/// Whether an event is a message the commands act on, and its choice.
fn choice_of(item: &familychat_core::timeline::TimelineItem) -> Option<MessageChoice> {
    let TimelineItemKind::Event(event) = &item.kind else {
        return None;
    };
    let text = match &event.content {
        TimelineContent::Message { body, .. } => body.as_str().to_owned(),
        TimelineContent::Sticker { body } => body.as_str().to_owned(),
        _ => return None,
    };
    let editable = matches!(
        &event.content,
        TimelineContent::Message {
            kind: MessageKind::Text | MessageKind::Emote | MessageKind::Notice,
            ..
        }
    ) && event.is_own
        && event.event_id.is_some();
    Some(MessageChoice {
        target: MessageRef {
            item: item.id.clone(),
            event_id: event.event_id.clone(),
        },
        sender: sender_of(event),
        text,
        own: event.is_own,
        editable,
        problem: match event.send_state {
            Some(SendState::Failed { problem, .. }) => problem,
            _ => None,
        },
    })
}

fn sender_of(event: &TimelineEvent) -> String {
    sanitize_line(&event.sender_label())
}

/// The messages of the open room the commands can be about, oldest first.
pub(crate) fn actionable(open: &OpenRoom) -> Vec<MessageChoice> {
    open.items.iter().filter_map(choice_of).collect()
}

/// The selected message if `accept` takes it, or (with none selected) the
/// newest one it takes.
fn chosen(open: &OpenRoom, accept: impl Fn(&MessageChoice) -> bool) -> Option<MessageChoice> {
    let messages = actionable(open);
    match &open.selected_item {
        Some(selected) => messages
            .into_iter()
            .find(|choice| &choice.target.item == selected && accept(choice)),
        None => messages.into_iter().rev().find(|choice| accept(choice)),
    }
}

/// `~/x` and `~` as the home folder.
pub(crate) fn expand_home(input: &str) -> PathBuf {
    let input = input.trim();
    if let Some(home) = std::env::home_dir() {
        if input == "~" {
            return home;
        }
        if let Some(rest) = input
            .strip_prefix("~/")
            .or_else(|| input.strip_prefix("~\\"))
        {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

/// Tab in the attach question: the field completed as far as the files
/// that match agree, and the names that match (folders end in the
/// separator). Reads one folder's names, nothing more.
pub(crate) fn complete_path(input: &str) -> (String, Vec<String>) {
    let separator = std::path::MAIN_SEPARATOR;
    let ends_in_separator = input.ends_with('/') || input.ends_with(separator);
    let (folder_text, prefix) = if ends_in_separator || input.is_empty() {
        (input.to_owned(), String::new())
    } else {
        match input.rfind(['/', separator]) {
            Some(at) => (input[..=at].to_owned(), input[at + 1..].to_owned()),
            None => (String::new(), input.to_owned()),
        }
    };
    let folder = if folder_text.is_empty() {
        PathBuf::from(".")
    } else {
        expand_home(&folder_text)
    };
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return (input.to_owned(), Vec::new());
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
                return None;
            }
            let dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            Some(if dir {
                format!("{name}{separator}")
            } else {
                name
            })
        })
        .collect();
    names.sort();
    let Some(first) = names.first() else {
        return (input.to_owned(), Vec::new());
    };
    let mut common = first.clone();
    for name in &names[1..] {
        let shared = common
            .chars()
            .zip(name.chars())
            .take_while(|(a, b)| a == b)
            .count();
        common = common.chars().take(shared).collect();
    }
    let completed = format!("{folder_text}{common}");
    let shown = if names.len() > 1 {
        names.into_iter().map(|name| sanitize_line(&name)).collect()
    } else {
        Vec::new()
    };
    (completed, shown)
}

fn room_mut(rooms: &mut RoomsState) -> Option<&mut OpenRoom> {
    rooms.open.as_mut()
}

impl App {
    /// The open room's composer state: writable, paused or not joined.
    pub(crate) fn composer_state(rooms: &RoomsState) -> Option<ComposerState> {
        let open = rooms.open.as_ref()?;
        Some(ComposerState::of(&rooms.status, &open.room))
    }

    /// Polls what the actions reported and keeps the composer's timers;
    /// `true` when anything changed.
    pub(crate) fn writing_tick(&mut self) -> bool {
        let tasks = self.tasks.clone();
        let mut status = None;
        let Some(rooms) = self.rooms_state() else {
            return false;
        };
        let mut changed = false;
        let mut events = Vec::new();
        if let Some(receiver) = rooms.action_events.as_mut() {
            while let Ok(event) = receiver.try_recv() {
                events.push(event);
            }
        }
        let mut joined_now = false;
        for event in events {
            changed = true;
            joined_now |= matches!(
                event,
                ActionEvent::Done {
                    action: Action::AcceptInvite,
                    ..
                }
            );
            status = action_event(rooms, event).or(status);
        }
        let state = App::composer_state(rooms);
        let now = Instant::now();
        let actions = rooms.actions.clone();
        let focus = rooms.focus;
        if let Some(open) = rooms.open.as_mut() {
            if let Some((user, reply)) = open.acknowledging.as_mut() {
                match reply.try_recv() {
                    Ok(Ok(())) => {
                        let user = user.clone();
                        open.warnings.retain(|warning| warning.user_id != user);
                        open.acknowledging = None;
                        changed = true;
                    }
                    Ok(Err(error)) => {
                        status = Some(sanitize_line(&error.to_string()));
                        open.acknowledging = None;
                        changed = true;
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        open.acknowledging = None;
                        changed = true;
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                }
            }
            if let (Some(composer), Some(actions)) = (open.composer.as_mut(), actions.as_ref()) {
                if let Some(typing) = composer.idle(now) {
                    tasks.run(actions.typing(&composer.room, typing));
                }
                if let Some(draft) = composer.due_draft(now) {
                    tasks.run(actions.save_draft(&composer.room, draft));
                }
            }
            // Read to the newest message: the receipt and the marker move
            // there, at most every two seconds.
            let reading = focus != Focus::List
                && open.loaded
                && open.from_bottom == 0
                && open.room.unread.is_unread();
            if reading
                && rooms
                    .last_read
                    .is_none_or(|at| now.duration_since(at) >= READ_EVERY)
                && let Some(actions) = actions.as_ref()
            {
                rooms.last_read = Some(now);
                tasks.run(actions.mark_read(&open.room.room_id));
            }
        }
        // Paused (or no longer in the room): the composer is read-only, so
        // the room's commands have the keys.
        if rooms.focus == Focus::Composer && state != Some(ComposerState::Writable) {
            rooms.focus = Focus::Room;
            changed = true;
        }
        // An invitation accepted: the room is a joined one now. The list
        // shows it under All (not Invites), and it opens again with a
        // timeline and a composer.
        let accepted = rooms
            .open
            .as_ref()
            .is_some_and(|open| open.composer.is_none())
            && joined_now;
        if accepted
            && rooms.filter == familychat_core::rooms::RoomListFilter::Invites
            && rooms.controls_set_filter(familychat_core::rooms::RoomListFilter::All)
        {
            changed = true;
        }
        let reopen = rooms.open.as_ref().and_then(|open| {
            if open.composer.is_some() {
                return None;
            }
            rooms
                .rooms
                .iter()
                .position(|room| room.room_id == open.room.room_id && !room.is_invite())
        });
        if let Some(index) = reopen {
            let focused = rooms.focus != Focus::List;
            rooms.open = None;
            self.open_room(index, focused);
            changed = true;
        }
        if let Some(status) = status {
            self.status = Some(status);
        }
        changed
    }

    /// A key while the composer has the keys; `false` when it isn't the
    /// composer's (the app runs it as a command).
    pub(crate) fn composer_key(&mut self, key: KeyEvent) -> bool {
        if composer::is_editor_key(&key) {
            self.external_edit();
            return true;
        }
        let tasks = self.tasks.clone();
        let Some(rooms) = self.rooms_state() else {
            return false;
        };
        let actions = rooms.actions.clone();
        let Some(open) = room_mut(rooms) else {
            return false;
        };
        let Some(composer) = open.composer.as_mut() else {
            return false;
        };
        // Backspace in an empty field takes back the last file waiting.
        if key.code == KeyCode::Backspace
            && composer.text().is_empty()
            && composer.files.pop().is_some()
        {
            return true;
        }
        match composer.key(key) {
            KeyOutcome::Edited(true) => {
                if let Some(typing) = composer.changed(Instant::now())
                    && let Some(actions) = actions
                {
                    tasks.run(actions.typing(&composer.room, typing));
                }
                open.from_bottom = 0;
                true
            }
            KeyOutcome::Edited(false) => true,
            KeyOutcome::Send => {
                self.send_message();
                true
            }
            KeyOutcome::Leave => {
                if let Some(rooms) = self.rooms_state() {
                    rooms.focus = Focus::Room;
                }
                true
            }
            KeyOutcome::EditLatest => {
                self.edit_message();
                true
            }
            KeyOutcome::Ignored => match key.code {
                KeyCode::PageUp | KeyCode::PageDown => {
                    let page = self
                        .rooms_ref()
                        .map_or(1, |rooms| rooms.timeline_height.saturating_sub(2).max(1))
                        as isize;
                    self.scroll_timeline(if key.code == KeyCode::PageUp {
                        page
                    } else {
                        -page
                    });
                    true
                }
                KeyCode::Tab | KeyCode::BackTab => true,
                _ => key.modifiers.contains(KeyModifiers::ALT),
            },
        }
    }

    /// A paste while the composer has the keys.
    pub(crate) fn composer_paste(&mut self, pasted: &str) -> bool {
        let tasks = self.tasks.clone();
        let Some(rooms) = self.rooms_state() else {
            return false;
        };
        if rooms.focus != Focus::Composer {
            return false;
        }
        let actions = rooms.actions.clone();
        let Some(composer) = rooms.open.as_mut().and_then(|open| open.composer.as_mut()) else {
            return false;
        };
        composer.paste(pasted);
        if let Some(typing) = composer.changed(Instant::now())
            && let Some(actions) = actions
        {
            tasks.run(actions.typing(&composer.room, typing));
        }
        true
    }

    /// Enter: sends what is written and the files waiting, then clears the
    /// field. Everything goes through the actions.
    pub(crate) fn send_message(&mut self) {
        let tasks = self.tasks.clone();
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        if App::composer_state(rooms) != Some(ComposerState::Writable) {
            return;
        }
        let Some(actions) = rooms.actions.clone() else {
            return;
        };
        let Some(open) = rooms.open.as_mut() else {
            return;
        };
        let Some(composer) = open.composer.as_mut() else {
            return;
        };
        let draft = composer.draft();
        let has_text = !draft.text.as_str().trim().is_empty();
        if !has_text && composer.files.is_empty() {
            return;
        }
        let room = composer.room.clone();
        let mut reply_to = draft.reply_to.clone();
        match &composer.target {
            Target::Edit { target, .. } => {
                if has_text {
                    tasks.run(actions.send(&room, &draft, SendTarget::Edit(target.clone())));
                }
            }
            _ => {
                for file in std::mem::take(&mut composer.files) {
                    tasks.run(actions.send_file(&room, file.source, reply_to.take()));
                }
                if has_text {
                    let target = match reply_to {
                        Some(event_id) => SendTarget::Reply(event_id),
                        None => SendTarget::New,
                    };
                    tasks.run(actions.send(&room, &draft, target));
                }
            }
        }
        let loaded = composer.draft_loaded;
        if let Some(typing) = composer.sent() {
            tasks.run(actions.typing(&room, typing));
        }
        // The kept draft is spent.
        if loaded {
            tasks.run(actions.save_draft(&room, Draft::default()));
        }
        open.from_bottom = 0;
        open.selected_item = None;
        open.generation += 1;
    }

    /// `[` and `]`: the previous or next message, which the room's commands
    /// are then about.
    fn select_message(&mut self, step: isize) {
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        rooms.focus = Focus::Room;
        let Some(open) = rooms.open.as_mut() else {
            return;
        };
        let messages = actionable(open);
        if messages.is_empty() {
            self.status = Some("There are no messages here yet.".to_owned());
            return;
        }
        let last = messages.len() as isize - 1;
        let current = open.selected_item.as_ref().and_then(|selected| {
            messages
                .iter()
                .position(|choice| &choice.target.item == selected)
        });
        let next = match current {
            Some(index) => (index as isize + step).clamp(0, last),
            None if step < 0 => last,
            None => return,
        };
        let choice = &messages[usize::try_from(next).unwrap_or_default()];
        open.selected_item = Some(choice.target.item.clone());
        open.generation += 1;
        // Brought into view once laid out again.
        open.reveal_selected = true;
        let what = if choice.own {
            "your message"
        } else {
            "a message"
        };
        self.status = Some(format!(
            "Selected {what} from {}: R replies, + reacts{}",
            choice.sender,
            if choice.own {
                ", E edits, D deletes"
            } else {
                ""
            }
        ));
    }

    fn open_choice(&self, accept: impl Fn(&MessageChoice) -> bool) -> Option<MessageChoice> {
        let open = self.rooms_ref()?.open.as_ref()?;
        chosen(open, accept)
    }

    fn can_write(&mut self) -> bool {
        let state = self.rooms_ref().and_then(App::composer_state);
        match state {
            Some(ComposerState::Writable) => true,
            Some(ComposerState::Paused) => {
                self.status = Some(familychat_actions::Refusal::Paused.to_string());
                false
            }
            Some(ComposerState::NotJoined) => {
                self.status = Some(familychat_actions::Refusal::NotJoined.to_string());
                false
            }
            None => false,
        }
    }

    /// `R`: a reply to the selected (or newest) message, in the composer.
    fn reply_to_message(&mut self) {
        if !self.can_write() {
            return;
        }
        let Some(choice) = self.open_choice(|_| true) else {
            self.status = Some("There's no message to reply to.".to_owned());
            return;
        };
        let Some(event_id) = choice.target.event_id.clone() else {
            self.status = Some("That message hasn't reached the server yet.".to_owned());
            return;
        };
        if let Some(rooms) = self.rooms_state() {
            rooms.focus = Focus::Composer;
            if let Some(composer) = rooms.open.as_mut().and_then(|open| open.composer.as_mut()) {
                composer.reply_to(event_id, &choice.sender, &choice.text);
            }
        }
    }

    /// `E` (or Up in an empty composer): edit the selected (or newest) of
    /// the account's messages.
    fn edit_message(&mut self) {
        if !self.can_write() {
            return;
        }
        let Some(choice) = self.open_choice(|choice| choice.editable) else {
            self.status = Some("Pick one of your messages to edit.".to_owned());
            return;
        };
        if let Some(rooms) = self.rooms_state() {
            rooms.focus = Focus::Composer;
            if let Some(open) = rooms.open.as_mut() {
                open.selected_item = None;
                open.generation += 1;
                if let Some(composer) = open.composer.as_mut() {
                    composer.edit(choice.target, &choice.text);
                }
            }
        }
    }

    /// `+`: the quick reaction picker for the selected (or newest) message.
    fn react_to_message(&mut self) {
        if !self.can_write() {
            return;
        }
        match self.open_choice(|choice| choice.target.event_id.is_some()) {
            Some(choice) => self.prompt = Some(Prompt::React(choice.target)),
            None => self.status = Some("There's no message to react to.".to_owned()),
        }
    }

    /// `D`: delete the selected message, one of the account's, after
    /// asking.
    fn delete_message(&mut self) {
        if !self.can_write() {
            return;
        }
        let selected = self
            .rooms_ref()
            .and_then(|rooms| rooms.open.as_ref())
            .is_some_and(|open| open.selected_item.is_some());
        let choice = self.open_choice(|choice| choice.own).filter(|_| selected);
        match choice {
            Some(choice) => {
                self.prompt = Some(Prompt::Delete {
                    target: choice.target,
                    preview: composer::one_line(&choice.text),
                });
            }
            None => {
                self.status = Some(
                    "Select one of your messages to delete it: [ and ] move between them."
                        .to_owned(),
                );
            }
        }
    }

    /// `S`: the selected message the encryption stopped goes anyway.
    fn send_anyway(&mut self) {
        let tasks = self.tasks.clone();
        let choice = self.open_choice(|choice| {
            choice.own && choice.problem.is_some_and(SendProblem::can_send_anyway)
        });
        let selected = self
            .rooms_ref()
            .and_then(|rooms| rooms.open.as_ref())
            .is_some_and(|open| open.selected_item.is_some());
        let (Some(choice), true) = (choice, selected) else {
            self.status = Some("Select a message that wasn't sent first.".to_owned());
            return;
        };
        let Some(rooms) = self.rooms_ref() else {
            return;
        };
        if let (Some(actions), Some(open)) = (&rooms.actions, &rooms.open) {
            tasks.run(actions.send_anyway(&open.room.room_id, choice.target));
            self.status = Some("Sending it anyway…".to_owned());
        }
    }

    /// `w`: the room's first identity warning answered (OK, or the old
    /// verification withdrawn), through the session.
    fn acknowledge_warning(&mut self) {
        let session = self.opened().map(|opened| opened.session.clone());
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        let account = rooms.account.key.clone();
        let Some(open) = rooms.open.as_mut() else {
            return;
        };
        let (Some(warning), Some(session)) = (open.warnings.first().cloned(), session) else {
            self.status = Some("There's no identity warning here.".to_owned());
            return;
        };
        let reply = session.acknowledge_identity(&account, &warning.user_id);
        open.acknowledging = Some((warning.user_id, reply));
    }

    /// `A`: asks for the path of a file to send with the next message.
    fn attach_file(&mut self) {
        if !self.can_write() {
            return;
        }
        let can = self
            .rooms_ref()
            .and_then(|rooms| rooms.open.as_ref())
            .and_then(|open| open.composer.as_ref())
            .is_some_and(composer::Composer::can_attach);
        if !can {
            self.status = Some("Files can't be added to an edit.".to_owned());
            return;
        }
        let start = self.downloads.display().to_string();
        let start = if start.ends_with(std::path::MAIN_SEPARATOR) {
            start
        } else {
            format!("{start}{}", std::path::MAIN_SEPARATOR)
        };
        self.prompt = Some(Prompt::Attach {
            input: start,
            choices: Vec::new(),
        });
    }

    /// The attach question's answer: the file is checked against the
    /// server's upload limit (its size only: nothing is read) and then
    /// waits in the composer.
    pub(crate) fn attach_path(&mut self, input: &str) {
        let path = expand_home(input);
        if !path.is_file() {
            self.status = Some(format!(
                "There's no file at {}.",
                sanitize_line(&path.display().to_string())
            ));
            self.prompt = Some(Prompt::Attach {
                input: input.to_owned(),
                choices: Vec::new(),
            });
            return;
        }
        self.add_file(FileSource::Path(path));
    }

    /// Checks `source` against the upload limit, through the actions.
    pub(crate) fn add_file(&mut self, source: FileSource) {
        let tasks = self.tasks.clone();
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        let Some(actions) = rooms.actions.clone() else {
            return;
        };
        rooms.focus = Focus::Composer;
        if let Some(composer) = rooms.open.as_mut().and_then(|open| open.composer.as_mut()) {
            let check = tasks.spawn(actions.file_allowed(source.clone()));
            composer.checks.push((source, check));
        }
    }

    /// Tab in the attach question.
    pub(crate) fn complete_attach(input: &str) -> (String, Vec<String>) {
        let (completed, mut choices) = complete_path(input);
        choices.truncate(CHOICES_SHOWN);
        (completed, choices)
    }

    /// A reaction from the picker, toggled through the actions.
    pub(crate) fn react_with(&mut self, target: MessageRef, key: &str) {
        let tasks = self.tasks.clone();
        let Some(rooms) = self.rooms_ref() else {
            return;
        };
        if let (Some(actions), Some(open)) = (&rooms.actions, &rooms.open) {
            tasks.run(actions.toggle_reaction(&open.room.room_id, target, key));
        }
    }

    /// The delete question answered yes.
    pub(crate) fn redact(&mut self, target: MessageRef) {
        let tasks = self.tasks.clone();
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        if let (Some(actions), Some(open)) = (&rooms.actions, rooms.open.as_mut()) {
            tasks.run(actions.redact(&open.room.room_id, target, None));
            open.selected_item = None;
            open.generation += 1;
        }
    }

    /// The leave question answered yes.
    pub(crate) fn leave(&mut self, room: RoomSummary) {
        let tasks = self.tasks.clone();
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        if let Some(actions) = &rooms.actions {
            tasks.run(actions.leave(&room.room_id));
            rooms.focus = Focus::List;
            self.status = Some(format!("Leaving {}…", sanitize_line(&room.name)));
        }
    }

    /// `a` and `x` on an invitation.
    fn answer_invite(&mut self, accept: bool) {
        let tasks = self.tasks.clone();
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        let Some(open) = rooms.open.as_ref().filter(|open| open.room.is_invite()) else {
            self.status = Some("There's no invitation here.".to_owned());
            return;
        };
        if let Some(actions) = &rooms.actions {
            let name = sanitize_line(&open.room.name);
            tasks.run(actions.answer_invite(&open.room.room_id, accept));
            self.status = Some(if accept {
                format!("Joining {name}…")
            } else {
                format!("Declining the invitation to {name}…")
            });
        }
    }

    /// `^_`: hands the message being written to the external editor, when
    /// `--editor` turned it on.
    pub(crate) fn external_edit(&mut self) {
        if !self.settings.external_editor {
            self.status = Some(
                "The external editor is off. Start with familychat tui --editor to use it (the \
                 message is then written unencrypted to a temporary file while the editor is \
                 open)."
                    .to_owned(),
            );
            return;
        }
        let Some(command) = crate::editor::editor_command() else {
            self.status = Some("Set VISUAL or EDITOR to the editor to use.".to_owned());
            return;
        };
        let Some(text) = self
            .rooms_ref()
            .and_then(|rooms| rooms.open.as_ref())
            .and_then(|open| open.composer.as_ref())
            .map(composer::Composer::text)
        else {
            return;
        };
        self.effects.push(Effect::Edit { command, text });
    }

    /// The external editor exited: its text replaces the composer's, or
    /// what went wrong shows on the status line.
    pub fn external_edit_done(&mut self, result: anyhow::Result<String>) {
        match result {
            Ok(text) => {
                let tasks = self.tasks.clone();
                if let Some(rooms) = self.rooms_state() {
                    let actions = rooms.actions.clone();
                    rooms.focus = Focus::Composer;
                    if let Some(composer) =
                        rooms.open.as_mut().and_then(|open| open.composer.as_mut())
                    {
                        composer.replace_text(&text);
                        if let Some(typing) = composer.changed(Instant::now())
                            && let Some(actions) = actions
                        {
                            tasks.run(actions.typing(&composer.room, typing));
                        }
                    }
                }
                self.status = Some(
                    "Text from the editor. Its temporary file was overwritten and removed."
                        .to_owned(),
                );
            }
            Err(error) => self.status = Some(sanitize_line(&format!("{error:#}"))),
        }
    }

    /// The writing commands; `false` when `cmd` isn't one of them.
    pub(crate) fn writing_run(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::FocusComposer => {
                if self.can_write()
                    && let Some(rooms) = self.rooms_state()
                    && rooms
                        .open
                        .as_ref()
                        .is_some_and(|open| open.composer.is_some())
                {
                    rooms.focus = Focus::Composer;
                }
            }
            Cmd::AttachFile => self.attach_file(),
            Cmd::SelectPreviousMessage => self.select_message(-1),
            Cmd::SelectNextMessage => self.select_message(1),
            Cmd::ReplyToMessage => self.reply_to_message(),
            Cmd::EditMessage => self.edit_message(),
            Cmd::ReactToMessage => self.react_to_message(),
            Cmd::DeleteMessage => self.delete_message(),
            Cmd::SendAnyway => self.send_anyway(),
            Cmd::Acknowledge => self.acknowledge_warning(),
            Cmd::Accept => self.answer_invite(true),
            Cmd::Decline => self.answer_invite(false),
            Cmd::Send => self.send_message(),
            Cmd::NewLine => {
                if let Some(composer) = self
                    .rooms_state()
                    .and_then(|rooms| rooms.open.as_mut())
                    .and_then(|open| open.composer.as_mut())
                {
                    composer.editor.insert_newline();
                }
            }
            Cmd::Editor => self.external_edit(),
            Cmd::ToTimeline => {
                if let Some(rooms) = self.rooms_state() {
                    rooms.focus = Focus::Room;
                }
            }
            Cmd::LeaveRoom => {
                let room = self
                    .rooms_ref()
                    .and_then(|rooms| rooms.open.as_ref())
                    .map(|open| open.room.clone());
                match room {
                    Some(room) if !room.is_invite() => self.prompt = Some(Prompt::Leave(room)),
                    Some(_) => self.answer_invite(false),
                    None => self.status = Some("Open a room first.".to_owned()),
                }
            }
            Cmd::ConfirmIdentity => self.confirm_identity(),
            Cmd::SetUpRecovery => self.set_up_recovery(),
            _ => return false,
        }
        true
    }

    /// The selected message's state, for the command bar.
    pub(crate) fn selected_choice(&self) -> Option<MessageChoice> {
        let open = self.rooms_ref()?.open.as_ref()?;
        open.selected_item.as_ref()?;
        chosen(open, |_| true)
    }
}

/// An event from the actions: the status line's sentence, if any, and the
/// composer's problem, the draft, an answered invitation.
fn action_event(rooms: &mut RoomsState, event: ActionEvent) -> Option<String> {
    let message = event.message().map(|message| sanitize_line(&message));
    match event {
        ActionEvent::DraftLoaded { room, draft } => {
            let open = rooms.open.as_mut()?;
            if open.room.room_id != room {
                return None;
            }
            let reply = draft
                .as_ref()
                .and_then(|draft| draft.reply_to.clone())
                .map(|event_id| {
                    let answered = open.items.iter().find_map(|item| match &item.kind {
                        TimelineItemKind::Event(event)
                            if event.event_id.as_ref() == Some(&event_id) =>
                        {
                            Some(event.clone())
                        }
                        _ => None,
                    });
                    Target::Reply {
                        event_id,
                        sender: answered.as_ref().map(sender_of).unwrap_or_default(),
                        text: answered
                            .and_then(|event| match event.content {
                                TimelineContent::Message { body, .. } => {
                                    Some(composer::one_line(body.as_str()))
                                }
                                _ => None,
                            })
                            .unwrap_or_default(),
                    }
                });
            if let Some(composer) = open.composer.as_mut() {
                composer.apply_draft(draft, reply);
            }
            None
        }
        ActionEvent::Refused { room, action, .. } | ActionEvent::Failed { room, action, .. } => {
            if matches!(
                action,
                Action::Send | Action::Reply | Action::Edit | Action::SendFile
            ) && let Some(composer) = rooms
                .open
                .as_mut()
                .filter(|open| Some(&open.room.room_id) == room.as_ref())
                .and_then(|open| open.composer.as_mut())
            {
                composer.error.clone_from(&message);
            }
            message
        }
        ActionEvent::Done {
            action: Action::AcceptInvite,
            ..
        } => Some("You joined the room.".to_owned()),
        ActionEvent::Done {
            action: Action::DeclineInvite,
            ..
        } => Some("Invitation declined.".to_owned()),
        ActionEvent::Done {
            action: Action::Leave,
            ..
        } => Some("You left the room.".to_owned()),
        ActionEvent::Done {
            action: Action::Redact,
            ..
        } => Some("Message deleted.".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn folder(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let folder = std::env::temp_dir().join(format!(
            "familychat-tui-complete-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(folder.join("photos")).expect("the folder");
        std::fs::write(folder.join("holiday-1.jpg"), b"one").expect("a file");
        std::fs::write(folder.join("holiday-2.jpg"), b"two").expect("a file");
        std::fs::write(folder.join(".hidden"), b"no").expect("a file");
        folder
    }

    fn with_separator(path: &Path) -> String {
        format!("{}{}", path.display(), std::path::MAIN_SEPARATOR)
    }

    #[test]
    fn tab_completes_as_far_as_the_names_agree() {
        let folder = folder("agree");
        let base = with_separator(&folder);
        let (completed, choices) = complete_path(&format!("{base}ho"));
        assert_eq!(completed, format!("{base}holiday-"));
        assert_eq!(choices, ["holiday-1.jpg", "holiday-2.jpg"]);
        let (completed, choices) = complete_path(&format!("{base}holiday-2"));
        assert_eq!(completed, format!("{base}holiday-2.jpg"));
        assert!(choices.is_empty());
        let (completed, _) = complete_path(&format!("{base}pho"));
        assert_eq!(
            completed,
            format!("{base}photos{}", std::path::MAIN_SEPARATOR)
        );
        let (_, choices) = complete_path(&base);
        assert!(
            !choices.iter().any(|name| name.starts_with('.')),
            "hidden files only when asked for: {choices:?}"
        );
        let (completed, choices) = complete_path(&format!("{base}nothing"));
        assert_eq!(completed, format!("{base}nothing"));
        assert!(choices.is_empty());
        let _ = std::fs::remove_dir_all(folder);
    }
}
