use std::sync::mpsc::{Receiver, Sender, channel};

use cornu_core::{ClientOptions, HttpClient, Request, Response, SendError};
use eframe::egui;
use tokio::runtime::Runtime;
use tokio::task::AbortHandle;

pub type DocId = u64;

pub struct Completed {
    pub doc: DocId,
    pub result: Result<Response, SendError>,
}

pub struct Worker {
    runtime: Runtime,
    client: HttpClient,
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
        let client = HttpClient::new(&ClientOptions::default())?;
        let (tx, rx) = channel();
        Ok(Self {
            runtime,
            client,
            tx,
            rx,
        })
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

    /// For one-off downloads that do not belong to a request tab.
    pub fn fetch(
        &self,
        ctx: &egui::Context,
        request: Request,
    ) -> Receiver<Result<Response, SendError>> {
        let client = self.client.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let _ = tx.send(client.send(&request).await);
            ctx.request_repaint();
        });
        rx
    }

    pub fn completed(&self) -> impl Iterator<Item = Completed> + '_ {
        self.rx.try_iter()
    }
}
