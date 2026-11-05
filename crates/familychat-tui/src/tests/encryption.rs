//! Encryption over the fixture session (ADR 0008, "Encryption"; ADR
//! 0009): "Confirm it's you" with the recovery key (masked, never on
//! screen) or skipped, the password page, emoji verification shown as
//! emoji and names (names only in ASCII), a request from another device
//! (one at a time), and recovery with the key shown once. What was asked
//! is the fixture's record, which never holds a secret.

use crossterm::event::{Event, KeyCode};
use familychat_core::encryption::VerificationAnswer;
use familychat_test_support::fixtures::encryption::{
    EncryptionAsk, FIXTURE_PASSWORD, FIXTURE_RECOVERY_KEY,
};

use super::{
    Screen, Variant, app_for,
    behaviour::{key, typed, until},
    settled, text_of,
};
use crate::{app::Effect, encryption::Dialog};

fn takes_over(app: &crate::app::App) -> bool {
    app.rooms_ref()
        .is_some_and(|rooms| rooms.encryption.takes_over())
}

fn dialog(app: &crate::app::App) -> Option<Dialog> {
    app.rooms_ref().map(|rooms| rooms.encryption.dialog)
}

#[test]
fn the_recovery_key_confirms_this_device_and_is_never_drawn() {
    let (mut app, startup, mut screen) = settled("confirm-identity", 80, 24);
    assert!(takes_over(&app));
    let text = text_of(&screen.draw(&mut app));
    assert!(text.contains("Confirm it's you"), "{text}");
    key(&mut app, KeyCode::Char('r'));
    // A paste goes into the masked field, and never on screen.
    app.handle_event(Event::Paste(FIXTURE_RECOVERY_KEY.to_owned()));
    let text = text_of(&screen.draw(&mut app));
    assert!(!text.contains("EsTj"), "the key is on screen:\n{text}");
    assert!(text.contains("Recovery key: ••••"), "{text}");
    key(&mut app, KeyCode::Enter);
    until(&mut app, &mut screen, "the device is confirmed", |app| {
        !takes_over(app)
    });
    assert_eq!(
        startup.session().encryption_asks(),
        [EncryptionAsk::Recover { right_key: true }]
    );
    assert_eq!(
        app.status(),
        Some("Confirmed. This device can now read your messages.")
    );
}

#[test]
fn a_wrong_recovery_key_says_so_and_the_field_starts_again() {
    let (mut app, startup, mut screen) = settled("confirm-recovery-key-wrong", 80, 24);
    let text = text_of(&screen.draw(&mut app));
    assert!(text.contains("doesn't match"), "{text}");
    assert!(takes_over(&app));
    assert_eq!(
        startup.session().encryption_asks(),
        [EncryptionAsk::Recover { right_key: false }]
    );
    // Esc goes back to the ways to confirm; s and y skip.
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(dialog(&app), Some(Dialog::SkipWarning));
    key(&mut app, KeyCode::Char('y'));
    assert!(!takes_over(&app));
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("This device isn't confirmed yet"), "{text}");
    // V brings the screen back.
    key(&mut app, KeyCode::Char('V'));
    assert!(takes_over(&app));
}

#[test]
fn the_password_page_sets_the_identity_up() {
    let (mut app, startup, mut screen) = settled("confirm-password", 80, 24);
    // The fixture typed "secret": wrong.
    key(&mut app, KeyCode::Enter);
    until(&mut app, &mut screen, "the wrong password", |app| {
        app.rooms_ref()
            .is_some_and(|rooms| rooms.encryption.problem.is_some())
    });
    typed(&mut app, FIXTURE_PASSWORD);
    let text = text_of(&screen.draw(&mut app));
    assert!(!text.contains(FIXTURE_PASSWORD), "{text}");
    key(&mut app, KeyCode::Enter);
    until(&mut app, &mut screen, "encryption set up", |app| {
        !takes_over(app)
    });
    assert_eq!(
        startup.session().encryption_asks(),
        [
            EncryptionAsk::SetUpIdentity {
                with_password: true
            },
            EncryptionAsk::SetUpIdentity {
                with_password: true
            }
        ]
    );
}

#[test]
fn an_incoming_request_compares_emoji_by_name_and_verifies() {
    for variant in [Variant::Default, Variant::Ascii] {
        let (mut app, startup) = app_for("verify-incoming", variant);
        let mut screen = Screen::new(80, 24);
        screen.settle(&mut app);
        assert_eq!(dialog(&app), Some(Dialog::Verification));
        let text = text_of(&screen.draw(&mut app));
        assert!(text.contains("Ana's phone wants to verify"), "{text}");
        // Enter doesn't accept: only y.
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('y'));
        let text = text_of(&screen.settle(&mut app));
        assert!(text.contains("Compare emoji"), "{text}");
        key(&mut app, KeyCode::Char('e'));
        let text = text_of(&screen.settle(&mut app));
        for name in [
            "Dog",
            "Key",
            "Pizza",
            "Rocket",
            "Guitar",
            "Umbrella",
            "Butterfly",
        ] {
            assert!(text.contains(name), "{name}:\n{text}");
        }
        assert_eq!(
            text.contains('🐶'),
            variant == Variant::Default,
            "emoji with their names, names only in ASCII:\n{text}"
        );
        key(&mut app, KeyCode::Char('y'));
        let text = text_of(&screen.settle(&mut app));
        assert!(text.contains("Verified."), "{text}");
        key(&mut app, KeyCode::Enter);
        assert_eq!(dialog(&app), Some(Dialog::None));
        let answers: Vec<VerificationAnswer> = startup
            .session()
            .encryption_asks()
            .into_iter()
            .filter_map(|ask| match ask {
                EncryptionAsk::Answer(answer) => Some(answer),
                _ => None,
            })
            .collect();
        assert_eq!(
            answers,
            [
                VerificationAnswer::Accept,
                VerificationAnswer::CompareEmoji,
                VerificationAnswer::Match
            ]
        );
    }
}

#[test]
fn a_second_request_while_one_is_open_is_declined() {
    let (mut app, startup, mut screen) = settled("verify-incoming", 80, 24);
    let account = familychat_core::session::SessionApi::accounts(startup.session().as_ref())
        .borrow()[0]
        .key
        .clone();
    assert!(startup.session().request_verification(&account));
    until(
        &mut app,
        &mut screen,
        "the second request is declined",
        |_| {
            startup
                .session()
                .encryption_asks()
                .contains(&EncryptionAsk::Answer(VerificationAnswer::Cancel))
        },
    );
    assert_eq!(dialog(&app), Some(Dialog::Verification));
    // The first is still there, and n declines it too.
    key(&mut app, KeyCode::Char('n'));
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("You stopped the verification."), "{text}");
}

#[test]
fn our_verification_waits_and_cancels_on_esc() {
    let (mut app, startup, mut screen) = settled("verify-waiting", 80, 24);
    let text = text_of(&screen.draw(&mut app));
    assert!(text.contains("accept the request"), "{text}");
    key(&mut app, KeyCode::Esc);
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("You stopped the verification."), "{text}");
    key(&mut app, KeyCode::Esc);
    assert_eq!(dialog(&app), Some(Dialog::None));
    assert!(takes_over(&app), "still to confirm");
    assert!(
        startup
            .session()
            .encryption_asks()
            .contains(&EncryptionAsk::VerifyThisDevice)
    );
}

#[test]
fn recovery_shows_its_key_once_and_closes_once_it_is_kept() {
    let (mut app, startup, mut screen) = settled("recovery-key-shown", 80, 24);
    let text = text_of(&screen.draw(&mut app));
    assert!(text.contains(FIXTURE_RECOVERY_KEY), "{text}");
    assert_eq!(
        startup.session().encryption_asks(),
        [EncryptionAsk::EnableRecovery]
    );
    // Not before it is kept.
    key(&mut app, KeyCode::Enter);
    assert_eq!(dialog(&app), Some(Dialog::RecoveryKey));
    key(&mut app, KeyCode::Char('c'));
    assert_eq!(
        app.take_effects(),
        [Effect::Copy(FIXTURE_RECOVERY_KEY.to_owned())]
    );
    key(&mut app, KeyCode::Enter);
    assert_eq!(dialog(&app), Some(Dialog::None));
    assert!(
        app.rooms_ref()
            .is_some_and(|rooms| rooms.encryption.recovery_key.is_none()),
        "the key is dropped (wiped) with the dialog"
    );
    let text = text_of(&screen.settle(&mut app));
    assert!(!text.contains(FIXTURE_RECOVERY_KEY), "{text}");
    // ^R again: it is set up now.
    app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char('r'),
        crossterm::event::KeyModifiers::CONTROL,
    )));
    assert_eq!(app.status(), Some("Recovery is already set up."));
}
