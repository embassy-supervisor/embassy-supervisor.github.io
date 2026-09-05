//! Native guard for fault injection through the playground's shell: the
//! crate's `fault-inject` verbs, done to a task built at runtime from DSL
//! text. Mirrors `supervisor/tests/fault_inject.rs` scenario by scenario,
//! minus hog (the mock clock only advances from the test thread, and the
//! page's one thread could never do that mid-spin).
//!
//! One `#[test]` fn: the builder's statics fill once per process. Verbs
//! that need the embassy executor run on a mailbox task; the assertions
//! stay on the test thread so a failure fails the test.

use std::sync::Mutex;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, Fault, FaultKind, InjectError, TaskNode, try_request_control};
use embassy_supervisor_playground::{build, events, health, parse, registry};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    node TICKER = Terminate, task: ticker_task, beat_timeout: 200, ack_timeout: 200;
    node SIBLING = Terminate, task: sibling_task, beat_timeout: 200;
    node PARKER = Pause, task: parker_task, ack_timeout: 200;
    node LENDER = Terminate, task: lender_task, resources: [PORT: Uart],
        slot_timeout: 200;
    node PARKED = Terminate;
}
"#;

const BEHAVIORS: &str = r#"{
    "TICKER": { "kind": "periodic", "period_ms": 50 },
    "SIBLING": { "kind": "periodic", "period_ms": 50 },
    "PARKER": { "kind": "periodic", "period_ms": 50 },
    "LENDER": { "kind": "idle" }
}"#;

#[derive(Clone, Copy, Debug)]
enum Verb {
    Stop,
    Start,
    Deactivate,
    Activate,
}

static VERB: Channel<CriticalSectionRawMutex, (&'static TaskNode, Verb), 2> = Channel::new();
static RESULTS: Mutex<Vec<Result<(), FaultKind>>> = Mutex::new(Vec::new());

#[embassy_executor::task]
async fn verbs(spawner: Spawner) {
    let sup = build::built().unwrap().sup;
    loop {
        let (node, verb) = VERB.receive().await;
        let r = match verb {
            Verb::Stop => sup.stop_node(node).await,
            Verb::Start => sup.start_node(node, &spawner).await,
            Verb::Deactivate => sup.deactivate(node).await,
            Verb::Activate => {
                sup.activate(node, &spawner).await;
                Ok(())
            }
        };
        RESULTS.lock().unwrap().push(r.map_err(|f| f.kind));
    }
}

#[embassy_executor::task]
async fn health_driver() {
    health::drive().await
}

/// The page's loop: report the fault and re-enter, never stop.
#[embassy_executor::task]
async fn supervise(spawner: Spawner) {
    loop {
        let fault = build::drive_supervisor(&spawner).await;
        events::push_fault(&fault);
    }
}

fn settle(mut cond: impl FnMut() -> bool, max_virtual_ms: u64) -> bool {
    for _ in 0..max_virtual_ms {
        if cond() {
            return true;
        }
        MockDriver::get().advance(Duration::from_millis(1));
        std::thread::sleep(StdDuration::from_micros(300));
    }
    cond()
}

fn advance_ms(n: u64) {
    let mut left = n;
    settle(
        || {
            left = left.saturating_sub(1);
            left == 0
        },
        n + 1,
    );
}

fn by_name(name: &str) -> &'static registry::NodeRt {
    registry::nodes()
        .iter()
        .find(|rt| rt.model.name == name)
        .unwrap_or_else(|| panic!("no node {name}"))
}

/// Run a supervisor verb on the executor and wait for its result.
fn verb(node: &'static TaskNode, v: Verb) -> Result<(), FaultKind> {
    let n = RESULTS.lock().unwrap().len();
    VERB.try_send((node, v)).expect("mailbox");
    assert!(
        settle(|| RESULTS.lock().unwrap().len() > n, 5000),
        "verb never returned"
    );
    RESULTS.lock().unwrap()[n]
}

/// What the page would have seen: health events and supervisor faults,
/// accumulated across snapshots (each snapshot drains them).
#[derive(Default)]
struct Seen {
    health: Vec<(String, String)>,
    faults: Vec<String>,
    fault_of: Vec<(String, Option<String>)>,
}

impl Seen {
    fn pump(&mut self) {
        let snap = events::snapshot();
        self.health.extend(
            snap.health
                .iter()
                .map(|h| (h.node.to_string(), h.kind.clone())),
        );
        self.faults.extend(snap.faults.iter().cloned());
        self.fault_of = snap
            .nodes
            .iter()
            .map(|n| (n.name.to_string(), n.fault.map(str::to_string)))
            .collect();
    }

    fn stale(&self, node: &str) -> usize {
        self.health
            .iter()
            .filter(|(n, k)| n == node && k.starts_with("stale"))
            .count()
    }

    fn recovered(&self, node: &str) -> usize {
        self.health
            .iter()
            .filter(|(n, k)| n == node && k == "recovered")
            .count()
    }

    fn fault_shown(&self, node: &str) -> Option<String> {
        self.fault_of
            .iter()
            .find(|(n, _)| n == node)
            .and_then(|(_, f)| f.clone())
    }
}

#[test]
fn faults_are_done_to_the_task() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    let built = build::build(outcome.model.take().unwrap(), BEHAVIORS).expect("build");
    assert!(built.named_executors.is_empty());
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|sp| {
            sp.spawn(health_driver().unwrap());
            sp.spawn(verbs(sp).unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    let ticker = by_name("TICKER").node;
    let sibling = by_name("SIBLING").node;
    let parker = by_name("PARKER").node;
    let lender = by_name("LENDER").node;
    let parked = by_name("PARKED").node;
    let port = registry::resources()
        .iter()
        .find(|r| r.name == "PORT")
        .unwrap();

    assert!(
        settle(
            || ticker.is_running()
                && sibling.is_running()
                && parker.is_running()
                && lender.is_running(),
            3000
        ),
        "bring-up did not settle"
    );
    assert!(!port.slot.is_filled(), "the lender took its port");
    let mut seen = Seen::default();

    // ── baseline: a healthy graph reports nothing ──────────────────────────
    advance_ms(600);
    seen.pump();
    assert!(seen.health.is_empty(), "{:?}", seen.health);
    assert_eq!(ticker.fault(), Fault::None);

    // ── stall: the shell withholds polls; the monitor reports it ───────────
    ticker.inject(Fault::Stall).unwrap();
    advance_ms(100); // the inject's own wake lands
    let polls = ticker.poll_count();
    let sib_polls = sibling.poll_count();
    assert!(
        settle(
            || {
                seen.pump();
                seen.stale("TICKER") == 1
            },
            2000
        ),
        "the monitor reports the stalled node"
    );
    assert_eq!(ticker.poll_count(), polls, "a stalled task is not polled");
    assert!(
        sibling.poll_count() > sib_polls,
        "only the stalled node froze"
    );
    assert_eq!(seen.stale("SIBLING"), 0);
    assert!(ticker.is_running(), "stalled, not down");
    assert_eq!(seen.fault_shown("TICKER").as_deref(), Some("stall"));
    ticker.clear_fault();
    assert!(
        settle(
            || {
                seen.pump();
                seen.recovered("TICKER") == 1
            },
            2000
        ),
        "the monitor sees it beat again"
    );
    assert!(ticker.poll_count() > polls, "polls resumed");
    assert_eq!(seen.stale("TICKER"), 1, "one Stale per episode");
    seen.pump();
    assert_eq!(seen.fault_shown("TICKER"), None);

    // ── stall, then a stop: the stall lifts for the shutdown ───────────────
    ticker.inject(Fault::Stall).unwrap();
    advance_ms(50);
    verb(ticker, Verb::Stop).expect("a stalled task still answers a stop");
    assert!(!ticker.is_running());
    assert_eq!(
        ticker.fault(),
        Fault::Stall,
        "a stop does not clear the fault"
    );
    ticker.clear_fault();
    verb(ticker, Verb::Start).expect("start");
    assert!(settle(|| ticker.is_running(), 1000));

    // ── wedge, through the control queue: the loop reports and re-enters ──
    ticker.inject(Fault::Wedge).unwrap();
    seen.pump();
    assert_eq!(seen.fault_shown("TICKER").as_deref(), Some("wedge"));
    try_request_control(ticker, ControlOp::Restart).unwrap();
    assert!(
        settle(
            || {
                seen.pump();
                !seen.faults.is_empty()
            },
            2000
        ),
        "the restart runs into the ack window"
    );
    assert!(seen.faults[0].contains("TICKER"), "{:?}", seen.faults);
    assert!(ticker.is_running(), "a wedged node stays marked running");
    assert!(!ticker.shutdown_requested(), "the request is hidden");
    ticker.clear_fault();
    assert!(
        settle(|| !ticker.is_running(), 1000),
        "the swallowed ack lands"
    );
    assert!(ticker.shutdown_requested());
    // The supervisor re-entered: its verbs still work.
    verb(ticker, Verb::Start).expect("start after the episode");
    assert!(settle(|| ticker.is_running(), 1000));
    advance_ms(300);
    seen.pump();
    assert_eq!(seen.faults.len(), 1, "one fault for the episode");

    // ── wedge across a pause, then a resume in place ───────────────────────
    parker.inject(Fault::Wedge).unwrap();
    let err = verb(parker, Verb::Deactivate).expect_err("the pause never acks");
    assert!(matches!(err, FaultKind::ShutdownTimeout), "{err:?}");
    assert!(parker.is_running());
    parker.clear_fault();
    assert!(
        settle(|| !parker.is_running(), 1000),
        "the park lands on the clear"
    );
    verb(parker, Verb::Activate).unwrap();
    assert!(settle(|| parker.is_running(), 1000), "resumed in place");
    verb(parker, Verb::Deactivate).expect("a clean pause after the episode");
    assert!(!parker.is_running());
    verb(parker, Verb::Activate).unwrap();
    assert!(settle(|| parker.is_running(), 1000));

    // ── crash: the future is dropped, the shell exits cleanly ──────────────
    lender.inject(Fault::Crash).unwrap();
    assert!(settle(|| lender.has_exited(), 1000));
    assert!(!lender.is_running());
    assert_eq!(lender.fault(), Fault::None, "a crash is one-shot");
    assert!(port.slot.is_filled(), "the lent port came back");
    assert_eq!(
        port.held_by.load(std::sync::atomic::Ordering::Relaxed),
        registry::HELD_BY_NONE
    );
    try_request_control(lender, ControlOp::Restart).unwrap();
    assert!(
        settle(|| lender.is_running(), 2000),
        "a crashed node respawns"
    );
    assert!(!port.slot.is_filled(), "and takes its port again");

    // ── a parked node has no shell ─────────────────────────────────────────
    assert_eq!(parked.inject(Fault::Stall), Err(InjectError::NoShell));
    assert_eq!(parked.inject(Fault::Crash), Err(InjectError::NoShell));
    parked
        .inject(Fault::Wedge)
        .expect("wedge lives in the node");
    assert_eq!(parked.fault(), Fault::Wedge);
    parked.clear_fault();
    assert_eq!(parked.fault(), Fault::None);
}
