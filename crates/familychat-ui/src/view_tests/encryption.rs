//! End-to-end encryption in the window (ADR 0009), over the fixture
//! session's encryption: "Confirm it's you" and skipping it, the recovery
//! key, the password the server asks for, verification (ours and
//! incoming), recovery set-up with the key shown once, the devices and
//! signing one out, room key export and import, identity warnings and
//! "Send anyway". Each screen and dialog is also checked for accessible
//! names and 24 px targets.

use std::{path::PathBuf, sync::Arc};

use familychat_core::session::SessionApi as _;
use familychat_core::{
    encryption::{
        DeviceTrust, EncryptionStatus, IdentitySetup, RecoveryState, VerificationAnswer,
        VerificationStep, wording,
    },
    messaging::RoomRequest,
    session::AccountKey,
    sync::SyncState,
    timeline::SendProblem,
};
use familychat_test_support::fixtures::{
    FixtureProfile, FixtureSession, FixtureSignin, FixtureStartup,
    encryption::{
        EncryptionAsk, FIXTURE_PASSWORD, FIXTURE_RECOVERY_KEY, family_warnings, fixture_emoji,
        settled,
    },
    fixture_user,
    rooms::FAMILY_ROOM,
};
use gpui::{AppContext as _, Entity, Role, TestAppContext, VisualTestContext};

use super::a11y::redraw;
use super::hit_targets::check_targets;
use super::{click, open, wait_until};
use crate::a11y::audit;
use crate::encryption::{Dialog, EncryptionView, Page};
use crate::{EncryptionLaunch, InitialView, RoomsLaunch, ShellView};

fn unconfirmed() -> EncryptionStatus {
    EncryptionStatus {
        device: DeviceTrust::Unverified,
        ..settled()
    }
}

/// The window signed in to the fixture family with `status`, opened as
/// `launch` says.
fn open_with(
    cx: &mut TestAppContext,
    status: EncryptionStatus,
    launch: RoomsLaunch,
    prepare: impl FnOnce(&FixtureSession, &AccountKey),
) -> (
    Entity<ShellView>,
    &mut VisualTestContext,
    Arc<FixtureSession>,
) {
    let session = FixtureSession::new();
    let account = session.add_account(&fixture_user(), SyncState::Running);
    session.with_family();
    session.set_encryption(&account, status);
    prepare(&session, &account);
    let signin = FixtureSignin::new(session.clone());
    let startup = FixtureStartup::with(session.clone(), signin, &[FixtureProfile::Ready]);
    let (view, cx) = open(cx, startup, InitialView::Rooms(launch));
    (view, cx, session)
}

fn launch(encryption: EncryptionLaunch) -> RoomsLaunch {
    RoomsLaunch {
        encryption: Some(encryption),
        ..RoomsLaunch::default()
    }
}

fn encryption(view: &Entity<ShellView>, cx: &mut VisualTestContext) -> Entity<EncryptionView> {
    let signed_in = cx
        .read_entity(view, |view, _| view.signed_in().cloned())
        .expect("the signed-in view shows");
    cx.read_entity(&signed_in, |signed_in, _| signed_in.encryption().clone())
}

fn read<T>(
    view: &Entity<ShellView>,
    cx: &mut VisualTestContext,
    with: impl FnOnce(&EncryptionView) -> T,
) -> T {
    let encryption = encryption(view, cx);
    cx.read_entity(&encryption, |encryption, _| with(encryption))
}

fn act(
    view: &Entity<ShellView>,
    cx: &mut VisualTestContext,
    with: impl FnOnce(&mut EncryptionView, &mut gpui::Window, &mut gpui::Context<EncryptionView>),
) {
    let encryption = encryption(view, cx);
    cx.update(|window, cx| encryption.update(cx, |encryption, cx| with(encryption, window, cx)));
    cx.run_until_parked();
}

fn type_into(view: &Entity<ShellView>, cx: &mut VisualTestContext, field: usize, text: &str) {
    let encryption = encryption(view, cx);
    let input = cx.read_entity(&encryption, |encryption, _| {
        encryption.inputs()[field].clone()
    });
    cx.update(|_, cx| input.update(cx, |input, cx| input.set_text(text, cx)));
}

const PASSWORD_FIELD: usize = 0;
const KEY_FIELD: usize = 1;
const PASSPHRASE_FIELD: usize = 2;
const CONFIRM_FIELD: usize = 3;

fn field_text(view: &Entity<ShellView>, cx: &mut VisualTestContext, field: usize) -> String {
    let encryption = encryption(view, cx);
    let input = cx.read_entity(&encryption, |encryption, _| {
        encryption.inputs()[field].clone()
    });
    cx.read_entity(&input, |input, _| input.text().to_owned())
}

fn assert_all_named(entries: &[(Role, String)]) {
    assert!(!entries.is_empty(), "nothing was recorded");
    let unnamed: Vec<Role> = entries
        .iter()
        .filter(|(_, label)| label.trim().is_empty())
        .map(|(role, _)| *role)
        .collect();
    assert!(unnamed.is_empty(), "unnamed controls: {unnamed:?}");
}

fn has(entries: &[(Role, String)], role: Role, label: &str) -> bool {
    audit::labels(entries, role).contains(&label)
}

#[gpui::test]
fn an_unconfirmed_device_is_asked_to_confirm_and_may_skip_with_a_warning(cx: &mut TestAppContext) {
    let (view, cx, _session) = open_with(cx, unconfirmed(), RoomsLaunch::default(), |_, _| {});
    assert!(read(&view, cx, EncryptionView::takes_over));
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(&entries, Role::Heading, wording::CONFIRM_TITLE));
    assert!(
        audit::labels(&entries, Role::Button)
            .iter()
            .any(|label| label.starts_with(wording::USE_ANOTHER_DEVICE)),
        "{entries:?}"
    );
    assert!(
        audit::labels(&entries, Role::Button)
            .iter()
            .any(|label| label.starts_with(wording::USE_RECOVERY_KEY))
    );
    assert!(check_targets("confirm it's you", cx) >= 3);

    click("button-encryption-skip", cx);
    assert_eq!(
        read(&view, cx, |view| view.dialog().clone()),
        Dialog::SkipWarning
    );
    let entries = redraw(cx);
    assert!(has(&entries, Role::Dialog, wording::SKIP_WARNING_TITLE));
    assert!(check_targets("skip warning", cx) >= 2);
    click("button-encryption-skip-confirm", cx);
    assert!(
        !read(&view, cx, EncryptionView::takes_over),
        "the rooms show"
    );
    let entries = redraw(cx);
    assert!(
        audit::labels(&entries, Role::Status)
            .iter()
            .chain(audit::labels(&entries, Role::Alert).iter())
            .any(|label| label.contains(wording::NOT_CONFIRMED_BANNER)),
        "{entries:?}"
    );

    // The banner, or the command, asks again.
    click("button-encryption-banner-confirm", cx);
    assert!(read(&view, cx, EncryptionView::takes_over));
}

#[gpui::test]
fn the_recovery_key_confirms_this_device_and_a_wrong_one_says_so(cx: &mut TestAppContext) {
    let (view, cx, session) = open_with(
        cx,
        unconfirmed(),
        launch(EncryptionLaunch::RecoveryKeyEntry),
        |_, _| {},
    );
    assert_eq!(read(&view, cx, EncryptionView::page), Page::RecoveryKey);
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(&entries, Role::Form, wording::ENTER_KEY_TITLE));
    assert!(check_targets("recovery key entry", cx) >= 2);

    type_into(&view, cx, KEY_FIELD, "EsTj 0000 not the key");
    click("button-encryption-key-submit", cx);
    wait_until(cx, "the refusal", |cx| {
        read(&view, cx, |view| view.problem().map(str::to_owned))
            == Some(wording::WRONG_RECOVERY_KEY.to_owned())
    });
    assert_eq!(
        field_text(&view, cx, KEY_FIELD),
        "",
        "the key is taken out of the field"
    );

    type_into(&view, cx, KEY_FIELD, &FIXTURE_RECOVERY_KEY.replace(' ', ""));
    click("button-encryption-key-submit", cx);
    wait_until(cx, "the confirmation", |cx| {
        !read(&view, cx, EncryptionView::takes_over)
    });
    assert_eq!(
        session.encryption_asks(),
        [
            EncryptionAsk::Recover { right_key: false },
            EncryptionAsk::Recover { right_key: true },
        ]
    );
}

#[gpui::test]
fn a_server_that_asks_for_the_password_is_answered_from_the_page(cx: &mut TestAppContext) {
    let status = EncryptionStatus {
        device: DeviceTrust::Unverified,
        identity: IdentitySetup::NeedsPassword,
        recovery: RecoveryState::Disabled,
        other_devices: false,
    };
    let (view, cx, session) = open_with(cx, status, RoomsLaunch::default(), |_, _| {});
    assert!(read(&view, cx, EncryptionView::takes_over));
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(&entries, Role::Heading, wording::PASSWORD_TITLE));
    assert!(check_targets("password page", cx) >= 2);

    click("button-encryption-password-submit", cx);
    assert_eq!(
        read(&view, cx, |view| view.problem().map(str::to_owned)),
        Some(wording::NO_PASSWORD.to_owned()),
        "nothing is sent without a password"
    );
    type_into(&view, cx, PASSWORD_FIELD, "not-it");
    click("button-encryption-password-submit", cx);
    wait_until(cx, "the refusal", |cx| {
        read(&view, cx, |view| view.problem().map(str::to_owned))
            == Some(wording::WRONG_PASSWORD.to_owned())
    });
    assert_eq!(
        field_text(&view, cx, PASSWORD_FIELD),
        "",
        "the password is wiped"
    );
    type_into(&view, cx, PASSWORD_FIELD, FIXTURE_PASSWORD);
    click("button-encryption-password-submit", cx);
    wait_until(cx, "the identity", |cx| {
        !read(&view, cx, EncryptionView::takes_over)
    });
    assert_eq!(
        session.encryption_asks(),
        [
            EncryptionAsk::SetUpIdentity {
                with_password: true
            },
            EncryptionAsk::SetUpIdentity {
                with_password: true
            },
        ]
    );
    assert!(
        !format!("{:?}", session.encryption_asks()).contains(FIXTURE_PASSWORD),
        "no password is recorded"
    );
}

#[gpui::test]
fn emoji_that_match_confirm_this_device(cx: &mut TestAppContext) {
    let (view, cx, session) = open_with(
        cx,
        unconfirmed(),
        launch(EncryptionLaunch::Verify),
        |session, _| {
            session.start_verification_at(VerificationStep::CompareEmoji(fixture_emoji()));
        },
    );
    wait_until(cx, "the emoji", |cx| {
        matches!(
            read(&view, cx, |view| view.step().clone()),
            VerificationStep::CompareEmoji(_)
        )
    });
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(&entries, Role::Dialog, wording::VERIFY_TITLE));
    assert!(
        audit::labels(&entries, Role::List)
            .iter()
            .any(|label| label.starts_with("Emoji to compare: Dog, Key")),
        "{entries:?}"
    );
    assert!(audit::labels(&entries, Role::ListItem).len() >= 7);
    assert!(check_targets("emoji", cx) >= 2);

    click("button-encryption-verify-match", cx);
    wait_until(cx, "done", |cx| {
        read(&view, cx, |view| view.step().clone()) == VerificationStep::Done
    });
    assert!(!read(&view, cx, EncryptionView::takes_over), "confirmed");
    assert_eq!(
        session.encryption_asks(),
        [
            EncryptionAsk::VerifyThisDevice,
            EncryptionAsk::Answer(VerificationAnswer::Match),
        ]
    );
    click("button-encryption-verify-close", cx);
    assert_eq!(read(&view, cx, |view| view.dialog().clone()), Dialog::None);
}

#[gpui::test]
fn an_incoming_request_is_accepted_shows_a_qr_code_then_compares_emoji(cx: &mut TestAppContext) {
    let (view, cx, session) = open_with(cx, settled(), RoomsLaunch::default(), |_, _| {});
    let account = session.accounts().borrow()[0].key.clone();
    assert!(session.request_verification(&account));
    wait_until(cx, "the request", |cx| {
        read(&view, cx, |view| view.step().clone()) == VerificationStep::Incoming
    });
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(&entries, Role::Dialog, wording::INCOMING_TITLE));
    assert!(check_targets("incoming request", cx) >= 2);

    click("button-encryption-verify-accept", cx);
    wait_until(cx, "the QR code", |cx| {
        matches!(
            read(&view, cx, |view| view.step().clone()),
            VerificationStep::Ready { qr: Some(_) }
        )
    });
    let entries = redraw(cx);
    assert!(has(
        &entries,
        Role::Image,
        "A QR code for your other device to scan"
    ));
    click("button-encryption-verify-emoji", cx);
    wait_until(cx, "the emoji", |cx| {
        matches!(
            read(&view, cx, |view| view.step().clone()),
            VerificationStep::CompareEmoji(_)
        )
    });
    click("button-encryption-verify-no-match", cx);
    wait_until(cx, "the cancellation", |cx| {
        matches!(
            read(&view, cx, |view| view.step().clone()),
            VerificationStep::Cancelled(_)
        )
    });
    assert_eq!(
        session.encryption_asks(),
        [
            EncryptionAsk::Answer(VerificationAnswer::Accept),
            EncryptionAsk::Answer(VerificationAnswer::CompareEmoji),
            EncryptionAsk::Answer(VerificationAnswer::NoMatch),
        ]
    );
}

#[gpui::test]
fn recovery_shows_its_key_once_and_wants_it_kept(cx: &mut TestAppContext) {
    let status = EncryptionStatus {
        recovery: RecoveryState::Disabled,
        ..settled()
    };
    let (view, cx, session) = open_with(cx, status, RoomsLaunch::default(), |_, _| {});
    let entries = redraw(cx);
    assert!(
        audit::labels(&entries, Role::Status)
            .iter()
            .chain(audit::labels(&entries, Role::Alert).iter())
            .any(|label| label.contains(wording::RECOVERY_BANNER)),
        "the rooms offer recovery: {entries:?}"
    );
    click("button-encryption-banner-recovery", cx);
    assert_eq!(
        read(&view, cx, |view| view.dialog().clone()),
        Dialog::SetUpRecovery
    );
    assert!(check_targets("recovery set-up", cx) >= 2);
    click("button-encryption-recovery-make", cx);
    wait_until(cx, "the key", |cx| {
        read(&view, cx, |view| view.dialog().clone()) == Dialog::RecoveryKey
    });
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(&entries, Role::Dialog, wording::RECOVERY_KEY_TITLE));
    assert!(check_targets("recovery key", cx) >= 3);
    assert_eq!(
        read(&view, cx, EncryptionView::recovery_key_state),
        (true, false)
    );

    // Done waits until the key is kept.
    click("button-encryption-recovery-done", cx);
    assert_eq!(
        read(&view, cx, |view| view.dialog().clone()),
        Dialog::RecoveryKey
    );
    click("button-encryption-recovery-copy", cx);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(FIXTURE_RECOVERY_KEY.to_owned())
    );
    click("button-encryption-recovery-done", cx);
    assert_eq!(read(&view, cx, |view| view.dialog().clone()), Dialog::None);
    assert_eq!(
        read(&view, cx, EncryptionView::recovery_key_state),
        (false, false),
        "the key is gone once the dialog closes"
    );
    assert_eq!(session.encryption_asks(), [EncryptionAsk::EnableRecovery]);
}

#[gpui::test]
fn a_recovery_key_is_saved_where_the_person_chooses(cx: &mut TestAppContext) {
    let status = EncryptionStatus {
        recovery: RecoveryState::Disabled,
        ..settled()
    };
    let (view, cx, _session) = open_with(
        cx,
        status,
        launch(EncryptionLaunch::RecoveryKeyShown),
        |_, _| {},
    );
    wait_until(cx, "the key", |cx| {
        read(&view, cx, |view| view.dialog().clone()) == Dialog::RecoveryKey
    });
    click("button-encryption-recovery-save", cx);
    assert!(cx.did_prompt_for_new_path(), "the person chooses where");
    let dir = std::env::temp_dir().join(format!("familychat-recovery-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory");
    let chosen = dir.join("key.txt");
    let answer = chosen.clone();
    cx.simulate_new_path_selection(move |_| Some(answer));
    wait_until(cx, "the save", |cx| {
        read(&view, cx, EncryptionView::recovery_key_state).1
    });
    let saved = std::fs::read_to_string(&chosen).expect("the key file");
    assert!(saved.contains(FIXTURE_RECOVERY_KEY));
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui::test]
fn devices_are_listed_and_one_is_signed_out_with_the_password(cx: &mut TestAppContext) {
    let (view, cx, session) =
        open_with(cx, settled(), launch(EncryptionLaunch::Devices), |_, _| {});
    wait_until(cx, "the devices", |cx| {
        read(&view, cx, |view| view.devices().cloned()).is_some()
    });
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(&entries, Role::Dialog, wording::DEVICES_TITLE));
    let rows = audit::labels(&entries, Role::ListItem);
    assert!(
        rows.iter()
            .any(|row| row.starts_with("Family Chat (Linux). This device. Verified.")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.starts_with("Old tablet.") && row.ends_with("Not verified.")),
        "{rows:?}"
    );
    assert!(check_targets("devices", cx) >= 4);

    // The old tablet's Sign out, as the launch presses it: the server asks
    // for the password first.
    act(&view, cx, |view, window, cx| {
        view.launch(EncryptionLaunch::SignOutDevice, window, cx);
    });
    wait_until(cx, "the password field", |cx| {
        audit::labels(&redraw(cx), Role::PasswordInput).contains(&"Password")
    });
    assert!(check_targets("sign out", cx) >= 2);
    type_into(&view, cx, PASSWORD_FIELD, "wrong");
    click("button-encryption-sign-out-confirm", cx);
    wait_until(cx, "the refusal", |cx| {
        read(&view, cx, |view| view.problem().map(str::to_owned))
            == Some(wording::WRONG_PASSWORD.to_owned())
    });
    type_into(&view, cx, PASSWORD_FIELD, FIXTURE_PASSWORD);
    click("button-encryption-sign-out-confirm", cx);
    wait_until(cx, "the devices again", |cx| {
        read(&view, cx, |view| view.dialog().clone()) == Dialog::Devices
            && read(&view, cx, |view| {
                view.devices()
                    .and_then(|devices| devices.as_ref().ok())
                    .is_some_and(|devices| {
                        devices.iter().all(|device| device.device_id != "OLDTABLET")
                    })
            })
    });
    let asks: Vec<_> = session
        .encryption_asks()
        .into_iter()
        .filter(|ask| matches!(ask, EncryptionAsk::SignOutDevice { .. }))
        .collect();
    assert_eq!(
        asks,
        [false, true, true].map(|with_password| EncryptionAsk::SignOutDevice {
            device_id: "OLDTABLET".to_owned(),
            with_password,
        })
    );
}

#[gpui::test]
fn room_keys_are_exported_with_a_checked_passphrase_and_imported(cx: &mut TestAppContext) {
    let (view, cx, session) = open_with(cx, settled(), launch(EncryptionLaunch::Export), |_, _| {});
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(&entries, Role::Dialog, wording::EXPORT_TITLE));
    assert!(check_targets("export", cx) >= 2);

    type_into(&view, cx, PASSPHRASE_FIELD, "short");
    type_into(&view, cx, CONFIRM_FIELD, "short");
    click("button-encryption-export-save", cx);
    assert_eq!(
        read(&view, cx, |view| view.problem().map(str::to_owned)),
        Some(wording::PASSPHRASE_TOO_SHORT.to_owned())
    );
    type_into(&view, cx, PASSPHRASE_FIELD, "correct horse battery");
    type_into(&view, cx, CONFIRM_FIELD, "correct horse battery staple");
    click("button-encryption-export-save", cx);
    assert_eq!(
        read(&view, cx, |view| view.problem().map(str::to_owned)),
        Some(wording::PASSPHRASES_DIFFER.to_owned())
    );
    assert!(
        !cx.did_prompt_for_new_path(),
        "nothing is asked until they match"
    );
    type_into(&view, cx, CONFIRM_FIELD, "correct horse battery");
    click("button-encryption-export-save", cx);
    assert!(cx.did_prompt_for_new_path(), "the person chooses where");
    let chosen = PathBuf::from("/fixture/Documents/keys.txt");
    let answer = chosen.clone();
    cx.simulate_new_path_selection(move |_| Some(answer));
    wait_until(cx, "the export", |cx| {
        read(&view, cx, |view| view.dialog().clone()) == Dialog::None
    });
    assert_eq!(
        field_text(&view, cx, PASSPHRASE_FIELD),
        "",
        "the passphrase is wiped"
    );

    act(&view, cx, |view, window, cx| view.import_keys(window, cx));
    let entries = redraw(cx);
    assert!(has(&entries, Role::Dialog, wording::IMPORT_TITLE));
    assert!(check_targets("import", cx) >= 2);
    click("button-encryption-import-choose", cx);
    let file = PathBuf::from("/fixture/Documents/keys.txt");
    let picked = file.clone();
    cx.simulate_path_prompt_response(move |_| Some(vec![picked]));
    cx.run_until_parked();
    type_into(&view, cx, PASSPHRASE_FIELD, "correct horse battery");
    click("button-encryption-import-go", cx);
    wait_until(cx, "the import", |cx| {
        read(&view, cx, |view| view.dialog().clone()) == Dialog::None
    });
    assert_eq!(
        session.encryption_asks(),
        [EncryptionAsk::Export(chosen), EncryptionAsk::Import(file)]
    );
}

#[gpui::test]
fn identity_warnings_show_in_the_room_and_are_answered(cx: &mut TestAppContext) {
    let (view, cx, session) = open_with(
        cx,
        settled(),
        RoomsLaunch {
            room: Some(FAMILY_ROOM.to_owned()),
            ..RoomsLaunch::default()
        },
        |session, _| session.set_identity_warnings(FAMILY_ROOM, family_warnings()),
    );
    let rooms = super::rooms::rooms_view(&view, cx);
    wait_until(cx, "the warnings", |cx| {
        rooms.read_with(cx, |rooms, _| rooms.identity_warnings().len() == 2)
    });
    let entries = redraw(cx);
    assert_all_named(&entries);
    for label in ["OK", "Withdraw verification"] {
        assert!(has(&entries, Role::Button, label), "{label}: {entries:?}");
    }
    assert!(check_targets("identity warnings", cx) >= 2);
    let warnings = rooms.read_with(cx, |rooms, _| rooms.identity_warnings().to_vec());
    let mum = warnings
        .iter()
        .position(|warning| warning.action_label() == "Withdraw verification")
        .expect("Mum's warning");
    let buttons = super::targets_named(cx, "button-room-identity-acknowledge");
    let selector = buttons
        .iter()
        .copied()
        .find(|selector| selector.ends_with(&mum.to_string()))
        .expect("Mum's button");
    click(selector, cx);
    wait_until(cx, "the answer", |cx| {
        rooms.read_with(cx, |rooms, _| rooms.identity_warnings().len() == 1)
    });
    assert_eq!(
        session.encryption_asks(),
        [EncryptionAsk::Acknowledge(
            familychat_test_support::fixtures::rooms::user("mum")
        )]
    );
}

#[gpui::test]
fn a_stopped_message_is_sent_anyway_through_the_actions(cx: &mut TestAppContext) {
    let (view, cx, session) = open_with(
        cx,
        settled(),
        RoomsLaunch {
            room: Some(FAMILY_ROOM.to_owned()),
            ..RoomsLaunch::default()
        },
        |session, _| session.stop_own_message(FAMILY_ROOM, SendProblem::UnconfirmedDevices),
    );
    let rooms = super::rooms::rooms_view(&view, cx);
    wait_until(cx, "the room", |cx| {
        rooms.read_with(cx, |rooms, cx| {
            rooms
                .timeline()
                .is_some_and(|timeline| timeline.read(cx).is_loaded())
        })
    });
    let entries = redraw(cx);
    assert!(
        audit::labels(&entries, Role::Button).contains(&wording::SEND_ANYWAY),
        "{entries:?}"
    );
    let selector = super::targets_named(cx, "button-timeline-send-anyway")
        .first()
        .copied()
        .expect("the button is a target");
    click(selector, cx);
    wait_until(cx, "the request", |_| {
        session
            .record()
            .requests
            .iter()
            .any(|(_, request)| matches!(request, RoomRequest::SendAnyway(_)))
    });
}
