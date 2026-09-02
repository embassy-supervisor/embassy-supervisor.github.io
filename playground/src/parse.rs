//! DSL text -> [`GraphModel`] via the real `embassy-supervisor-syntax` parser
//! (through `embassy-supervisor-tools`' pure entry points), plus the badge
//! and cycle analysis the interpreter needs.
//!
//! Field extraction comes from the tools crate's typed [`full_model`]
//! accessor — the same projection `model_json` is built on, so the
//! playground cannot drift from the diagram tooling. What stays here is
//! *interpretation*: badges for clauses the interpreter degrades, the
//! literal-only policy on pool bounds, the signal table and the Kahn order.
//!
//! Only pure tools APIs are called (`parse_source`, `resolve`,
//! `scan_source`, `dataflow_lints`, `full_model`); the crate's fs/process
//! code is never reachable from them, so it links away on wasm.

use embassy_supervisor_tools::model::{
    self as tm, CfgTexts, ExprValue, ItemModel, LitValue, TaskKind,
};
use embassy_supervisor_tools::{
    Decl, DeclKind, LintCats, dataflow_lints, full_model, parse_source, resolve, scan_source,
};
use std::collections::BTreeMap;

use crate::model::*;

const FILE: &str = "playground.rs";

pub fn parse(src: &str) -> ParseOutcome {
    let decls = match parse_source(src, FILE) {
        Ok(d) => d,
        Err(e) => {
            return ParseOutcome {
                ok: false,
                errors: vec![ParseError {
                    line: e.line,
                    msg: e.message,
                }],
                lints: Vec::new(),
                badges: Vec::new(),
                model: None,
            };
        }
    };

    let (resolved, resolve_warnings) = resolve(decls);

    let mut discovered = Vec::new();
    scan_source(src, FILE, &mut discovered);
    let mut lints = dataflow_lints(&resolved, &discovered, &[], &LintCats::all(), &[]);
    lints.extend(resolve_warnings);

    let mut badges = Vec::new();
    let mut errors = Vec::new();

    let runnable: Vec<&Decl> = resolved
        .iter()
        .filter(|d| matches!(d.kind, DeclKind::Graph | DeclKind::Compose))
        .collect();
    let Some(decl) = runnable.first() else {
        let msg = if resolved.iter().any(|d| d.kind == DeclKind::Fragment) {
            "only supervisor_fragment! declarations found; add a compose_graph! or a supervisor_graph!"
        } else {
            "no supervisor_graph! declaration found"
        };
        errors.push(ParseError {
            line: 1,
            msg: msg.to_string(),
        });
        return ParseOutcome {
            ok: false,
            errors,
            lints,
            badges,
            model: None,
        };
    };
    for extra in runnable.iter().skip(1) {
        badges.push(Badge {
            item: extra.name().unwrap_or_else(|| "graph".into()),
            clause: "graph".into(),
            note: "only the first graph runs in the playground".into(),
        });
    }

    // The one spec-level clause the typed model does not carry.
    if decl.spec.observe_writes.is_some() || decl.spec.observe_reads.is_some() {
        badge(
            &mut badges,
            "graph",
            "observe",
            "graph-level observe defaults: the playground counts every signal itself",
        );
    }

    let full = full_model(std::slice::from_ref(*decl));
    let model = build_model(&full.graphs[0], &mut badges, &mut errors);
    let ok = errors.is_empty();
    ParseOutcome {
        ok,
        errors,
        lints,
        badges,
        model,
    }
}

fn badge(badges: &mut Vec<Badge>, item: &str, clause: &str, note: &str) {
    badges.push(Badge {
        item: item.to_string(),
        clause: clause.to_string(),
        note: note.to_string(),
    });
}

fn cfg_badge(badges: &mut Vec<Badge>, item: &str, clause: &str, cfg: &CfgTexts) {
    if !cfg.is_empty() {
        badge(badges, item, clause, "cfg-gated: treated as enabled");
    }
}

/// Path/expr token text with the spacing `quote` inserts stripped, so a
/// `task:` path doubles as a behavior-map key (`crate::mqtt::publish_task`).
fn tight(text: &str) -> String {
    text.replace(' ', "")
}

fn lit_u64(l: &LitValue, item: &str, clause: &str, errors: &mut Vec<ParseError>) -> Option<u64> {
    match l.as_u64() {
        Some(v) => Some(v),
        None => {
            errors.push(ParseError {
                line: 1,
                msg: format!("{item}: {clause} must be an integer literal"),
            });
            None
        }
    }
}

/// The playground's own policy: pool bounds must be small integer literals
/// (the runtime builder sizes real member arrays from them).
fn expr_u8(e: &ExprValue, item: &str, clause: &str, errors: &mut Vec<ParseError>) -> Option<u8> {
    if let Ok(v) = tight(&e.text).parse::<u8>() {
        return Some(v);
    }
    errors.push(ParseError {
        line: 1,
        msg: format!("{item}: the playground needs `{clause}` to be a small integer literal"),
    });
    None
}

/// Recover a shrink cooldown from a `policy:` expression such as
/// `DeferredShrink::new(Duration::from_secs(4))`. Anything else keeps the
/// default and gets a badge.
fn policy_cooldown_ms(policy: &ExprValue, item: &str, badges: &mut Vec<Badge>) -> u64 {
    let text = tight(&policy.text);
    for (pat, scale) in [("from_secs(", 1000u64), ("from_millis(", 1)] {
        if let Some(i) = text.find(pat) {
            let rest = &text[i + pat.len()..];
            if let Some(end) = rest.find(')')
                && let Ok(v) = rest[..end].parse::<u64>()
            {
                return v * scale;
            }
        }
    }
    badge(
        badges,
        item,
        "policy:",
        "unrecognized policy expression: using DeferredShrink with a 4 s cooldown",
    );
    4000
}

fn signal_refs(
    list: &[tm::SignalModel],
    item: &str,
    clause: &str,
    badges: &mut Vec<Badge>,
) -> Vec<SignalRef> {
    list.iter()
        .map(|s| {
            if !s.cfg.is_empty() {
                badge(badges, item, clause, "cfg-gated entry: treated as enabled");
            }
            if s.via.is_some() {
                badge(
                    badges,
                    item,
                    clause,
                    "`observed via <expr>`: counted by the playground's own observer instead",
                );
            }
            SignalRef {
                name: s.path.clone(),
                observed: s.observed || s.via.is_some(),
                beat: s.beat,
                veto: s.veto,
            }
        })
        .collect()
}

fn dep_models(deps: &[tm::DepModel], item: &str, badges: &mut Vec<Badge>) -> Vec<DepModel> {
    deps.iter()
        .map(|d| {
            if !d.cfg.is_empty() {
                badge(badges, item, "deps:", "cfg-gated dep: treated as enabled");
            }
            DepModel {
                name: d.name.clone(),
                ready: d.ready,
                bound: d.bound,
            }
        })
        .collect()
}

fn resource_models(
    resources: &[tm::ResourceModel],
    item: &str,
    badges: &mut Vec<Badge>,
) -> Vec<ResourceModel> {
    resources
        .iter()
        .map(|r| {
            if !r.cfg.is_empty() {
                badge(
                    badges,
                    item,
                    "resources:",
                    "cfg-gated entry: treated as enabled",
                );
            }
            if r.local {
                badge(
                    badges,
                    item,
                    "resources:",
                    "`local` marker: single-core wasm treats it as plain",
                );
            }
            if r.serialized {
                badge(
                    badges,
                    item,
                    "resources:",
                    "`serialized`: a compile-time rule (every holder on one executor); the playground's executors all poll on one thread anyway",
                );
            }
            ResourceModel {
                name: r.name.clone(),
                consume: r.consume,
                shared: r.shared,
                divisible: r.divisible,
                serialized: r.serialized,
            }
        })
        .collect()
}

fn node_common_badges(n: &tm::NodeModel, item: &str, badges: &mut Vec<Badge>) {
    if n.executor.is_some() && n.resources.iter().any(|r| r.shared && r.local) {
        badge(
            badges,
            item,
            "resources:",
            "`shared local` with `executor:` is rejected by the real macro: a local resource cannot cross executor tiers",
        );
    }
    if !n.cfg.is_empty() {
        badge(badges, item, "#[cfg]", "cfg-gated node: treated as enabled");
    }
    if n.exit.is_some() {
        badge(
            badges,
            item,
            "exit:",
            "exit values are macro-generated statics; not simulated",
        );
    }
    if n.state.is_some() {
        badge(
            badges,
            item,
            "state:",
            "heap-state boxes are macro-generated; not simulated",
        );
    }
    if n.cancel {
        badge(
            badges,
            item,
            "cancel",
            "the generic worker always runs cancellable-acked",
        );
    }
    if n.discover.is_some() {
        badge(
            badges,
            item,
            "discover",
            "derived at compile time: shown and linted, not executed",
        );
    }
    if !n.dataflow.is_empty() {
        badge(
            badges,
            item,
            "dataflow:",
            "adopted fns are shown and linted; only explicit reads:/writes: execute",
        );
    }
    if n.pool_size.is_some() {
        badge(
            badges,
            item,
            "pool_size:",
            "task pool sizing is fixed in the playground",
        );
    }
}

fn build_model(
    g: &tm::GraphModel,
    badges: &mut Vec<Badge>,
    errors: &mut Vec<ParseError>,
) -> Option<GraphModel> {
    let mut nodes: Vec<NodeModel> = Vec::new();
    let mut pools: Vec<PoolModel> = Vec::new();
    let mut executors: Vec<String> = Vec::new();

    for item in &g.items {
        match item {
            ItemModel::Executor(e) => {
                if !e.cfg.is_empty() {
                    badge(
                        badges,
                        &e.name,
                        "#[cfg]",
                        "cfg-gated executor: treated as enabled",
                    );
                }
                if executors.len() >= 3 {
                    // Slot 0 is the root executor; three named ones fit MAX_EXECUTORS = 4.
                    errors.push(ParseError {
                        line: g.line,
                        msg: format!(
                            "{}: the playground supports at most 3 named executors",
                            e.name
                        ),
                    });
                } else {
                    executors.push(e.name.clone());
                }
            }
            ItemModel::Node(n) => {
                let name = n.name.clone();
                node_common_badges(n, &name, badges);
                if !matches!(n.mode.as_str(), "Terminate" | "Pause" | "OnDemand") {
                    errors.push(ParseError {
                        line: g.line,
                        msg: format!("{name}: unknown mode `{}`", n.mode),
                    });
                }
                let task = match &n.task {
                    Some(t) => {
                        if t.kind == TaskKind::Spawn {
                            badge(
                                badges,
                                &name,
                                "spawn:",
                                "hand-written tasks are simulated like task: workers",
                            );
                        }
                        Some(tight(&t.path))
                    }
                    None => {
                        badge(
                            badges,
                            &name,
                            "parked",
                            "no task: declared; the app spawns it when a scenario binds a behavior, otherwise it stays idle",
                        );
                        None
                    }
                };
                nodes.push(NodeModel {
                    name: name.clone(),
                    mode: n.mode.to_lowercase(),
                    deps: dep_models(&n.deps, &name, badges),
                    task,
                    resources: resource_models(&n.resources, &name, badges),
                    provides: n
                        .provides
                        .iter()
                        .map(|p| {
                            if !p.cfg.is_empty() {
                                badge(
                                    badges,
                                    &name,
                                    "provides:",
                                    "cfg-gated entry: treated as enabled",
                                );
                            }
                            p.name.clone()
                        })
                        .collect(),
                    disabled: n.disabled.as_ref().is_some_and(|cfg| {
                        cfg_badge(badges, &name, "disabled", cfg);
                        true
                    }),
                    executor: n.executor.clone(),
                    slot_timeout_ms: n.slot_timeout_ms.as_ref().and_then(|l| {
                        cfg_badge(badges, &name, "slot_timeout:", &l.cfg);
                        lit_u64(l, &name, "slot_timeout:", errors)
                    }),
                    ack_timeout_ms: n.ack_timeout_ms.as_ref().and_then(|l| {
                        cfg_badge(badges, &name, "ack_timeout:", &l.cfg);
                        lit_u64(l, &name, "ack_timeout:", errors)
                    }),
                    beat_timeout_ms: n.beat_timeout_ms.as_ref().and_then(|l| {
                        cfg_badge(badges, &name, "beat_timeout:", &l.cfg);
                        lit_u64(l, &name, "beat_timeout:", errors)
                    }),
                    beat_window: n.beat_window.as_ref().and_then(|l| {
                        cfg_badge(badges, &name, "beat_window:", &l.cfg);
                        lit_u64(l, &name, "beat_window:", errors).map(|v| v as u8)
                    }),
                    ready_on_write: n.ready_on_write.as_ref().is_some_and(|cfg| {
                        cfg_badge(badges, &name, "ready_on_write", cfg);
                        true
                    }),
                    reads: signal_refs(&n.reads, &name, "reads:", badges),
                    writes: signal_refs(&n.writes, &name, "writes:", badges),
                    pool: None,
                });
            }
            ItemModel::Pool(p) => build_pool(p, g.line, &mut nodes, &mut pools, badges, errors),
        }
    }

    if nodes.len() > MAX_NODES {
        errors.push(ParseError {
            line: g.line,
            msg: format!(
                "the playground runs at most {MAX_NODES} nodes (pool members included); this graph declares {}",
                nodes.len()
            ),
        });
        return None;
    }

    check_resource_kinds(&nodes, g.line, errors);
    let signals = collect_signals(&nodes, g.line, errors)?;
    let order = kahn_order(&nodes, &pools, g.line, errors)?;

    Some(GraphModel {
        name: g.name.clone(),
        nodes,
        pools,
        executors,
        signals,
        order,
    })
}

fn build_pool(
    p: &tm::PoolModel,
    line: usize,
    nodes: &mut Vec<NodeModel>,
    pools: &mut Vec<PoolModel>,
    badges: &mut Vec<Badge>,
    errors: &mut Vec<ParseError>,
) {
    let name = p.name.clone();
    if !p.cfg.is_empty() {
        badge(
            badges,
            &name,
            "#[cfg]",
            "cfg-gated pool: treated as enabled",
        );
    }
    if p.state.is_some() {
        badge(
            badges,
            &name,
            "state:",
            "heap-state boxes are macro-generated; not simulated",
        );
    }
    if p.cancel {
        badge(
            badges,
            &name,
            "cancel",
            "the generic worker always runs cancellable-acked",
        );
    }
    if p.discover.is_some() {
        badge(
            badges,
            &name,
            "discover",
            "derived at compile time: shown and linted, not executed",
        );
    }
    if !p.dataflow.is_empty() {
        badge(
            badges,
            &name,
            "dataflow:",
            "adopted fns are shown and linted; only explicit reads:/writes: execute",
        );
    }
    if p.policy_ty.is_some() {
        badge(
            badges,
            &name,
            "policy:",
            "explicit policy types are ignored; DeferredShrink is used",
        );
    }

    if p.task.kind == TaskKind::Spawn {
        badge(
            badges,
            &name,
            "spawn:",
            "hand-written tasks are simulated like task: workers",
        );
    }
    let task = tight(&p.task.path);
    for m in &p.modes {
        if !matches!(m.as_str(), "Terminate" | "Pause" | "OnDemand") {
            errors.push(ParseError {
                line,
                msg: format!("{name}: unknown pool member mode `{m}`"),
            });
        }
    }
    let (Some(min), Some(max)) = (
        expr_u8(&p.min, &name, "min:", errors),
        expr_u8(&p.max, &name, "max:", errors),
    ) else {
        return;
    };
    if max as usize > p.modes.len() || min > max {
        errors.push(ParseError {
            line,
            msg: format!(
                "{name}: need min <= max <= member count (min {min}, max {max}, {} members)",
                p.modes.len()
            ),
        });
        return;
    }
    let cooldown_ms = policy_cooldown_ms(&p.policy, &name, badges);

    let deps = dep_models(&p.deps, &name, badges);
    let resources = resource_models(&p.resources, &name, badges);
    let reads = signal_refs(&p.reads, &name, "reads:", badges);
    let writes = signal_refs(&p.writes, &name, "writes:", badges);
    let slot_timeout_ms = p.slot_timeout_ms.as_ref().and_then(|l| {
        cfg_badge(badges, &name, "slot_timeout:", &l.cfg);
        lit_u64(l, &name, "slot_timeout:", errors)
    });
    let ack_timeout_ms = p.ack_timeout_ms.as_ref().and_then(|l| {
        cfg_badge(badges, &name, "ack_timeout:", &l.cfg);
        lit_u64(l, &name, "ack_timeout:", errors)
    });

    let mut members = Vec::new();
    for (i, mode) in p.modes.iter().enumerate() {
        let member = format!("{name}#{i}");
        members.push(member.clone());
        nodes.push(NodeModel {
            name: member,
            mode: mode.to_lowercase(),
            deps: deps.clone(),
            task: Some(task.clone()),
            resources: resources.clone(),
            provides: Vec::new(),
            disabled: false,
            executor: p.executor.clone(),
            slot_timeout_ms,
            ack_timeout_ms,
            beat_timeout_ms: None,
            beat_window: None,
            ready_on_write: false,
            reads: reads.clone(),
            writes: writes.clone(),
            pool: Some(name.clone()),
        });
    }
    pools.push(PoolModel {
        name,
        modes: p.modes.clone(),
        members,
        min,
        max,
        cooldown_ms,
    });
}

/// The real macro unifies a `divisible` name across its holders like a
/// `shared` one and refuses to let it double as a take-kind slot; the
/// builder relies on the same invariant when it sizes one `Budget` per name.
fn check_resource_kinds(nodes: &[NodeModel], line: usize, errors: &mut Vec<ParseError>) {
    let mut divisible: BTreeMap<&str, bool> = BTreeMap::new();
    for n in nodes {
        for r in &n.resources {
            let prev = divisible.entry(&r.name).or_insert(r.divisible);
            if *prev != r.divisible {
                errors.push(ParseError {
                    line,
                    msg: format!(
                        "{}: `{}` is declared `divisible` by one holder and as a slot by another; a budget cannot double as a slot",
                        n.name, r.name
                    ),
                });
            }
        }
    }
}

fn collect_signals(
    nodes: &[NodeModel],
    line: usize,
    errors: &mut Vec<ParseError>,
) -> Option<Vec<SignalModel>> {
    let mut order: Vec<String> = Vec::new();
    let mut map: BTreeMap<String, SignalModel> = BTreeMap::new();
    for n in nodes {
        for (refs, write) in [(&n.writes, true), (&n.reads, false)] {
            for s in refs {
                if !map.contains_key(&s.name) {
                    order.push(s.name.clone());
                    map.insert(
                        s.name.clone(),
                        SignalModel {
                            name: s.name.clone(),
                            ..Default::default()
                        },
                    );
                }
                let m = map.get_mut(&s.name).unwrap();
                let side = if write {
                    &mut m.writers
                } else {
                    &mut m.readers
                };
                if !side.contains(&n.name) {
                    side.push(n.name.clone());
                }
                m.observed |= s.observed;
                m.beat |= s.beat;
                if write && s.veto {
                    m.veto = true;
                    if !m.veto_writers.contains(&n.name) {
                        m.veto_writers.push(n.name.clone());
                    }
                }
            }
        }
    }
    for m in map.values() {
        if m.veto_writers.len() > MAX_VETO_SLOTS {
            errors.push(ParseError {
                line,
                msg: format!(
                    "`{}`: a veto gate holds at most {MAX_VETO_SLOTS} contributor bits; this graph declares {} veto writers",
                    m.name,
                    m.veto_writers.len()
                ),
            });
            return None;
        }
    }
    if order.len() > MAX_SIGNALS {
        errors.push(ParseError {
            line,
            msg: format!(
                "the playground tracks at most {MAX_SIGNALS} signals; this graph uses {}",
                order.len()
            ),
        });
        return None;
    }
    Some(order.into_iter().map(|k| map.remove(&k).unwrap()).collect())
}

/// Kahn topological sort over spawn deps. A dep naming a pool resolves to
/// the floor member only, matching the crate. Reports unknown names and
/// cycles as errors.
fn kahn_order(
    nodes: &[NodeModel],
    pools: &[PoolModel],
    line: usize,
    errors: &mut Vec<ParseError>,
) -> Option<Vec<u8>> {
    let idx: BTreeMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.name.as_str(), i))
        .collect();
    let pool_members: BTreeMap<&str, Vec<usize>> = pools
        .iter()
        .map(|p| {
            (
                p.name.as_str(),
                p.members.iter().map(|m| idx[m.as_str()]).collect(),
            )
        })
        .collect();

    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()]; // dep -> dependents
    let mut indegree = vec![0usize; nodes.len()];
    for (i, n) in nodes.iter().enumerate() {
        for d in &n.deps {
            let targets: Vec<usize> = if let Some(&j) = idx.get(d.name.as_str()) {
                vec![j]
            } else if let Some(members) = pool_members.get(d.name.as_str()) {
                // A pool-named dep resolves to the floor member only,
                // matching the crate's semantics.
                vec![members[0]]
            } else {
                errors.push(ParseError {
                    line,
                    msg: format!("{}: unknown dep `{}`", n.name, d.name),
                });
                continue;
            };
            for j in targets {
                edges[j].push(i);
                indegree[i] += 1;
            }
        }
    }
    if !errors.is_empty() {
        return None;
    }

    let mut queue: Vec<usize> = (0..nodes.len()).filter(|&i| indegree[i] == 0).collect();
    let mut order = Vec::with_capacity(nodes.len());
    let mut head = 0;
    while head < queue.len() {
        let i = queue[head];
        head += 1;
        order.push(i as u8);
        for &j in &edges[i] {
            indegree[j] -= 1;
            if indegree[j] == 0 {
                queue.push(j);
            }
        }
    }
    if order.len() != nodes.len() {
        let stuck: Vec<&str> = (0..nodes.len())
            .filter(|&i| indegree[i] > 0)
            .map(|i| nodes[i].name.as_str())
            .collect();
        errors.push(ParseError {
            line,
            msg: format!("dependency cycle involving: {}", stuck.join(", ")),
        });
        return None;
    }
    Some(order)
}
