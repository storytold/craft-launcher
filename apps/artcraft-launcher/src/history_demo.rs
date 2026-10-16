//! Made-up AI history for `--demo-data`, so the History page can be seen with a realistic amount
//! of data without touching anyone's real log.
//!
//! Invariants: nothing here reads or writes the shared `jobs.jsonl`; the same log comes out every
//! time for a given `now_ms` (a small fixed generator, no clock or randomness inside), so a layout
//! looks the same from one start to the next; times are relative to `now_ms` so it always looks recent.

use craft_ai_model::history::{EndRecord, HISTORY_SCHEMA, StartRecord, merge};
use craft_ai_model::job::Usage;
use craft_ai_model::{ErrorKind, HistoryLog, HistoryRecord, Locality, Outcome};

const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// One kind of job: app, role id and label, capability, connection id and label, model, where it runs.
struct Kind {
    app: &'static str,
    role: &'static str,
    label: &'static str,
    capability: &'static str,
    connection: &'static str,
    connection_label: &'static str,
    model: &'static str,
    locality: Locality,
    destination: &'static str,
}

const KINDS: &[Kind] = &[
    Kind {
        app: "photocraft",
        role: "fill",
        label: "Generative Fill",
        capability: "image.inpaint",
        connection: "c-images",
        connection_label: "Hosted images",
        model: "flux-1.1-pro",
        locality: Locality::Remote,
        destination: "https://api.example-images.com",
    },
    Kind {
        app: "photocraft",
        role: "expand",
        label: "Generative Expand",
        capability: "image.outpaint",
        connection: "c-images",
        connection_label: "Hosted images",
        model: "gpt-image-1",
        locality: Locality::Remote,
        destination: "https://api.example-images.com",
    },
    Kind {
        app: "photocraft",
        role: "select",
        label: "Select Subject",
        capability: "image.segment",
        connection: "c-local",
        connection_label: "This computer",
        model: "sam3",
        locality: Locality::Local,
        destination: "http://127.0.0.1:8188",
    },
    Kind {
        app: "lightcraft",
        role: "denoise",
        label: "AI Denoise",
        capability: "image.denoise",
        connection: "c-local",
        connection_label: "This computer",
        model: "nafnet",
        locality: Locality::Local,
        destination: "http://127.0.0.1:8188",
    },
    Kind {
        app: "wordcraft",
        role: "summarise",
        label: "Summarise",
        capability: "text.generate",
        connection: "c-text",
        connection_label: "Hosted text",
        model: "gpt-4o",
        locality: Locality::Remote,
        destination: "https://api.example-text.com",
    },
    Kind {
        app: "pdfcraft",
        role: "translate",
        label: "Translate Page",
        capability: "text.translate",
        connection: "c-text",
        connection_label: "Hosted text",
        model: "claude-sonnet",
        locality: Locality::Remote,
        destination: "https://api.example-text.com",
    },
];

/// A fixed pseudo-random sequence (an LCG), so the demo never changes between starts.
struct Seq(u64);

impl Seq {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % below.max(1)
    }
}

/// About forty jobs over the last twelve days across four apps: mostly finished and priced, some
/// local (free), a few that failed for each reason, two cancelled, one still running.
pub fn log(now_ms: u64) -> HistoryLog {
    let mut seq = Seq(7);
    let mut records: Vec<HistoryRecord> = Vec::new();
    for n in 0..40u64 {
        let kind = &KINDS[usize::try_from(seq.next(KINDS.len() as u64)).unwrap_or(0)];
        // Newest first by index: spread across twelve days with a bias towards recent ones.
        let age = (n * n * DAY * 12 / 1600) + seq.next(3 * HOUR) + 2 * MINUTE;
        let at = now_ms.saturating_sub(age);
        let run = format!("demo-{n:03}");
        records.push(HistoryRecord::Start(StartRecord {
            v: HISTORY_SCHEMA,
            run: run.clone(),
            at_ms: at,
            app: kind.app.into(),
            role_id: kind.role.into(),
            role_label: kind.label.into(),
            capability: kind.capability.into(),
            connection: kind.connection.into(),
            connection_label: kind.connection_label.into(),
            model: kind.model.into(),
            locality: kind.locality,
            destination: kind.destination.into(),
            sent: Vec::new(),
            params: Vec::new(),
            provider_job: None,
            estimate: None,
            prompt: None,
        }));
        let outcome = match n {
            0 => None,
            5 | 21 => Some(Outcome::Cancelled),
            7 => Some(Outcome::Failed { kind: ErrorKind::Quota }),
            13 => Some(Outcome::Failed { kind: ErrorKind::RateLimited }),
            18 => Some(Outcome::Failed { kind: ErrorKind::Network }),
            27 => Some(Outcome::Failed { kind: ErrorKind::Provider }),
            33 => Some(Outcome::Failed { kind: ErrorKind::Auth }),
            _ => Some(Outcome::Done),
        };
        if let Some(outcome) = outcome {
            let paid = kind.locality == Locality::Remote && matches!(outcome, Outcome::Done);
            records.push(HistoryRecord::End(EndRecord {
                v: HISTORY_SCHEMA,
                run,
                at_ms: at + 1_500 + seq.next(if kind.locality == Locality::Local { 9_000 } else { 40_000 }),
                outcome,
                usage: paid.then(|| Usage { cost_micros: Some(8_000 + seq.next(110_000)), currency: Some("USD".into()), ..Usage::default() }),
            }));
        }
    }
    merge(records, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_791_500_000_000;

    #[test]
    fn the_same_log_comes_out_every_time() {
        assert_eq!(log(NOW), log(NOW));
    }

    #[test]
    fn it_has_enough_to_fill_the_page_in_every_state() {
        let log = log(NOW);
        assert_eq!(log.entries.len(), 40);
        assert_eq!(log.skipped, 0);
        let outcome = |f: fn(&Outcome) -> bool| log.entries.iter().filter(|e| e.outcome().is_some_and(f)).count();
        assert_eq!(log.entries.iter().filter(|e| e.end.is_none()).count(), 1, "one still running");
        assert_eq!(outcome(|o| matches!(o, Outcome::Cancelled)), 2);
        assert_eq!(outcome(|o| matches!(o, Outcome::Failed { .. })), 5);
        assert!(outcome(|o| matches!(o, Outcome::Done)) > 25);
        let apps: std::collections::BTreeSet<&str> = log.entries.iter().map(|e| e.start.app.as_str()).collect();
        assert_eq!(apps.len(), 4, "{apps:?}");
        assert!(log.entries.iter().any(|e| e.reported_cost().is_some()), "some priced");
        assert!(log.entries.iter().any(|e| e.start.locality == Locality::Local), "some free, on this computer");
    }

    #[test]
    fn times_are_in_the_past_newest_first_and_within_two_weeks() {
        let log = log(NOW);
        assert!(log.entries.iter().all(|e| e.start.at_ms < NOW && e.start.at_ms > NOW - 14 * DAY));
        assert!(log.entries.windows(2).all(|w| w[0].start.at_ms >= w[1].start.at_ms));
    }

    #[test]
    fn only_finished_remote_jobs_carry_a_price() {
        for e in &log(NOW).entries {
            if e.reported_cost().is_some() {
                assert_eq!(e.start.locality, Locality::Remote);
                assert!(matches!(e.outcome(), Some(Outcome::Done)));
            }
        }
    }
}
