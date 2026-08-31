//! The app-owned health escalation driver: the liveness monitor is
//! report-only by design, and this is the application deciding what a stale
//! node means. Shared between the wasm page and the native guards.

use embassy_supervisor::{ControlOp, HealthKind, try_request_control, wait_health};

use crate::build::Escalation;
use crate::{build, events, registry};

/// Drain health events forever, applying the built graph's escalation policy
/// before recording each one for the snapshot.
pub async fn drive() -> ! {
    loop {
        let ev = wait_health().await;
        let kind = match ev.kind {
            // Ticks are µs on this target; the reader thinks in ms.
            HealthKind::Stale { ticks } => format!("stale({}ms)", ticks / 1000),
            HealthKind::Recovered => "recovered".to_string(),
            _ => "unknown".to_string(),
        };
        let stale = matches!(ev.kind, HealthKind::Stale { .. });
        let policy = build::built()
            .and_then(|b| b.escalations.get(ev.node.name()))
            .cloned()
            .unwrap_or(Escalation::Report);
        let action = if !stale {
            "reported".to_string()
        } else {
            match &policy {
                Escalation::Report => "reported".to_string(),
                Escalation::ClearReady => {
                    ev.node.clear_ready();
                    log::warn!("policy: {} stale — readiness withdrawn", ev.node.name());
                    "clear_ready".to_string()
                }
                Escalation::Restart => {
                    let _ = try_request_control(ev.node, ControlOp::Restart);
                    log::warn!("policy: {} stale — restart queued", ev.node.name());
                    "restart".to_string()
                }
                Escalation::Deactivate => {
                    let _ = try_request_control(ev.node, ControlOp::Deactivate);
                    log::warn!("policy: {} stale — deactivate queued", ev.node.name());
                    "deactivate".to_string()
                }
                Escalation::Activate(other) => {
                    if let Some(rt) = registry::nodes().iter().find(|rt| rt.model.name == *other) {
                        let _ = try_request_control(rt.node, ControlOp::Activate);
                        log::warn!("policy: {} stale — activating {other}", ev.node.name());
                    }
                    format!("activate:{other}")
                }
            }
        };
        events::push_health(ev.node.name(), kind, action);
    }
}
