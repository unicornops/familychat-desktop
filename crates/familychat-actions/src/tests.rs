//! The actions over the fixture session: what they ask the session for,
//! what they refuse before asking, and what the callback hears.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use familychat_core::{
    OwnedEventId, OwnedRoomId, OwnedUserId,
    messaging::{Draft, MediaLimits, Mention, MessageRef, RoomRequest, ServerDefaults},
    rooms::{Membership, RoomSummary},
    session::{AccountKey, SessionError},
    sync::{AccountStatus, SyncState},
    timeline::{MessageBody, TimelineItemId},
};
use familychat_security::image_metadata::has_location;
use familychat_test_support::{
    fixtures::{FIXTURE_UPLOAD_LIMIT, FixtureSession, fixture_user, rooms},
    photo::{PHOTO_LATITUDE, photo_with_location},
};

use super::*;

struct Setup {
    session: Arc<FixtureSession>,
    account: AccountKey,
    actions: Actions,
    events: Arc<Mutex<Vec<ActionEvent>>>,
}

impl Setup {
    fn new() -> Self {
        let session = FixtureSession::new();
        let account = session.add_account(&fixture_user(), SyncState::Running);
        session.with_family();
        let events = Arc::new(Mutex::new(Vec::new()));
        let heard = events.clone();
        let actions = Actions::new(session.clone(), account.clone(), move |event| {
            heard.lock().expect("events").push(event);
        });
        Self {
            session,
            account,
            actions,
            events,
        }
    }

    fn events(&self) -> Vec<ActionEvent> {
        std::mem::take(&mut *self.events.lock().expect("events"))
    }

    fn requests(&self) -> Vec<RoomRequest> {
        self.session
            .record()
            .requests
            .into_iter()
            .map(|(_, request)| request)
            .collect()
    }
}

fn family() -> OwnedRoomId {
    rooms::room_id(rooms::FAMILY_ROOM)
}

fn draft(text: &str) -> Draft {
    Draft {
        text: MessageBody::new(text),
        ..Draft::default()
    }
}

fn event_id(id: &str) -> OwnedEventId {
    OwnedEventId::try_from(id).expect("an event id")
}

#[tokio::test]
async fn a_message_is_sent_with_its_markdown_and_mentions() {
    let setup = Setup::new();
    let mum = OwnedUserId::try_from("@mum:smith.safechat.family").expect("an id");
    let draft = Draft {
        text: MessageBody::new("**Mum**, dinner?"),
        mentions: vec![Mention {
            user_id: mum.clone(),
            label: "Mum".to_owned(),
        }],
        ..Draft::default()
    };
    setup.actions.send(&family(), &draft, SendTarget::New).await;
    assert_eq!(
        setup.events(),
        [ActionEvent::Done {
            room: Some(family()),
            action: Action::Send
        }]
    );
    let requests = setup.requests();
    let [
        RoomRequest::Send {
            message,
            reply_to: None,
        },
    ] = requests.as_slice()
    else {
        panic!("{requests:?}");
    };
    assert_eq!(message.body.as_str(), "**Mum**, dinner?");
    assert_eq!(message.mentions, [mum]);
    let html = message.html.as_ref().expect("html");
    assert!(
        html.as_str().contains(
            "<strong><a href=\"https://matrix.to/#/@mum:smith.safechat.family\">Mum</a></strong>"
        ),
        "{}",
        html.as_str()
    );
}

#[tokio::test]
async fn replies_and_edits_name_their_message() {
    let setup = Setup::new();
    let answered = event_id("$family-1:smith.safechat.family");
    setup
        .actions
        .send(
            &family(),
            &draft("yes!"),
            SendTarget::Reply(answered.clone()),
        )
        .await;
    let edited = MessageRef {
        item: TimelineItemId("x".to_owned()),
        event_id: Some(answered.clone()),
    };
    setup
        .actions
        .send(
            &family(),
            &draft("yes please"),
            SendTarget::Edit(edited.clone()),
        )
        .await;
    let requests = setup.requests();
    assert!(matches!(
        &requests[0],
        RoomRequest::Send { reply_to: Some(event), .. } if *event == answered
    ));
    assert!(matches!(
        &requests[1],
        RoomRequest::Edit { target, message } if *target == edited && message.body.as_str() == "yes please"
    ));
    let actions: Vec<_> = setup
        .events()
        .into_iter()
        .map(|event| match event {
            ActionEvent::Done { action, .. } => action,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(actions, [Action::Reply, Action::Edit]);
}

#[tokio::test]
async fn an_empty_message_is_not_sent() {
    let setup = Setup::new();
    setup
        .actions
        .send(&family(), &draft("  \n "), SendTarget::New)
        .await;
    assert!(setup.requests().is_empty());
    assert!(matches!(
        setup.events().as_slice(),
        [ActionEvent::Refused {
            refusal: Refusal::Empty,
            ..
        }]
    ));
}

#[tokio::test]
async fn nothing_is_sent_while_paused_and_sending_works_again_after() {
    let setup = Setup::new();
    setup.session.set_sync(
        &setup.account,
        SyncState::Suspended {
            recheck_in: Duration::from_secs(30),
        },
    );
    assert!(setup.actions.is_paused());
    let target = MessageRef {
        item: TimelineItemId("x".to_owned()),
        event_id: Some(event_id("$family-1:smith.safechat.family")),
    };
    setup
        .actions
        .send(&family(), &draft("hello"), SendTarget::New)
        .await;
    setup
        .actions
        .toggle_reaction(&family(), target.clone(), "👍")
        .await;
    setup.actions.redact(&family(), target, None).await;
    setup.actions.typing(&family(), true).await;
    setup.actions.mark_read(&family()).await;
    setup
        .actions
        .send_file(
            &family(),
            FileSource::Bytes {
                name: "notes.txt".to_owned(),
                data: Arc::from(b"notes".as_slice()),
            },
            None,
        )
        .await;
    setup
        .actions
        .create_room(Some("Club".to_owned()), Vec::new())
        .await;
    assert!(setup.requests().is_empty(), "{:?}", setup.requests());
    assert!(setup.session.record().created.is_empty());
    let events = setup.events();
    assert_eq!(events.len(), 5, "{events:?}");
    for event in &events {
        assert!(
            matches!(
                event,
                ActionEvent::Refused {
                    refusal: Refusal::Paused,
                    ..
                }
            ),
            "{event:?}"
        );
        assert!(
            event
                .message()
                .expect("a sentence")
                .starts_with("Chat is paused")
        );
    }
    // Drafts are kept on the device, paused or not.
    setup.actions.save_draft(&family(), draft("later")).await;
    assert_eq!(setup.session.record().drafts.len(), 1);

    setup.session.set_sync(&setup.account, SyncState::Running);
    setup
        .actions
        .send(&family(), &draft("hello"), SendTarget::New)
        .await;
    assert_eq!(setup.requests().len(), 1);
}

#[tokio::test]
async fn a_send_the_server_refuses_as_paused_says_so() {
    let setup = Setup::new();
    setup.session.fail_requests(Some(SessionError::Paused));
    setup
        .actions
        .send(&family(), &draft("hello"), SendTarget::New)
        .await;
    let events = setup.events();
    let [
        event @ ActionEvent::Failed {
            error: SessionError::Paused,
            ..
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}");
    };
    assert!(
        event
            .message()
            .expect("a sentence")
            .starts_with("Chat is paused")
    );
    setup.session.fail_requests(Some(SessionError::Failed(
        "syt_token in M_UNKNOWN".to_owned(),
    )));
    setup
        .actions
        .send(&family(), &draft("hello"), SendTarget::New)
        .await;
    let message = setup.events()[0].message().expect("a sentence");
    assert_eq!(message, Action::Send.failed_sentence());
}

#[tokio::test]
async fn a_file_over_the_limit_is_refused_before_anything_is_asked_of_the_room() {
    let setup = Setup::new();
    let big = FileSource::Bytes {
        name: "holiday.mov".to_owned(),
        data: Arc::from(vec![
            0u8;
            usize::try_from(FIXTURE_UPLOAD_LIMIT).expect("fits") + 1
        ]),
    };
    setup.actions.check_file(&family(), big.clone()).await;
    setup.actions.send_file(&family(), big, None).await;
    assert!(setup.requests().is_empty());
    assert_eq!(setup.session.record().media_limit_asks, 2);
    let events = setup.events();
    assert_eq!(events.len(), 2);
    for event in events {
        let ActionEvent::Refused {
            refusal: Refusal::TooLarge { limit, .. },
            ..
        } = &event
        else {
            panic!("{event:?}");
        };
        assert_eq!(*limit, FIXTURE_UPLOAD_LIMIT);
        assert_eq!(
            event.message().expect("a sentence"),
            "That file is 11 MB, and this server takes files up to 11 MB. Nothing was sent."
        );
    }
}

#[tokio::test]
async fn without_the_servers_limit_no_file_is_sent() {
    let setup = Setup::new();
    setup.session.set_media_limits(None);
    setup
        .actions
        .send_file(
            &family(),
            FileSource::Bytes {
                name: "notes.txt".to_owned(),
                data: Arc::from(b"notes".as_slice()),
            },
            None,
        )
        .await;
    assert!(setup.requests().is_empty());
    assert!(matches!(
        setup.events().as_slice(),
        [ActionEvent::Refused {
            refusal: Refusal::LimitUnknown,
            ..
        }]
    ));
    // A server that sets no limit takes the file.
    setup.session.set_media_limits(Some(MediaLimits::default()));
    setup
        .actions
        .send_file(
            &family(),
            FileSource::Bytes {
                name: "notes.txt".to_owned(),
                data: Arc::from(b"notes".as_slice()),
            },
            None,
        )
        .await;
    assert_eq!(setup.requests().len(), 1);
}

#[tokio::test]
async fn a_photo_is_sent_without_its_location() {
    let setup = Setup::new();
    let photo = photo_with_location();
    assert!(has_location(&photo));
    let dir = std::env::temp_dir().join(format!("familychat-actions-photo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory");
    let path = dir.join("bedroom.jpg");
    std::fs::write(&path, &photo).expect("written");
    let reply_to = event_id("$family-1:smith.safechat.family");
    setup
        .actions
        .send_file(&family(), FileSource::Path(path), Some(reply_to.clone()))
        .await;
    let _ = std::fs::remove_dir_all(&dir);
    let requests = setup.requests();
    let [
        RoomRequest::SendFile {
            file,
            reply_to: Some(answered),
        },
    ] = requests.as_slice()
    else {
        panic!("{requests:?} {:?}", setup.events());
    };
    assert_eq!(*answered, reply_to);
    assert_eq!(file.file_name, "bedroom.jpg");
    assert_eq!(file.mime_type, "image/jpeg");
    assert_eq!(file.image_size, Some((32, 24)));
    assert!(!has_location(&file.data));
    let text = String::from_utf8_lossy(&file.data);
    assert!(!text.contains(PHOTO_LATITUDE));
    assert!(!text.contains("GPS"));
    assert!(file.data.len() < photo.len());
}

#[tokio::test]
async fn a_picture_whose_location_cannot_be_removed_is_not_sent() {
    let setup = Setup::new();
    let mut heic = vec![0, 0, 0, 24];
    heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
    setup
        .actions
        .send_file(
            &family(),
            FileSource::Bytes {
                name: "IMG_0001.HEIC".to_owned(),
                data: Arc::from(heic),
            },
            None,
        )
        .await;
    assert!(setup.requests().is_empty());
    let events = setup.events();
    let message = events[0].message().expect("a sentence");
    assert!(message.contains("HEIC"), "{message}");
}

#[tokio::test]
async fn new_rooms_are_encrypted_unless_the_server_says_not() {
    let setup = Setup::new();
    let mum = OwnedUserId::try_from("@mum:smith.safechat.family").expect("an id");
    setup.actions.create_direct_chat(mum.clone()).await;
    setup.session.set_server_defaults(ServerDefaults {
        encryption_default: Some(true),
    });
    setup
        .actions
        .create_room(Some("  Book club ".to_owned()), vec![mum.clone()])
        .await;
    setup.session.set_server_defaults(ServerDefaults {
        encryption_default: Some(false),
    });
    setup
        .actions
        .create_room(Some("Supervised".to_owned()), Vec::new())
        .await;
    let created = setup.session.record().created;
    assert_eq!(created.len(), 3);
    assert!(created[0].encrypted && created[0].direct && created[0].invite == [mum]);
    assert!(created[1].encrypted && !created[1].direct);
    assert_eq!(created[1].name.as_deref(), Some("Book club"));
    assert!(!created[2].encrypted);
    let events = setup.events();
    assert!(matches!(
        &events[0],
        ActionEvent::RoomCreated { direct: true, .. }
    ));
    assert!(matches!(
        &events[1],
        ActionEvent::RoomCreated { direct: false, .. }
    ));
}

#[tokio::test]
async fn drafts_are_kept_read_and_forgotten() {
    let setup = Setup::new();
    let kept = Draft {
        text: MessageBody::new("see you at"),
        reply_to: Some(event_id("$family-1:smith.safechat.family")),
        ..Draft::default()
    };
    setup.actions.save_draft(&family(), kept.clone()).await;
    setup.actions.load_draft(&family()).await;
    assert_eq!(
        setup.events(),
        [ActionEvent::DraftLoaded {
            room: family(),
            draft: Some(kept)
        }]
    );
    setup.actions.save_draft(&family(), draft("   ")).await;
    setup.actions.load_draft(&family()).await;
    assert_eq!(
        setup.events(),
        [ActionEvent::DraftLoaded {
            room: family(),
            draft: None
        }]
    );
    assert_eq!(
        setup
            .session
            .record()
            .drafts
            .last()
            .map(|(_, draft)| draft.clone()),
        Some(None)
    );
}

#[tokio::test]
async fn invitations_reactions_and_leaving_go_to_the_room() {
    let setup = Setup::new();
    let invite = rooms::room_id(rooms::INVITE_ROOM);
    setup.actions.answer_invite(&invite, true).await;
    let target = MessageRef {
        item: TimelineItemId("x".to_owned()),
        event_id: Some(event_id("$family-1:smith.safechat.family")),
    };
    setup.actions.toggle_reaction(&family(), target, "🎉").await;
    setup.actions.leave(&family()).await;
    let requests = setup.requests();
    assert_eq!(requests[0], RoomRequest::AnswerInvite(true));
    assert!(matches!(&requests[1], RoomRequest::ToggleReaction { key, .. } if key == "🎉"));
    assert_eq!(requests[2], RoomRequest::Leave);
    assert_eq!(setup.session.record().invites, [(invite, true)]);
    let actions: Vec<_> = setup
        .events()
        .into_iter()
        .filter_map(|event| match event {
            ActionEvent::Done { action, .. } => Some(action),
            _ => None,
        })
        .collect();
    assert_eq!(
        actions,
        [Action::AcceptInvite, Action::React, Action::Leave]
    );
}

#[tokio::test]
async fn the_channel_hears_every_event() {
    let session = FixtureSession::new();
    let account = session.add_account(&fixture_user(), SyncState::Running);
    session.with_family();
    let (actions, mut events) = Actions::channel(session, account);
    actions.mark_read(&family()).await;
    actions.send(&family(), &draft("hi"), SendTarget::New).await;
    assert!(matches!(
        events.try_recv(),
        Ok(ActionEvent::Done {
            action: Action::Send,
            ..
        })
    ));
    assert!(events.try_recv().is_err());
}

#[test]
fn typing_notices_start_renew_and_stop() {
    let mut typing = TypingNotices::default();
    let start = Instant::now();
    assert_eq!(typing.changed(start, false), Some(true));
    assert_eq!(typing.changed(start + Duration::from_secs(1), false), None);
    assert_eq!(typing.changed(start + TYPING_RENEW, false), Some(true));
    assert!(typing.is_typing());
    assert_eq!(typing.changed(start + TYPING_RENEW, true), Some(false));
    assert_eq!(typing.stopped(), None);
}

#[test]
fn the_composer_is_read_only_while_paused_or_outside_the_room() {
    let running = AccountStatus {
        sync: SyncState::Running,
        problem: None,
    };
    let paused = AccountStatus {
        sync: SyncState::Suspended {
            recheck_in: Duration::from_secs(30),
        },
        problem: None,
    };
    let offline = AccountStatus {
        sync: SyncState::Offline {
            retry_in: Duration::from_secs(1),
        },
        problem: None,
    };
    let room = RoomSummary::new(family(), "Family");
    let mut invite = room.clone();
    invite.membership = Membership::Invited;
    assert_eq!(ComposerState::of(&running, &room), ComposerState::Writable);
    assert_eq!(ComposerState::of(&offline, &room), ComposerState::Writable);
    assert_eq!(ComposerState::of(&paused, &room), ComposerState::Paused);
    assert_eq!(
        ComposerState::of(&running, &invite),
        ComposerState::NotJoined
    );
    assert!(
        ComposerState::Paused
            .placeholder("Family")
            .starts_with("Chat is paused")
    );
    assert_eq!(
        ComposerState::Writable.placeholder("Family"),
        "Message Family"
    );
}

#[test]
fn the_encryption_default_is_on_unless_turned_off() {
    assert!(encrypt_new_room(&ServerDefaults::default()));
    assert!(encrypt_new_room(&ServerDefaults {
        encryption_default: Some(true)
    }));
    assert!(!encrypt_new_room(&ServerDefaults {
        encryption_default: Some(false)
    }));
}
