use std::sync::mpsc::{Receiver, Sender, channel};

use cornu_core::runner::{CancelToken, RunPlan, RunSummary, StepResult};
use cornu_core::{ClientOptions, CookieJar, HttpClient, Request, Response, SendError, TokenStore};
use eframe::egui;
use tokio::runtime::Runtime;
use tokio::task::AbortHandle;

pub type DocId = u64;

pub struct Completed {
    pub doc: DocId,
    pub result: Result<Response, SendError>,
}

pub enum RunEvent {
    Result(Box<StepResult>),
    /// Always the last event, after every result.
    Finished(RunSummary),
}

pub struct RunHandle {
    pub events: Receiver<RunEvent>,
    pub cancel: CancelToken,
}

pub struct Worker {
    runtime: Runtime,
    client: HttpClient,
    cookies: CookieJar,
    tokens: TokenStore,
    tx: Sender<Completed>,
    rx: Receiver<Completed>,
}

impl Worker {
    pub fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("cornu-worker")
            .enable_all()
            .build()?;
        let cookies = CookieJar::default();
        let tokens = TokenStore::default();
        let client = HttpClient::with_cookies(&ClientOptions::default(), cookies.clone())?
            .with_tokens(tokens.clone());
        let (tx, rx) = channel();
        Ok(Self {
            runtime,
            client,
            cookies,
            tokens,
            tx,
            rx,
        })
    }

    pub fn cookies(&self) -> &CookieJar {
        &self.cookies
    }

    pub fn tokens(&self) -> &TokenStore {
        &self.tokens
    }

    /// Later requests use `options`; on error the current client is kept.
    pub fn configure(&mut self, options: &ClientOptions) -> Result<(), SendError> {
        self.client = HttpClient::with_cookies(options, self.cookies.clone())?
            .with_tokens(self.tokens.clone());
        Ok(())
    }

    pub fn send(&self, ctx: &egui::Context, doc: DocId, request: Request) -> AbortHandle {
        let client = self.client.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.runtime
            .spawn(async move {
                let result = client.send(&request).await;
                let _ = tx.send(Completed { doc, result });
                ctx.request_repaint();
            })
            .abort_handle()
    }

    /// For background jobs that do not belong to a request tab.
    pub fn run<T, F>(&self, ctx: &egui::Context, job: impl FnOnce(HttpClient) -> F) -> Receiver<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.spawn(ctx, job).0
    }

    pub fn spawn<T, F>(
        &self,
        ctx: &egui::Context,
        job: impl FnOnce(HttpClient) -> F,
    ) -> (Receiver<T>, AbortHandle)
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let future = job(self.client.clone());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let task = self.runtime.spawn(async move {
            let _ = tx.send(future.await);
            ctx.request_repaint();
        });
        (rx, task.abort_handle())
    }

    pub fn start_run(&self, ctx: &egui::Context, plan: RunPlan) -> RunHandle {
        let (results_tx, mut results_rx) = tokio::sync::mpsc::unbounded_channel();
        let (events_tx, events) = channel();
        let cancel = CancelToken::default();
        let client = self.client.clone();
        let token = cancel.clone();
        let run = self
            .runtime
            .spawn(async move { cornu_core::runner::run(&client, plan, &token, results_tx).await });
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            while let Some(result) = results_rx.recv().await {
                if events_tx.send(RunEvent::Result(Box::new(result))).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            if let Ok(summary) = run.await {
                let _ = events_tx.send(RunEvent::Finished(summary));
            }
            ctx.request_repaint();
        });
        RunHandle { events, cancel }
    }

    pub fn completed(&self) -> impl Iterator<Item = Completed> + '_ {
        self.rx.try_iter()
    }
}
