//! `ComposerView`: where the person writes in the open room (PLAN.md,
//! section 13.7; ADR 0012). Enter sends and Shift+Enter starts a new line;
//! `@` suggests the room's members; a reply or an edit shows what it
//! answers or replaces, with a way to cancel; files come from the attach
//! button, the clipboard or a drop, and wait as cards until the message is
//! sent; the room hears that the person is typing.
//!
//! The composer decides no rule: what is sent, the Markdown, the upload
//! limit, taking a picture's location out, and refusing everything while
//! the account is paused are `familychat-actions`', which it calls. While
//! the account is paused (a Family Chat curfew) the field is read-only and
//! says so; the window's banner says "Chat is paused".
//!
//! The pattern is Zmail's compose (`zmail-ui`'s `compose.rs`: the attach
//! picker, dropped files and their cards) and its recipient field's
//! suggestion list (`compose/recipients.rs`: Up, Down, Enter and Escape
//! work the list while it shows), adapted to one chat field.

use std::{path::PathBuf, sync::Arc, time::Instant};

use familychat_actions::{
    Actions, ComposerState, DRAFT_SAVE_DELAY, FileSource, SendTarget, TYPING_IDLE, TypingNotices,
    files::size_label,
    mentions::{MentionQuery, complete, mention_query, suggestions},
};
use familychat_core::{
    OwnedEventId,
    messaging::{Draft, Mention, MessageRef},
    rooms::{RoomMember, RoomSummary},
    session::{AccountKey, SessionApi},
    timeline::MessageBody,
};
use gpui::{
    AnyElement, ClipboardEntry, Context, Entity, EventEmitter, ExternalPaths, FocusHandle,
    Focusable, FontWeight, ImageFormat, IntoElement, PathPromptOptions, Render, Role, Subscription,
    Task, Window, div, prelude::*, px, rgb,
};

use crate::a11y::AccessibleExt;
use crate::components::actions::Cancel;
use crate::components::popover::Popover;
use crate::components::text_input::{self, Paste};
use crate::components::{
    Avatar, ComponentTheme, HitTargetExt, IconButton, MIN_TARGET_SIZE, TextInput, TextInputEvent,
};
use crate::signin::primary_key;
use crate::styles::{
    ICON_SIZE_SM, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TYPE_BODY, TYPE_CAPTION,
    TypeStyleExt, UiColors, contrast::mix,
};
use crate::widgets::icon;

/// The composer's key context.
pub(crate) const KEY_CONTEXT: &str = "Composer";
/// Added while the mention suggestions show.
pub(crate) const MENTIONS_CONTEXT: &str = "showing_mentions";
/// The widest the suggestion list is.
const SUGGESTIONS_WIDTH: f32 = 320.0;
/// A suggestion's height.
const SUGGESTION_HEIGHT: f32 = 36.0;

/// What a message being written answers or replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A new message.
    New,
    /// A reply.
    Reply {
        /// The message it answers.
        event_id: OwnedEventId,
        /// Who wrote it.
        sender: String,
        /// One line of it.
        text: String,
    },
    /// New text for one of the account's messages.
    Edit {
        /// The message.
        target: MessageRef,
        /// One line of it as it was.
        text: String,
    },
}

/// A file waiting to be sent with the next message.
#[derive(Debug, Clone)]
pub(crate) struct PendingFile {
    source: FileSource,
    name: String,
    size: u64,
}

/// What the composer asks of its room view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposerEvent {
    /// A message went: the timeline follows it.
    Sent,
    /// Up in an empty composer: edit the account's latest message.
    EditLatest,
}

/// The composer of one room.
pub(crate) struct ComposerView {
    room: RoomSummary,
    actions: Actions,
    input: Entity<TextInput>,
    state: ComposerState,
    target: Target,
    mentions: Vec<Mention>,
    members: Vec<RoomMember>,
    query: Option<MentionQuery>,
    highlight: usize,
    files: Vec<PendingFile>,
    error: Option<String>,
    typing: TypingNotices,
    /// The draft was read (or there was none), so typing can save it.
    draft_loaded: bool,
    typing_task: Task<()>,
    draft_task: Task<()>,
    _members_task: Task<()>,
    _input: Subscription,
}

impl EventEmitter<ComposerEvent> for ComposerView {}

impl ComposerView {
    /// The composer of `room`, in `state`.
    pub(crate) fn new(
        session: Arc<dyn SessionApi>,
        account: AccountKey,
        room: RoomSummary,
        actions: Actions,
        state: ComposerState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = state.placeholder(&room.name);
        let input = cx.new(|cx| {
            TextInput::new(cx)
                .multiline()
                .submit_on_enter()
                .rows(1, 8)
                .label(format!("Message {}", room.name))
                .placeholder(placeholder)
        });
        let _input = cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
            TextInputEvent::Changed => this.text_changed(window, cx),
            TextInputEvent::Submitted => this.send(window, cx),
        });
        if state != ComposerState::Writable {
            input.update(cx, |input, cx| input.set_disabled(true, window, cx));
        }
        let reply = session.members(&account, &room.room_id);
        let members_task = cx.spawn(async move |this, cx| {
            if let Ok(Ok(members)) = reply.await {
                let _ = this.update(cx, |this, cx| {
                    this.members = members;
                    cx.notify();
                });
            }
        });
        cx.background_spawn(actions.load_draft(&room.room_id))
            .detach();
        Self {
            room,
            actions,
            input,
            state,
            target: Target::New,
            mentions: Vec::new(),
            members: Vec::new(),
            query: None,
            highlight: 0,
            files: Vec::new(),
            error: None,
            typing: TypingNotices::default(),
            draft_loaded: false,
            typing_task: Task::ready(()),
            draft_task: Task::ready(()),
            _members_task: members_task,
            _input,
        }
    }

    /// The text being written.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn text(&self, cx: &gpui::App) -> String {
        self.input.read(cx).text().to_owned()
    }

    /// What the message answers or replaces.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn target(&self) -> &Target {
        &self.target
    }

    /// The files waiting to be sent.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn files(&self) -> Vec<String> {
        self.files.iter().map(|file| file.name.clone()).collect()
    }

    /// What went wrong with the last file or send, if anything.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The members the mention list suggests now, by name.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn suggested(&self) -> Vec<String> {
        self.current_suggestions()
            .into_iter()
            .map(RoomMember::name)
            .collect()
    }

    /// Whether the composer can write.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn state(&self) -> ComposerState {
        self.state
    }

    /// The room changed (its name, its membership).
    pub(crate) fn set_room(&mut self, room: RoomSummary, cx: &mut Context<Self>) {
        self.room = room;
        cx.notify();
    }

    /// The account was paused or the pause ended: the field follows.
    pub(crate) fn set_state(
        &mut self,
        state: ComposerState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state == state {
            return;
        }
        self.state = state;
        let placeholder = state.placeholder(&self.room.name);
        let writable = state == ComposerState::Writable;
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, cx);
            input.set_disabled(!writable, window, cx);
        });
        if !writable {
            self.query = None;
            self.stop_typing(cx);
        }
        cx.notify();
    }

    /// Focuses the field.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut gpui::App) {
        let input = self.input.clone();
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Puts `text` in the field, as typed (fixtures and tests).
    pub(crate) fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text(text, cx));
        self.text_changed(window, cx);
    }

    /// A kept draft arrived: it fills the field if nothing was typed yet.
    pub(crate) fn apply_draft(
        &mut self,
        draft: Option<Draft>,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_loaded = true;
        let Some(draft) = draft else { return };
        if !self.input.read(cx).text().is_empty() || self.target != Target::New {
            return;
        }
        self.input
            .update(cx, |input, cx| input.set_text(draft.text.as_str(), cx));
        self.mentions = draft.mentions;
        self.target = target;
        let _ = window;
        cx.notify();
    }

    /// Starts a reply to a message.
    pub(crate) fn reply_to(
        &mut self,
        event_id: OwnedEventId,
        sender: String,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state != ComposerState::Writable {
            return;
        }
        if matches!(self.target, Target::Edit { .. }) {
            self.input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.target = Target::Reply {
            event_id,
            sender,
            text,
        };
        self.focus(window, cx);
        self.schedule_draft_save(cx);
        cx.notify();
    }

    /// Starts editing one of the account's messages: its text goes in the
    /// field.
    pub(crate) fn edit(
        &mut self,
        target: MessageRef,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state != ComposerState::Writable {
            return;
        }
        self.input.update(cx, |input, cx| input.set_text(&text, cx));
        self.mentions.clear();
        self.files.clear();
        self.target = Target::Edit {
            target,
            text: one_line(&text),
        };
        self.focus(window, cx);
        cx.notify();
    }

    /// Cancels a reply or an edit (an edit's text goes with it), or closes
    /// the suggestions.
    pub(crate) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.take().is_some() {
            cx.notify();
            return;
        }
        if matches!(self.target, Target::Edit { .. }) {
            self.input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.target = Target::New;
        self.error = None;
        self.schedule_draft_save(cx);
        let _ = window;
        cx.notify();
    }

    /// The draft as it stands.
    fn draft(&self, cx: &gpui::App) -> Draft {
        Draft {
            text: MessageBody::new(self.input.read(cx).text()),
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

    fn text_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let input = self.input.read(cx);
            (input.text().to_owned(), input.cursor_offset())
        };
        self.mentions
            .retain(|mention| text.contains(&mention.label));
        let query = mention_query(&text, cursor);
        if query.as_ref().map(|query| &query.query) != self.query.as_ref().map(|query| &query.query)
        {
            self.highlight = 0;
        }
        self.query = query;
        if let Some(typing) = self.typing.changed(Instant::now(), text.trim().is_empty()) {
            cx.background_spawn(self.actions.typing(&self.room.room_id, typing))
                .detach();
        }
        if self.typing.is_typing() {
            self.typing_task = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(TYPING_IDLE).await;
                let _ = this.update(cx, |this, cx| this.stop_typing(cx));
            });
        }
        self.schedule_draft_save(cx);
        let _ = window;
        cx.notify();
    }

    fn stop_typing(&mut self, cx: &mut Context<Self>) {
        if let Some(typing) = self.typing.stopped() {
            cx.background_spawn(self.actions.typing(&self.room.room_id, typing))
                .detach();
        }
    }

    /// Keeps the draft once typing pauses (an edit is never kept: its text
    /// is the message's).
    fn schedule_draft_save(&mut self, cx: &mut Context<Self>) {
        if !self.draft_loaded || matches!(self.target, Target::Edit { .. }) {
            return;
        }
        self.draft_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| this.save_draft_now(cx));
        });
    }

    /// Keeps the draft now (leaving the room).
    pub(crate) fn save_draft_now(&mut self, cx: &mut Context<Self>) {
        if !self.draft_loaded || matches!(self.target, Target::Edit { .. }) {
            return;
        }
        self.draft_task = Task::ready(());
        let draft = self.draft(cx);
        cx.background_spawn(self.actions.save_draft(&self.room.room_id, draft))
            .detach();
    }

    /// Sends what is written and the files waiting, then clears the field.
    pub(crate) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.is_some() && !self.current_suggestions().is_empty() {
            self.pick(self.highlight, window, cx);
            return;
        }
        if self.state != ComposerState::Writable {
            return;
        }
        let draft = self.draft(cx);
        let has_text = !draft.text.as_str().trim().is_empty();
        if !has_text && self.files.is_empty() {
            return;
        }
        let room = self.room.room_id.clone();
        let mut reply_to = draft.reply_to.clone();
        match &self.target {
            Target::Edit { target, .. } => {
                if has_text {
                    cx.background_spawn(self.actions.send(
                        &room,
                        &draft,
                        SendTarget::Edit(target.clone()),
                    ))
                    .detach();
                }
            }
            _ => {
                for file in std::mem::take(&mut self.files) {
                    cx.background_spawn(self.actions.send_file(
                        &room,
                        file.source,
                        reply_to.take(),
                    ))
                    .detach();
                }
                if has_text {
                    let target = match reply_to {
                        Some(event_id) => SendTarget::Reply(event_id),
                        None => SendTarget::New,
                    };
                    cx.background_spawn(self.actions.send(&room, &draft, target))
                        .detach();
                }
            }
        }
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.mentions.clear();
        self.files.clear();
        self.target = Target::New;
        self.error = None;
        self.query = None;
        self.stop_typing(cx);
        self.typing_task = Task::ready(());
        // The kept draft is spent.
        if self.draft_loaded {
            self.draft_task = Task::ready(());
            cx.background_spawn(self.actions.save_draft(&room, Draft::default()))
                .detach();
        }
        cx.emit(ComposerEvent::Sent);
        cx.notify();
    }

    fn current_suggestions(&self) -> Vec<&RoomMember> {
        match &self.query {
            Some(query) if self.state == ComposerState::Writable => {
                suggestions(&self.members, &query.query)
            }
            _ => Vec::new(),
        }
    }

    /// Picks the `index`th suggestion: its name replaces the `@query`.
    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(query) = self.query.clone() else {
            return;
        };
        let Some(member) = self
            .current_suggestions()
            .get(index)
            .map(|member| (*member).clone())
        else {
            return;
        };
        let text = self.input.read(cx).text().to_owned();
        let (text, cursor, mention) = complete(&text, &query, &member);
        self.input.update(cx, |input, cx| {
            input.set_text(text, cx);
            input.select_range(cursor..cursor, cx);
        });
        self.mentions.push(mention);
        self.query = None;
        self.highlight = 0;
        self.schedule_draft_save(cx);
        self.focus(window, cx);
        cx.notify();
    }

    fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.current_suggestions().len();
        if count == 0 {
            return;
        }
        let next = (self.highlight as isize + step).rem_euclid(count as isize);
        self.highlight = usize::try_from(next).unwrap_or_default();
        cx.notify();
    }

    // -- Files --------------------------------------------------------------

    /// Opens the file picker; the chosen files wait to be sent.
    pub(crate) fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_attach() {
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = this.update(cx, |this, cx| this.add_paths(paths, cx));
            }
        })
        .detach();
    }

    fn can_attach(&self) -> bool {
        self.state == ComposerState::Writable && !matches!(self.target, Target::Edit { .. })
    }

    /// Files dropped on the composer, or pasted.
    pub(crate) fn add_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            self.add_file(FileSource::Path(path), cx);
        }
    }

    /// Checks a file against the server's upload limit (through the
    /// actions) before it waits as a card; one over the limit shows why
    /// instead, and nothing is uploaded.
    pub(crate) fn add_file(&mut self, source: FileSource, cx: &mut Context<Self>) {
        if !self.can_attach() {
            return;
        }
        let checked = cx.background_spawn(self.actions.file_allowed(source.clone()));
        cx.spawn(async move |this, cx| {
            let result = checked.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(size) => {
                        let name = source.name();
                        this.error = None;
                        this.files.push(PendingFile { source, name, size });
                    }
                    Err(refusal) => this.error = Some(refusal.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Shows why a file or a message couldn't go.
    pub(crate) fn show_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(message);
        cx.notify();
    }

    fn remove_file(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.files.len() {
            self.files.remove(index);
        }
        cx.notify();
    }

    fn drop_paths(&mut self, paths: &ExternalPaths, _: &mut Window, cx: &mut Context<Self>) {
        self.add_paths(paths.paths().to_vec(), cx);
    }

    /// Secondary+V: a picture or files on the clipboard wait to be sent;
    /// text pastes into the field as usual.
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let mut found = false;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    found = true;
                    let extension = match image.format {
                        ImageFormat::Png => "png",
                        ImageFormat::Jpeg => "jpg",
                        ImageFormat::Gif => "gif",
                        ImageFormat::Webp => "webp",
                        ImageFormat::Bmp => "bmp",
                        ImageFormat::Tiff => "tiff",
                        _ => "img",
                    };
                    self.add_file(
                        FileSource::Bytes {
                            name: format!("Pasted picture.{extension}"),
                            data: Arc::from(image.bytes.clone()),
                        },
                        cx,
                    );
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    found = true;
                    self.add_paths(paths.paths().to_vec(), cx);
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if found {
            cx.stop_propagation();
        }
    }

    // -- Keys ---------------------------------------------------------------

    fn suggestions_showing(&self) -> bool {
        !self.current_suggestions().is_empty()
    }

    fn capture_up(&mut self, _: &text_input::Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.suggestions_showing() {
            cx.stop_propagation();
            self.move_highlight(-1, cx);
        } else if self.input.read(cx).text().is_empty() && self.target == Target::New {
            cx.stop_propagation();
            cx.emit(ComposerEvent::EditLatest);
        }
    }

    fn capture_down(&mut self, _: &text_input::Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.suggestions_showing() {
            cx.stop_propagation();
            self.move_highlight(1, cx);
        }
    }

    fn capture_tab(
        &mut self,
        _: &text_input::FocusNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.suggestions_showing() {
            cx.stop_propagation();
            self.pick(self.highlight, window, cx);
        }
    }

    fn on_cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.is_some() || self.target != Target::New || self.error.is_some() {
            cx.stop_propagation();
            self.cancel(window, cx);
        } else {
            cx.propagate();
        }
    }

    // -- Drawing ------------------------------------------------------------

    fn render_target(&self, colors: &UiColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, text, label) = match &self.target {
            Target::New => return None,
            Target::Reply { sender, text, .. } => (
                format!("Replying to {sender}"),
                text.clone(),
                "Cancel the reply",
            ),
            Target::Edit { text, .. } => (
                "Editing your message".to_owned(),
                text.clone(),
                "Cancel the edit",
            ),
        };
        let icon_name = if matches!(self.target, Target::Edit { .. }) {
            "pencil"
        } else {
            "reply"
        };
        Some(
            div()
                .id("composer-target")
                .accessible(Role::Group, format!("{title}: {text}"))
                .flex()
                .items_center()
                .gap(px(SPACE_2))
                .px(px(SPACE_3))
                .py(px(SPACE_1))
                .rounded(px(RADIUS_MD))
                .border_l_2()
                .border_color(rgb(colors.accent))
                .bg(rgb(mix(colors.background, colors.surface, 0.8)))
                .child(icon(icon_name, ICON_SIZE_SM, colors.accent))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .type_style(TYPE_CAPTION)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .truncate()
                                .type_style(TYPE_CAPTION)
                                .text_color(rgb(colors.muted))
                                .child(text),
                        ),
                )
                .child(
                    IconButton::new("composer-cancel-target", "x", label)
                        .shortcut("escape")
                        .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                )
                .into_any_element(),
        )
    }

    fn render_files(&self, colors: &UiColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.files.is_empty() {
            return None;
        }
        Some(
            div()
                .id("composer-files")
                .accessible(Role::List, "Files to send")
                .flex()
                .flex_wrap()
                .gap(px(SPACE_2))
                .children(self.files.iter().enumerate().map(|(index, file)| {
                    let picture = file.name.rsplit_once('.').is_some_and(|(_, extension)| {
                        matches!(
                            extension.to_ascii_lowercase().as_str(),
                            "png" | "jpg" | "jpeg" | "gif" | "webp"
                        )
                    });
                    let size = size_label(file.size);
                    div()
                        .id(("composer-file", index as u64))
                        .accessible(Role::ListItem, format!("{}, {size}", file.name))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_2))
                        .max_w(px(320.0))
                        .pl(px(SPACE_3))
                        .pr(px(SPACE_1))
                        .py(px(SPACE_1))
                        .rounded(px(RADIUS_MD))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .bg(rgb(colors.surface))
                        .child(icon(
                            if picture { "file-image" } else { "file" },
                            ICON_SIZE_SM,
                            colors.muted,
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .type_style(TYPE_CAPTION)
                                        .child(file.name.clone()),
                                )
                                .child(
                                    div()
                                        .type_style(TYPE_CAPTION)
                                        .text_color(rgb(colors.muted))
                                        .child(size),
                                ),
                        )
                        .child(
                            IconButton::new(
                                ("composer-remove-file", index as u64),
                                "x",
                                format!("Remove {}", file.name),
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_file(index, cx)),
                            ),
                        )
                }))
                .into_any_element(),
        )
    }

    fn render_error(&self, colors: &UiColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let error = self.error.clone()?;
        Some(
            div()
                .id("composer-error")
                .accessible(Role::Alert, error.clone())
                .flex()
                .items_center()
                .gap(px(SPACE_2))
                .px(px(SPACE_3))
                .py(px(SPACE_1))
                .rounded(px(RADIUS_MD))
                .border_1()
                .border_color(rgb(colors.danger))
                .bg(rgb(colors.danger_background))
                .child(icon("octagon-alert", ICON_SIZE_SM, colors.danger))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.foreground))
                        .child(error),
                )
                .child(
                    IconButton::new("composer-dismiss-error", "x", "Dismiss").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.error = None;
                            cx.notify();
                        }),
                    ),
                )
                .into_any_element(),
        )
    }

    fn render_suggestions(&self, colors: &UiColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let suggestions = self.current_suggestions();
        if suggestions.is_empty() {
            return None;
        }
        let count = suggestions.len();
        let list = Popover::new().min_width(px(SUGGESTIONS_WIDTH)).child(
            div()
                .id("composer-mentions")
                .accessible(Role::ListBox, "People to mention")
                .flex()
                .flex_col()
                .children(suggestions.into_iter().enumerate().map(|(index, member)| {
                    let name = member.name();
                    let highlighted = index == self.highlight;
                    div()
                        .id(("composer-mention", index as u64))
                        .hit_target(move || format!("composer-mention-{index}"))
                        .accessible(
                            Role::ListBoxOption,
                            format!("{name}, {} of {count}", index + 1),
                        )
                        .flex()
                        .items_center()
                        .gap(px(SPACE_2))
                        .h(px(SUGGESTION_HEIGHT))
                        .min_w(px(MIN_TARGET_SIZE))
                        .px(px(SPACE_2))
                        .rounded(px(RADIUS_MD))
                        .cursor_pointer()
                        .when(highlighted, |this| this.bg(rgb(colors.selected_background)))
                        .hover(|this| this.bg(rgb(colors.hover_background)))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.pick(index, window, cx)),
                        )
                        .child(
                            Avatar::new(name.clone())
                                .address(member.user_id.to_string())
                                .size(24.0),
                        )
                        .child(div().type_style(TYPE_BODY).child(name))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_right()
                                .type_style(TYPE_CAPTION)
                                .text_color(rgb(colors.muted))
                                .child(member.user_id.to_string()),
                        )
                })),
        );
        Some(
            div()
                .absolute()
                .bottom_full()
                .left(px(SPACE_6))
                .mb(px(SPACE_1))
                .child(list)
                .into_any_element(),
        )
    }
}

/// One short line of a message, for the reply and edit previews.
pub(crate) fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let mut short: String = line.chars().take(120).collect();
    if line.chars().count() > 120 || text.lines().filter(|line| !line.trim().is_empty()).count() > 1
    {
        short.push('…');
    }
    short
}

impl Focusable for ComposerView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for ComposerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let writable = self.state == ComposerState::Writable;
        let attach_enabled = self.can_attach();
        let showing = self.suggestions_showing();
        let mut context = gpui::KeyContext::default();
        context.add(KEY_CONTEXT);
        if showing {
            context.add(MENTIONS_CONTEXT);
        }
        div()
            .id("composer")
            .key_context(context)
            .accessible(Role::Group, "Write a message")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_2))
            .px(px(SPACE_4))
            .py(px(SPACE_3))
            .border_t_1()
            .border_color(rgb(colors.border))
            .capture_action(cx.listener(Self::capture_up))
            .capture_action(cx.listener(Self::capture_down))
            .capture_action(cx.listener(Self::capture_tab))
            .capture_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::on_cancel))
            .when(attach_enabled, |this| {
                let tint = mix(colors.background, colors.accent, 0.08);
                this.on_drop(cx.listener(Self::drop_paths))
                    .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(rgb(tint)))
            })
            .children(self.render_suggestions(&colors, cx))
            .children(self.render_target(&colors, cx))
            .children(self.render_files(&colors, cx))
            .children(self.render_error(&colors, cx))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(SPACE_2))
                    .child(
                        IconButton::new("composer-attach", "paperclip", "Attach a file")
                            .shortcut(primary_key("AttachFile"))
                            .disabled(!attach_enabled)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick_files(window, cx)),
                            ),
                    )
                    .child(div().flex_1().min_w_0().child(self.input.clone()))
                    .child(
                        IconButton::new("composer-send", "send", "Send")
                            .shortcut("enter")
                            .variant(crate::components::ButtonVariant::Primary)
                            .disabled(!writable)
                            .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::one_line;

    #[test]
    fn previews_are_one_line() {
        assert_eq!(one_line("hello"), "hello");
        assert_eq!(one_line("\nfirst\nsecond"), "first…");
        assert_eq!(one_line(&"a".repeat(200)).chars().count(), 121);
    }
}
