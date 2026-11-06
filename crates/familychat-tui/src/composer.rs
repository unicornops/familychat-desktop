//! The composer: where the person writes in the open room (ADR 0008,
//! "Writing"; ADR 0012; PLAN.md decision 10).
//!
//! It is Zmail's compose editor, `ratatui-textarea` with Zmail's settings
//! (no cursor-line underline, word wrapping, only plain keys reach it),
//! holding one message. Enter sends; `^J`, Alt-Enter (and Shift-Enter
//! where the terminal tells it apart) start a new line; `@` suggests the
//! room's members (Up, Down, Tab or Enter work the list); a reply or an
//! edit shows what it answers or replaces, and Esc cancels it; files wait
//! as lines until the message goes. Esc in a plain composer hands the
//! keys to the room's commands.
//!
//! The composer decides no rule: the Markdown, mentions, the upload limit
//! (checked before a file is read), a picture's location, and refusing
//! everything while paused are `familychat-actions`' (the App calls it).
//! What it holds is the person's own typing, but a draft, an edited
//! message's old text, a paste and the external editor's text all go
//! through the terminal sanitiser before they reach the editor.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use familychat_actions::{
    ComposerState, DRAFT_SAVE_DELAY, FileSource, Refusal, TYPING_IDLE, TypingNotices,
    mentions::{MentionQuery, complete, mention_query, suggestions},
};
use familychat_core::{
    OwnedEventId, OwnedRoomId,
    messaging::{Draft, Mention, MessageRef},
    rooms::RoomMember,
    session::Reply,
    timeline::MessageBody,
};
use familychat_security::terminal::{sanitize, sanitize_line};
use ratatui::style::Style;
use ratatui_textarea::{CursorMove, Input, Key, TextArea, WrapMode};
use tokio::sync::oneshot;

/// The most lines the field grows to before it scrolls.
pub(crate) const MAX_ROWS: usize = 6;

/// What a message being written answers or replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A new message.
    New,
    /// A reply.
    Reply {
        /// The message it answers.
        event_id: OwnedEventId,
        /// Who wrote it, filtered.
        sender: String,
        /// One line of it, filtered.
        text: String,
    },
    /// New text for one of the account's messages.
    Edit {
        /// The message.
        target: MessageRef,
        /// One line of it as it was, filtered.
        text: String,
    },
}

/// A file waiting to go with the next message, checked against the
/// server's limit (not read yet).
#[derive(Debug, Clone)]
pub(crate) struct PendingFile {
    pub(crate) source: FileSource,
    /// The name it goes under (the actions' safe name), filtered.
    pub(crate) name: String,
    pub(crate) size: u64,
}

/// What a key did in the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    /// Typed or moved; `true` when the text changed.
    Edited(bool),
    /// Enter: send.
    Send,
    /// Esc with nothing to cancel: the room's commands.
    Leave,
    /// Up in an empty composer: edit the account's latest message.
    EditLatest,
    /// Not the composer's key.
    Ignored,
}

/// The composer of one room.
#[derive(Debug)]
pub(crate) struct Composer {
    pub(crate) room: OwnedRoomId,
    pub(crate) editor: TextArea<'static>,
    pub(crate) target: Target,
    mentions: Vec<Mention>,
    pub(crate) members: Vec<RoomMember>,
    pub(crate) members_reply: Option<Reply<Vec<RoomMember>>>,
    pub(crate) query: Option<MentionQuery>,
    pub(crate) highlight: usize,
    pub(crate) files: Vec<PendingFile>,
    /// Files being checked against the upload limit.
    pub(crate) checks: Vec<(FileSource, oneshot::Receiver<Result<u64, Refusal>>)>,
    /// Why the last file or message couldn't go.
    pub(crate) error: Option<String>,
    pub(crate) typing: TypingNotices,
    last_change: Option<Instant>,
    /// The kept draft was read (or there was none), so typing can save it.
    pub(crate) draft_loaded: bool,
    /// When the draft is to be kept, once typing pauses.
    pub(crate) draft_due: Option<Instant>,
}

/// An editor holding `text`, with the cursor at its end.
pub(crate) fn editor(text: &str) -> TextArea<'static> {
    let mut editor = TextArea::new(text.split('\n').map(str::to_owned).collect());
    editor.set_cursor_line_style(Style::default());
    editor.set_wrap_mode(WrapMode::WordOrGlyph);
    editor.move_cursor(CursorMove::Bottom);
    editor.move_cursor(CursorMove::End);
    editor
}

/// One line of a message, for the reply and edit previews.
pub(crate) fn one_line(text: &str) -> String {
    let line = sanitize_line(text);
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 120 {
        let mut short: String = line.chars().take(119).collect();
        short.push('…');
        short
    } else {
        line
    }
}

/// Text from outside the person's keys (a paste, a draft, the editor):
/// filtered for the terminal, with Windows line ends as one.
pub(crate) fn clean(text: &str) -> String {
    sanitize(&text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// A plain key as the editor's input; control and Alt keys are ours.
fn editor_input(key: KeyEvent) -> Option<Input> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    let key = match key.code {
        KeyCode::Char(ch) => Key::Char(ch),
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => return None,
    };
    Some(Input {
        key,
        ctrl: false,
        alt: false,
        shift: false,
    })
}

/// Whether `key` starts a new line: `^J`, Alt-Enter, or Shift-Enter.
pub(crate) fn is_new_line(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::CONTROL),
        KeyCode::Char('j') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

/// Whether `key` is `^_` (which most terminals send as Ctrl-7, or as
/// Ctrl-_ or Ctrl-/ with keyboard enhancement), as Zmail's compose reads
/// it.
pub(crate) fn is_editor_key(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('_' | '7' | '/'))
}

impl Composer {
    /// An empty composer for `room`.
    pub(crate) fn new(room: OwnedRoomId, members_reply: Option<Reply<Vec<RoomMember>>>) -> Self {
        Self {
            room,
            editor: editor(""),
            target: Target::New,
            mentions: Vec::new(),
            members: Vec::new(),
            members_reply,
            query: None,
            highlight: 0,
            files: Vec::new(),
            checks: Vec::new(),
            error: None,
            typing: TypingNotices::default(),
            last_change: None,
            draft_loaded: false,
            draft_due: None,
        }
    }

    /// The text being written.
    pub(crate) fn text(&self) -> String {
        self.editor.lines().join("\n")
    }

    /// Replaces the text, the cursor at its end.
    pub(crate) fn set_text(&mut self, text: &str) {
        self.editor = editor(text);
        self.query = None;
    }

    /// The cursor as a byte offset in [`Self::text`].
    fn cursor_offset(&self) -> usize {
        let cursor = self.editor.cursor();
        let (row, column) = (cursor.0, cursor.1);
        let lines = self.editor.lines();
        let before: usize = lines.iter().take(row).map(|line| line.len() + 1).sum();
        let within = lines.get(row).map_or(0, |line| {
            line.char_indices()
                .nth(column)
                .map_or(line.len(), |(at, _)| at)
        });
        before + within
    }

    /// Puts the cursor at byte offset `offset` of the text.
    fn jump_to(&mut self, offset: usize) {
        let text = self.text();
        let before = &text[..offset.min(text.len())];
        let row = before.matches('\n').count();
        let column = before
            .rsplit('\n')
            .next()
            .map_or(0, |line| line.chars().count());
        self.editor.move_cursor(CursorMove::Jump(
            u16::try_from(row).unwrap_or(u16::MAX),
            u16::try_from(column).unwrap_or(u16::MAX),
        ));
    }

    /// What the field says when it is empty.
    pub(crate) fn placeholder(state: ComposerState, room_name: &str) -> String {
        state.placeholder(&sanitize_line(room_name))
    }

    /// The members the `@` list suggests now.
    pub(crate) fn suggestions(&self) -> Vec<&RoomMember> {
        match &self.query {
            Some(query) => suggestions(&self.members, &query.query),
            None => Vec::new(),
        }
    }

    /// The text changed at `now`: the mention being typed, the mentions
    /// still in the text, the draft's timer. The typing notice to send,
    /// if any.
    pub(crate) fn changed(&mut self, now: Instant) -> Option<bool> {
        let text = self.text();
        self.mentions
            .retain(|mention| text.contains(&mention.label));
        let query = mention_query(&text, self.cursor_offset());
        if query.as_ref().map(|query| &query.query) != self.query.as_ref().map(|query| &query.query)
        {
            self.highlight = 0;
        }
        self.query = query;
        self.last_change = Some(now);
        self.schedule_draft(now);
        self.typing.changed(now, text.trim().is_empty())
    }

    /// The cursor moved: the mention under it may have changed.
    fn moved(&mut self) {
        let text = self.text();
        self.query = mention_query(&text, self.cursor_offset());
    }

    fn schedule_draft(&mut self, now: Instant) {
        if self.draft_loaded && !matches!(self.target, Target::Edit { .. }) {
            self.draft_due = Some(now + DRAFT_SAVE_DELAY);
        }
    }

    /// Typing went quiet for [`TYPING_IDLE`]: the "stopped" notice, if the
    /// room was told the person was typing.
    pub(crate) fn idle(&mut self, now: Instant) -> Option<bool> {
        let quiet = self
            .last_change
            .is_some_and(|at| now.duration_since(at) >= TYPING_IDLE);
        if quiet && self.typing.is_typing() {
            return self.typing.stopped();
        }
        None
    }

    /// The draft is due to be kept: what to keep.
    pub(crate) fn due_draft(&mut self, now: Instant) -> Option<Draft> {
        let due = self.draft_due.is_some_and(|at| now >= at);
        if !due {
            return None;
        }
        self.draft_due = None;
        Some(self.draft())
    }

    /// The draft now (leaving the room), unless there is nothing to keep
    /// or the message is an edit (whose text is the message's).
    pub(crate) fn draft_now(&mut self) -> Option<Draft> {
        if !self.draft_loaded
            || matches!(self.target, Target::Edit { .. })
            || self.draft_due.take().is_none()
        {
            return None;
        }
        Some(self.draft())
    }

    /// The draft as it stands.
    pub(crate) fn draft(&self) -> Draft {
        Draft {
            text: MessageBody::new(self.text()),
            mentions: self.mentions.clone(),
            reply_to: match &self.target {
                Target::Reply { event_id, .. } => Some(event_id.clone()),
                _ => None,
            },
            editing: match &self.target {
                Target::Edit { target, .. } => target.event_id.clone(),
                _ => None,
            },
        }
    }

    /// A kept draft arrived: it fills the field if nothing was typed yet.
    pub(crate) fn apply_draft(&mut self, draft: Option<Draft>, reply: Option<Target>) {
        self.draft_loaded = true;
        let Some(draft) = draft else {
            return;
        };
        if !self.text().is_empty() || self.target != Target::New {
            return;
        }
        self.set_text(&clean(draft.text.as_str()));
        self.mentions = draft.mentions;
        if let Some(reply) = reply {
            self.target = reply;
        }
    }

    /// Starts a reply.
    pub(crate) fn reply_to(&mut self, event_id: OwnedEventId, sender: &str, text: &str) {
        if matches!(self.target, Target::Edit { .. }) {
            self.set_text("");
        }
        self.target = Target::Reply {
            event_id,
            sender: sanitize_line(sender),
            text: one_line(text),
        };
        self.schedule_draft(Instant::now());
    }

    /// Starts an edit: the message's text goes in the field.
    pub(crate) fn edit(&mut self, target: MessageRef, text: &str) {
        let text = clean(text);
        self.set_text(&text);
        self.mentions.clear();
        self.files.clear();
        self.target = Target::Edit {
            target,
            text: one_line(&text),
        };
    }

    /// Esc: closes the suggestions, or cancels a reply or an edit (whose
    /// text goes with it), or clears the error. `false` when there was
    /// nothing to cancel.
    pub(crate) fn cancel(&mut self) -> bool {
        if self.query.take().is_some() {
            return true;
        }
        if self.target != Target::New {
            if matches!(self.target, Target::Edit { .. }) {
                self.set_text("");
            }
            self.target = Target::New;
            self.error = None;
            self.schedule_draft(Instant::now());
            return true;
        }
        self.error.take().is_some()
    }

    /// Picks the `index`th suggestion: its name replaces the `@query`.
    pub(crate) fn pick(&mut self, index: usize) -> bool {
        let Some(query) = self.query.clone() else {
            return false;
        };
        let Some(member) = self
            .suggestions()
            .get(index)
            .map(|member| (*member).clone())
        else {
            return false;
        };
        let (text, cursor, mention) = complete(&self.text(), &query, &member);
        let text = clean(&text);
        self.editor = editor(&text);
        self.jump_to(cursor);
        self.mentions.push(Mention {
            label: sanitize_line(&mention.label),
            user_id: mention.user_id,
        });
        self.query = None;
        self.highlight = 0;
        self.schedule_draft(Instant::now());
        true
    }

    /// Text pasted into the field (bracketed paste), filtered.
    pub(crate) fn paste(&mut self, pasted: &str) {
        self.editor.insert_str(clean(pasted));
    }

    /// The external editor's text replaces the field's.
    pub(crate) fn replace_text(&mut self, text: &str) {
        self.set_text(&clean(text));
    }

    /// Clears the field after a send.
    pub(crate) fn sent(&mut self) -> Option<bool> {
        self.set_text("");
        self.mentions.clear();
        self.files.clear();
        self.target = Target::New;
        self.error = None;
        self.query = None;
        self.draft_due = None;
        self.typing.stopped()
    }

    /// A key while the composer has the keys and can write.
    pub(crate) fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        let showing = !self.suggestions().is_empty();
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        if is_new_line(&key) {
            self.editor.insert_newline();
            return KeyOutcome::Edited(true);
        }
        match key.code {
            KeyCode::Enter if showing => {
                self.pick(self.highlight);
                return KeyOutcome::Edited(true);
            }
            KeyCode::Tab if showing => {
                self.pick(self.highlight);
                return KeyOutcome::Edited(true);
            }
            KeyCode::Enter if plain => return KeyOutcome::Send,
            KeyCode::Esc => {
                return if self.cancel() {
                    KeyOutcome::Edited(false)
                } else {
                    KeyOutcome::Leave
                };
            }
            KeyCode::Up if showing => {
                let count = self.suggestions().len();
                self.highlight = (self.highlight + count - 1) % count;
                return KeyOutcome::Edited(false);
            }
            KeyCode::Down if showing => {
                let count = self.suggestions().len();
                self.highlight = (self.highlight + 1) % count;
                return KeyOutcome::Edited(false);
            }
            KeyCode::Up if self.text().is_empty() && self.target == Target::New => {
                return KeyOutcome::EditLatest;
            }
            _ => {}
        }
        let Some(input) = editor_input(key) else {
            return KeyOutcome::Ignored;
        };
        let before = self.text();
        self.editor.input(input);
        let changed = self.text() != before;
        if !changed {
            self.moved();
        }
        KeyOutcome::Edited(changed)
    }

    /// Whether a file can be added (not while editing a message).
    pub(crate) fn can_attach(&self) -> bool {
        !matches!(self.target, Target::Edit { .. })
    }

    /// Answers that arrived: the room's members, the file checks. `true`
    /// when anything did.
    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(reply) = self.members_reply.as_mut() {
            match reply.try_recv() {
                Ok(Ok(members)) => {
                    self.members = members;
                    self.members_reply = None;
                    changed = true;
                }
                Ok(Err(_)) | Err(oneshot::error::TryRecvError::Closed) => {
                    self.members_reply = None;
                    changed = true;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        let mut done = Vec::new();
        self.checks
            .retain_mut(|(source, reply)| match reply.try_recv() {
                Ok(result) => {
                    done.push((source.clone(), result));
                    false
                }
                Err(oneshot::error::TryRecvError::Empty) => true,
                Err(oneshot::error::TryRecvError::Closed) => {
                    done.push((source.clone(), Err(Refusal::Unreadable)));
                    false
                }
            });
        for (source, result) in done {
            changed = true;
            match result {
                Ok(size) => {
                    self.error = None;
                    self.files.push(PendingFile {
                        name: sanitize_line(&source.name()),
                        source,
                        size,
                    });
                }
                Err(refusal) => self.error = Some(sanitize_line(&refusal.to_string())),
            }
        }
        changed
    }

    /// Answers asked for that haven't arrived.
    pub(crate) fn pending(&self) -> usize {
        usize::from(self.members_reply.is_some()) + self.checks.len()
    }
}

#[cfg(test)]
mod tests {
    use familychat_core::{OwnedUserId, rooms::Membership};

    use super::*;

    fn member(local: &str, name: &str) -> RoomMember {
        RoomMember {
            user_id: OwnedUserId::try_from(format!("@{local}:smith.safechat.family"))
                .expect("an id"),
            display_name: Some(name.to_owned()),
            membership: Membership::Joined,
            role: familychat_core::rooms::MemberRole::Member,
            is_own: false,
        }
    }

    fn room() -> OwnedRoomId {
        OwnedRoomId::try_from("!family:smith.safechat.family").expect("a room id")
    }

    fn press(composer: &mut Composer, code: KeyCode) -> KeyOutcome {
        composer.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn enter_sends_and_the_new_line_keys_do_not() {
        let mut composer = Composer::new(room(), None);
        for ch in "Hi".chars() {
            press(&mut composer, KeyCode::Char(ch));
        }
        assert_eq!(press(&mut composer, KeyCode::Enter), KeyOutcome::Send);
        for key in [
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        ] {
            assert_eq!(composer.key(key), KeyOutcome::Edited(true));
        }
        assert_eq!(composer.text(), "Hi\n\n\n");
        assert_eq!(
            composer.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            KeyOutcome::Ignored
        );
    }

    #[test]
    fn mentions_complete_from_the_members_and_go_with_the_draft() {
        let mut composer = Composer::new(room(), None);
        composer.members = vec![member("mum", "Mum"), member("dad", "Dad")];
        for ch in "Hi @da".chars() {
            press(&mut composer, KeyCode::Char(ch));
            composer.changed(Instant::now());
        }
        let names: Vec<String> = composer.suggestions().iter().map(|m| m.name()).collect();
        assert_eq!(names, ["Dad"]);
        assert_eq!(press(&mut composer, KeyCode::Tab), KeyOutcome::Edited(true));
        assert_eq!(composer.text(), "Hi Dad ");
        let draft = composer.draft();
        assert_eq!(draft.mentions.len(), 1);
        assert_eq!(draft.mentions[0].label, "Dad");
        // The cursor is after the name: typing goes on there.
        press(&mut composer, KeyCode::Char('!'));
        assert_eq!(composer.text(), "Hi Dad !");
    }

    #[test]
    fn esc_cancels_an_edit_then_leaves() {
        let mut composer = Composer::new(room(), None);
        composer.edit(
            MessageRef {
                item: familychat_core::timeline::TimelineItemId("x".to_owned()),
                event_id: None,
            },
            "old \x1b[2Jtext",
        );
        assert_eq!(composer.text(), "old text", "filtered");
        assert_eq!(
            press(&mut composer, KeyCode::Esc),
            KeyOutcome::Edited(false)
        );
        assert_eq!(composer.text(), "");
        assert_eq!(composer.target, Target::New);
        assert_eq!(press(&mut composer, KeyCode::Esc), KeyOutcome::Leave);
        assert_eq!(press(&mut composer, KeyCode::Up), KeyOutcome::EditLatest);
    }

    #[test]
    fn pastes_are_filtered_and_keep_their_lines() {
        let mut composer = Composer::new(room(), None);
        composer.paste("one\r\ntwo\x1b]0;pwned\x07\x07");
        assert_eq!(composer.text(), "one\ntwo");
    }
}
