//! Runs a sequence of requests once per data row: rows run side by side up to a
//! concurrency limit, the steps of a row run in order.

pub mod data;

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinSet;

use crate::{
    AssertionResult, Captured, HttpClient, Request, Response, SendError, Variables, capture, check,
    passed, store,
};

/// The variables one iteration adds on top of each step's own, as `(name, value)`.
pub type Row = Vec<(String, String)>;

#[derive(Debug, Clone)]
pub struct Step {
    /// With its effective auth already applied; variables are resolved per iteration.
    pub request: Request,
    pub variables: Variables,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    /// How many iterations may be in flight at once; 0 is treated as 1.
    pub concurrency: usize,
    /// Pause after each request of an iteration, to stay under rate limits.
    pub delay: Duration,
    /// Cancel the run at the first request that does not pass.
    pub bail: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            concurrency: 1,
            delay: Duration::ZERO,
            bail: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunPlan {
    pub steps: Vec<Step>,
    /// No rows means a single iteration with no extra variables.
    pub rows: Vec<Row>,
    pub options: RunOptions,
}

impl RunPlan {
    pub fn iterations(&self) -> usize {
        self.rows.len().max(1)
    }

    pub fn total(&self) -> usize {
        self.iterations() * self.steps.len()
    }
}

#[derive(Debug)]
pub struct StepResult {
    pub iteration: usize,
    pub step: usize,
    /// The request as sent, with this iteration's variables resolved.
    pub request: Request,
    pub unresolved: Vec<String>,
    pub outcome: Result<Response, SendError>,
    pub captured: Vec<Captured>,
    pub assertions: Vec<AssertionResult>,
}

impl StepResult {
    pub fn passed(&self) -> bool {
        self.outcome
            .as_ref()
            .is_ok_and(|response| passed(response, &self.assertions))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    /// Answered with a 2xx status, or passing every assertion when the request has some.
    pub succeeded: usize,
    /// Answered, but with a non-2xx status or a failed assertion.
    pub failed: usize,
    /// Never answered: connection errors, timeouts, cancelled requests.
    pub errors: usize,
    /// Not started because the run was cancelled.
    pub skipped: usize,
    pub cancelled: bool,
    pub elapsed: Duration,
}

/// Stops a run: requests in flight are dropped and no new one starts.
#[derive(Debug, Clone)]
pub struct CancelToken(Arc<watch::Sender<bool>>);

impl Default for CancelToken {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(false)))
    }
}

impl CancelToken {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }
}

/// Results are sent as they complete, so in no particular order across iterations.
pub async fn run(
    client: &HttpClient,
    plan: RunPlan,
    cancel: &CancelToken,
    results: mpsc::UnboundedSender<StepResult>,
) -> RunSummary {
    let started = Instant::now();
    let total = plan.total();
    let iterations = plan.iterations();
    let options = plan.options.clone();
    let steps = Arc::new(plan.steps);
    let permits = Arc::new(Semaphore::new(options.concurrency.max(1)));
    let (counts_tx, mut counts_rx) = mpsc::unbounded_channel();

    let mut tasks = JoinSet::new();
    let mut rows = plan.rows.into_iter();
    for iteration in 0..iterations {
        // Taken here rather than in the task, so iterations start in order.
        let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
            break;
        };
        if cancel.is_cancelled() {
            break;
        }
        let row = rows.next().unwrap_or_default();
        let client = client.clone();
        let steps = Arc::clone(&steps);
        let cancel = cancel.clone();
        let results = results.clone();
        let counts = counts_tx.clone();
        let last_iteration = iteration + 1 == iterations;
        tasks.spawn(async move {
            let _permit = permit;
            // Captures feed the next steps of this iteration only, over the row values.
            let mut captures = Vec::new();
            for (index, step) in steps.iter().enumerate() {
                if cancel.is_cancelled() {
                    return;
                }
                let variables = step
                    .variables
                    .clone()
                    .with_layer(row.iter().cloned())
                    .with_layer(captures.iter().cloned());
                let resolved = step.request.resolve(&variables);
                let outcome = tokio::select! {
                    outcome = send(&client, &resolved.request) => outcome,
                    _ = cancel.cancelled() => Err(SendError::Cancelled),
                };
                let (captured, assertions) = match &outcome {
                    Ok(response) => (
                        capture(&step.request.captures, response),
                        check(&resolved.request.assertions, response),
                    ),
                    Err(_) => (Vec::new(), Vec::new()),
                };
                store(&mut captures, &captured);
                let count = match &outcome {
                    Ok(response) if passed(response, &assertions) => Count::Succeeded,
                    Ok(_) => Count::Failed,
                    Err(_) => Count::Error,
                };
                if options.bail && !matches!(count, Count::Succeeded) {
                    cancel.cancel();
                }
                let _ = counts.send(count);
                let _ = results.send(StepResult {
                    iteration,
                    step: index,
                    request: resolved.request,
                    unresolved: resolved.unresolved,
                    outcome,
                    captured,
                    assertions,
                });
                let last = last_iteration && index + 1 == steps.len();
                if !options.delay.is_zero() && !last {
                    tokio::select! {
                        _ = tokio::time::sleep(options.delay) => {}
                        _ = cancel.cancelled() => return,
                    }
                }
            }
        });
    }
    drop(counts_tx);
    while tasks.join_next().await.is_some() {}

    let mut summary = RunSummary {
        succeeded: 0,
        failed: 0,
        errors: 0,
        skipped: 0,
        cancelled: cancel.is_cancelled(),
        elapsed: started.elapsed(),
    };
    while let Ok(count) = counts_rx.try_recv() {
        match count {
            Count::Succeeded => summary.succeeded += 1,
            Count::Failed => summary.failed += 1,
            Count::Error => summary.errors += 1,
        }
    }
    summary.skipped = total - summary.succeeded - summary.failed - summary.errors;
    summary
}

/// Streams, gRPC or server-sent events, end with the timeout and keep what was received.
async fn send(client: &HttpClient, request: &Request) -> Result<Response, SendError> {
    if crate::is_websocket_url(&request.url) {
        return Err(SendError::WebSocket(
            "WebSocket requests are opened from the app, they cannot be run".into(),
        ));
    }
    let timeout = client.options.timeout;
    let stop = async move {
        match timeout {
            Some(timeout) => tokio::time::sleep(timeout).await,
            None => std::future::pending().await,
        }
    };
    if crate::is_grpc_url(&request.url) {
        client.grpc_call(request, stop, |_| {}).await
    } else {
        client.send_streaming(request, stop, |_| {}).await
    }
}

enum Count {
    Succeeded,
    Failed,
    Error,
}
