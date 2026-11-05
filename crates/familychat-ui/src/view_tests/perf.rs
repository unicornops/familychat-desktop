//! The 1,000-room, 10,000-event fixture (PLAN.md, section 13.6): it
//! scrolls without blocking. Adapted from Zmail's `view_tests/perf.rs`.
//!
//! [`a_large_room_scrolls_without_blocking`] runs with the other view
//! tests: the room list and the timeline are virtualised, so a scroll
//! frame lays out only the rows on screen, whatever the size of the room,
//! and a change to one event measures one row again.
//!
//! [`performance_budgets`] is the recorded benchmark. It is ignored by
//! default, since it only means something in a release build:
//!
//! ```sh
//! cargo test --release -p familychat-ui --lib performance_budgets -- --ignored --nocapture
//! ```
//!
//! The real `ShellView` runs in a GPUI test window with the platform's real
//! text system (cosmic-text on Linux, Core Text on macOS, DirectWrite on
//! Windows) in place of the test platform's no-op one, so text is shaped
//! and glyphs are rasterised as in the app. Each sample is the wall time on
//! the main thread of one step (the event, its handlers, then one window
//! frame: render, layout, prepaint and paint into the scene) until the
//! executor is idle: the CPU side of a frame, without the GPU drawing it.
//! `FAMILYCHAT_PERF_REPORT=<dir>` writes the tables to
//! `<dir>/performance.md`.

use std::time::{Duration, Instant};

use familychat_core::{
    diff::ListDiff,
    sync::SyncState,
    timeline::{TimelineContent, TimelineItemKind, TimelineUpdate},
};
use familychat_test_support::fixtures::{
    FixtureProfile, FixtureSession, FixtureSignin, FixtureStartup, fixture_user,
    rooms::{FAMILY_ROOM, LARGE_EVENTS, LARGE_ROOMS},
};
use gpui::{
    Entity, Modifiers, Pixels, Point, ScrollDelta, ScrollEvent, TestAppContext, TestDispatcher,
    TouchPhase, VisualTestContext, point, px, size,
};

use super::rooms::{rooms_view, timeline};
use super::{open, wait_until};
use crate::rooms::ROOM_LIST_WIDTH;
use crate::{InitialView, RoomsLaunch, ShellView, WINDOW_HEIGHT, WINDOW_WIDTH};

/// A 120 Hz frame.
const FRAME_BUDGET: Duration = Duration::from_micros(8_333);
/// From an action to the visible change.
const ACTION_BUDGET: Duration = Duration::from_millis(100);

/// Timings of one measurement.
struct Samples {
    name: String,
    budget: Duration,
    times: Vec<Duration>,
}

impl Samples {
    fn new(name: impl Into<String>, budget: Duration) -> Self {
        Self {
            name: name.into(),
            budget,
            times: Vec::new(),
        }
    }

    fn time<T>(&mut self, step: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let value = step();
        self.times.push(started.elapsed());
        value
    }

    fn sorted(&self) -> Vec<Duration> {
        let mut times = self.times.clone();
        times.sort();
        times
    }

    /// The `percent` percentile (nearest rank).
    fn percentile(&self, percent: usize) -> Duration {
        let times = self.sorted();
        let rank = (times.len() * percent).div_ceil(100).max(1);
        times[rank - 1]
    }

    fn max(&self) -> Duration {
        self.sorted().last().copied().unwrap_or_default()
    }

    fn over_budget(&self) -> usize {
        self.times
            .iter()
            .filter(|time| **time > self.budget)
            .count()
    }

    fn row(&self) -> String {
        format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            self.name,
            self.times.len(),
            ms(self.percentile(50)),
            ms(self.percentile(95)),
            ms(self.max()),
            ms(self.budget),
            self.over_budget(),
        )
    }
}

fn ms(duration: Duration) -> String {
    format!("{:.2} ms", duration.as_secs_f64() * 1_000.)
}

fn table(title: &str, samples: &[Samples]) -> String {
    let mut table = format!(
        "\n### {title}\n\n| measurement | n | median | p95 | max | budget | over budget |\n|---|---|---|---|---|---|---|\n"
    );
    for sample in samples {
        table.push_str(&sample.row());
        table.push('\n');
    }
    print!("{table}");
    table
}

/// The window on the large fixture, with its first room open.
fn open_large(cx: &mut TestAppContext) -> (Entity<ShellView>, &mut VisualTestContext) {
    let session = FixtureSession::new();
    session.add_account(&fixture_user(), SyncState::Running);
    session.with_large(LARGE_ROOMS, LARGE_EVENTS);
    let signin = FixtureSignin::new(session.clone());
    let startup = FixtureStartup::with(session, signin, &[FixtureProfile::Ready]);
    let (view, cx) = open(
        cx,
        startup,
        InitialView::Rooms(RoomsLaunch {
            room: Some(FAMILY_ROOM.to_owned()),
            ..RoomsLaunch::default()
        }),
    );
    cx.simulate_resize(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
    wait_until(cx, "the large room list", |cx| {
        rooms_view(&view, cx).read_with(cx, |rooms, _| rooms.rooms().len() == LARGE_ROOMS)
    });
    (view, cx)
}

fn over_timeline() -> Point<Pixels> {
    point(px(ROOM_LIST_WIDTH + 300.), px(WINDOW_HEIGHT / 2.))
}

fn over_room_list() -> Point<Pixels> {
    point(px(ROOM_LIST_WIDTH / 2.), px(WINDOW_HEIGHT / 2.))
}

fn scroll_wheel(at: Point<Pixels>, dy: f32, cx: &mut VisualTestContext) {
    cx.simulate_event(ScrollEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
        ..ScrollEvent::default()
    });
}

/// The large room's first visible row.
fn top_row(view: &Entity<ShellView>, cx: &mut VisualTestContext) -> usize {
    let timeline = timeline(view, cx);
    timeline.read_with(cx, |timeline, _| {
        timeline.list_state().logical_scroll_top().item_ix
    })
}

#[gpui::test]
fn a_large_room_scrolls_without_blocking(cx: &mut TestAppContext) {
    let (view, cx) = open_large(cx);
    let timeline = timeline(&view, cx);
    let rows = timeline.read_with(cx, |timeline, _| timeline.rows().len());
    assert!(rows > LARGE_EVENTS, "{rows} rows for {LARGE_EVENTS} events");

    // Scrolling up from the end lays out only what is on screen.
    cx.simulate_mouse_move(over_timeline(), None, Modifiers::none());
    let before = top_row(&view, cx);
    timeline.update(cx, |timeline, _| timeline.take_rendered_rows());
    let mut slowest = Duration::ZERO;
    for _ in 0..60 {
        let started = Instant::now();
        scroll_wheel(over_timeline(), 120., cx);
        cx.run_until_parked();
        slowest = slowest.max(started.elapsed());
    }
    let after = top_row(&view, cx);
    assert!(
        after < before,
        "the timeline scrolled up ({before} to {after})"
    );
    let rendered = timeline.update(cx, |timeline, _| timeline.take_rendered_rows());
    assert!(
        rendered < 60 * 60,
        "60 frames laid out {rendered} rows, not the room's {rows}"
    );
    // A generous bound for a debug build on a shared runner: a frame that
    // walked the whole room would take far longer.
    assert!(slowest < Duration::from_secs(2), "a frame took {slowest:?}");

    // A reaction arriving lays out one row again, not the room.
    timeline.update(cx, |timeline, cx| {
        let (index, mut item) = timeline
            .items()
            .iter()
            .enumerate()
            .rev()
            .find(|(_, item)| matches!(item.kind, TimelineItemKind::Event(_)))
            .map(|(index, item)| (index, item.clone()))
            .expect("an event");
        if let TimelineItemKind::Event(event) = &mut item.kind {
            event.content = TimelineContent::text("edited");
        }
        timeline.take_rendered_rows();
        timeline.apply_for_test(
            vec![TimelineUpdate::Items(vec![ListDiff::Set {
                index,
                value: item,
            }])],
            cx,
        );
    });
    cx.run_until_parked();

    // The room list scrolls the same way.
    cx.simulate_mouse_move(over_room_list(), None, Modifiers::none());
    for _ in 0..30 {
        scroll_wheel(over_room_list(), -200., cx);
        cx.run_until_parked();
    }
}

#[test]
#[ignore = "builds the 10,000-event room; run with --release (see the module docs)"]
fn performance_budgets() {
    let text_system = gpui_platform::current_platform(true).text_system();
    let mut app = TestAppContext::build_with_text_system(TestDispatcher::new(0), None, text_system);
    app.update(crate::fonts::register_ui_font);
    let started = Instant::now();
    let (view, cx) = open_large(&mut app);
    let timeline_entity = timeline(&view, cx);
    let opened = started.elapsed();
    println!(
        "opened the window on {LARGE_ROOMS} rooms and a {LARGE_EVENTS}-event room in {opened:.1?}"
    );

    // Scroll frames over the timeline: a flick up from the end, then from
    // the middle and near the start.
    cx.simulate_mouse_move(over_timeline(), None, Modifiers::none());
    for _ in 0..20 {
        scroll_wheel(over_timeline(), 64., cx);
        cx.run_until_parked();
    }
    let mut flick_end = Samples::new("timeline scroll frame (from the end)", FRAME_BUDGET);
    for _ in 0..120 {
        flick_end.time(|| {
            scroll_wheel(over_timeline(), 64., cx);
            cx.run_until_parked();
        });
    }
    let rows = timeline_entity.read_with(cx, |timeline, _| timeline.rows().len());
    let mut jumps = Samples::new("timeline scrollbar-style jump", FRAME_BUDGET);
    for step in 0..60 {
        let row = (step * 7_919) % rows;
        jumps.time(|| {
            timeline_entity.update(cx, |timeline, cx| {
                timeline.list_state().scroll_to_reveal_item(row);
                cx.notify();
            });
            cx.run_until_parked();
        });
    }
    let mut flick_middle = Samples::new("timeline scroll frame (middle)", FRAME_BUDGET);
    timeline_entity.update(cx, |timeline, _| {
        timeline.list_state().scroll_to_reveal_item(rows / 2);
    });
    cx.run_until_parked();
    for _ in 0..120 {
        flick_middle.time(|| {
            scroll_wheel(over_timeline(), -64., cx);
            cx.run_until_parked();
        });
    }

    // Scroll frames over the 1,000-room list.
    cx.simulate_mouse_move(over_room_list(), None, Modifiers::none());
    let mut list = Samples::new("room list scroll frame", FRAME_BUDGET);
    for _ in 0..120 {
        list.time(|| {
            scroll_wheel(over_room_list(), -64., cx);
            cx.run_until_parked();
        });
    }

    // Changes arriving.
    let mut one_change = Samples::new("one event changed (a reaction)", ACTION_BUDGET);
    let mut reset = Samples::new("whole timeline replaced (10,000 events)", ACTION_BUDGET);
    for round in 0..20 {
        let (index, mut item, items) = timeline_entity.read_with(cx, |timeline, _| {
            let index = timeline.items().len() - 1 - round;
            (
                index,
                timeline.items()[index].clone(),
                timeline.items().to_vec(),
            )
        });
        if let TimelineItemKind::Event(event) = &mut item.kind {
            event.content = TimelineContent::text(&format!("change {round}"));
        }
        one_change.time(|| {
            timeline_entity.update(cx, |timeline, cx| {
                timeline.apply_for_test(
                    vec![TimelineUpdate::Items(vec![ListDiff::Set {
                        index,
                        value: item,
                    }])],
                    cx,
                );
            });
            cx.run_until_parked();
        });
        reset.time(|| {
            timeline_entity.update(cx, |timeline, cx| {
                timeline.apply_for_test(
                    vec![TimelineUpdate::Items(vec![ListDiff::Reset {
                        values: items,
                    }])],
                    cx,
                );
            });
            cx.run_until_parked();
        });
    }

    // Opening rooms.
    let rooms = rooms_view(&view, cx);
    let mut open_room = Samples::new("open a room (Next room)", ACTION_BUDGET);
    for _ in 0..30 {
        open_room.time(|| {
            cx.simulate_keystrokes("alt-down");
            cx.run_until_parked();
        });
    }
    drop(rooms);

    let mut markdown = format!(
        "## Rooms and timeline, {LARGE_ROOMS} rooms and {LARGE_EVENTS} events\n\nWindow opened \
         in {opened:.1?}. CPU time on the main thread per step.\n"
    );
    markdown.push_str(&table(
        "Scrolling (CPU time per frame)",
        &[flick_end, flick_middle, jumps, list],
    ));
    markdown.push_str(&table(
        "Changes and actions (CPU time)",
        &[one_change, reset, open_room],
    ));
    if let Some(dir) = std::env::var_os("FAMILYCHAT_PERF_REPORT") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("the report directory is created");
        std::fs::write(dir.join("performance.md"), &markdown).expect("the tables are written");
    }
}
