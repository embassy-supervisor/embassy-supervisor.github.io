//! `default executor NAME;` through the playground: the syntax crate
//! resolves it before the model is built, so inheriting nodes and pools
//! route to a real executor here, parked nodes stay on the root, and the
//! `local` same-tier rule the real macro enforces becomes a badge.
//!
//! One `#[test]` fn: the builder's statics fill once per process (the
//! parse-only checks run first, on other graphs).

use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor_playground::{build, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    executor HIGH;
    default executor THREAD;
    node A = Terminate, task: a_task;
    node B = Terminate, task: b_task, executor: HIGH;
    pool P = [Terminate, OnDemand], task: p_task,
        policy: DeferredShrink::new(Duration::from_secs(2)), min: 1, max: 2;
    node PARKED = Terminate;
    node PROV = Terminate, task: prov_task, provides: [BUF], slot_timeout: 500;
    node CONS = Terminate, deps: [PROV ready], task: cons_task,
        resources: [BUF: local Ram], slot_timeout: 500;
}
"#;

const BEHAVIORS: &str = r#"{
    "A": { "kind": "idle" },
    "B": { "kind": "idle" },
    "P": { "kind": "idle" },
    "PROV": { "kind": "provider", "startup_ms": 50 },
    "CONS": { "kind": "idle" }
}"#;

/// The provider on a written tier, the consumer inheriting the default.
const MISMATCH: &str = r#"
supervisor_graph! {
    executor HIGH;
    default executor THREAD;
    node PROV = Terminate, task: prov_task, provides: [BUF], executor: HIGH;
    node CONS = Terminate, deps: [PROV ready], task: cons_task,
        resources: [BUF: local Ram];
}
"#;

const TOO_MANY: &str = r#"
supervisor_graph! {
    executor A;
    executor B;
    executor C;
    default executor D;
    node N = Terminate, task: n_task;
}
"#;

#[embassy_executor::task]
async fn supervise(spawner: Spawner) {
    let fault = build::drive_supervisor(&spawner).await;
    panic!("supervisor faulted: {fault}");
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

fn by_name(name: &str) -> &'static registry::NodeRt {
    registry::nodes()
        .iter()
        .find(|rt| rt.model.name == name)
        .unwrap_or_else(|| panic!("no node {name}"))
}

#[test]
fn default_executor_is_inherited() {
    // ── the model ──────────────────────────────────────────────────────────
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    let model = outcome.model.take().unwrap();
    let node = |name: &str| model.nodes.iter().find(|n| n.name == name).unwrap();
    assert_eq!(
        model.executors,
        vec!["HIGH".to_string(), "THREAD".to_string()]
    );
    let a = node("A");
    assert_eq!(
        a.executor.as_deref(),
        Some("THREAD"),
        "a task: node inherits"
    );
    assert!(a.executor_defaulted);
    let b = node("B");
    assert_eq!(
        b.executor.as_deref(),
        Some("HIGH"),
        "an explicit executor wins"
    );
    assert!(!b.executor_defaulted);
    let p0 = node("P#0");
    assert_eq!(
        p0.executor.as_deref(),
        Some("THREAD"),
        "pool members inherit"
    );
    assert!(p0.executor_defaulted);
    let parked = node("PARKED");
    assert_eq!(parked.executor, None, "a parked node stays on the root");
    assert!(!parked.executor_defaulted);
    assert!(
        !outcome.badges.iter().any(|b| b.note.contains("is `local`")),
        "provider and consumer both inherit the default: one tier, no badge; got {:?}",
        outcome.badges.iter().map(|b| &b.note).collect::<Vec<_>>()
    );

    // ── the `local` same-tier rule ─────────────────────────────────────────
    let outcome = parse::parse(MISMATCH);
    assert!(outcome.ok);
    let local: Vec<_> = outcome
        .badges
        .iter()
        .filter(|b| b.note.contains("`BUF` is `local`"))
        .collect();
    assert_eq!(
        local.len(),
        2,
        "one badge per declarer: {:?}",
        outcome.badges.iter().map(|b| &b.note).collect::<Vec<_>>()
    );
    assert!(local.iter().any(|b| b.item == "PROV"));
    assert!(local.iter().any(|b| b.item == "CONS"));
    for want in ["`PROV`", "`HIGH`", "`CONS`", "`THREAD`"] {
        assert!(local[0].note.contains(want), "{}", local[0].note);
    }

    // ── the default counts against the executor cap ────────────────────────
    let outcome = parse::parse(TOO_MANY);
    assert!(!outcome.ok);
    assert!(
        outcome
            .errors
            .iter()
            .any(|e| e.msg.contains("at most 3 named executors")),
        "{:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );

    // ── the runtime: inheriting nodes spawn on the slot ────────────────────
    let built = build::build(model, BEHAVIORS).expect("build");
    let names: Vec<&str> = built
        .named_executors
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names, ["HIGH", "THREAD"]);
    for (_, slot) in &built.named_executors {
        let slot = *slot;
        std::thread::spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|sp| slot.set(sp.make_send()));
        });
    }
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|sp| sp.spawn(supervise(sp).unwrap()));
    });
    assert!(
        settle(
            || {
                by_name("A").node.is_running()
                    && by_name("B").node.is_running()
                    && by_name("P#0").node.is_running()
                    && by_name("PROV").node.is_ready()
                    && by_name("CONS").node.is_running()
            },
            3000
        ),
        "bring-up did not settle"
    );
    assert_eq!(by_name("A").model.executor.as_deref(), Some("THREAD"));
}
