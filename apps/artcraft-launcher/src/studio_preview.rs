//! At most one composite in flight. Pointer events never wait for a raster render.
use crate::studio_document::Snapshot;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

pub struct Preview {
    pub image: photocraft_raster::Rgba8Image,
    pub document: photocraft_doc::Document,
}

type ReadyFrame = ([u64; 5], Result<Preview, String>);

pub struct Worker {
    jobs: SyncSender<Snapshot>,
    results: Receiver<ReadyFrame>,
    busy: bool,
}

pub fn render(snapshot: Snapshot) -> Result<Preview, String> {
    let display = snapshot.display.clone();
    let doc = snapshot.render()?;
    let image = display.to_rgba8(&photocraft_compose::render(&doc, photocraft_geom::Rect::from_size(doc.size)));
    Ok(Preview { image, document: doc })
}

impl Worker {
    pub fn new() -> Result<Self, String> {
        let (jobs, rx) = sync_channel::<Snapshot>(1);
        let (tx, results) = sync_channel(1);
        std::thread::Builder::new()
            .name("studio-composite".into())
            .spawn(move || {
                while let Ok(snapshot) = rx.recv() {
                    let key = snapshot.key;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(snapshot)))
                        .unwrap_or_else(|_| Err("Shared display rendering failed; native edits have been retained.".into()));
                    if tx.send((key, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { jobs, results, busy: false })
    }

    pub fn poll(&mut self) -> Result<Option<ReadyFrame>, String> {
        match self.results.try_recv() {
            Ok(result) => {
                self.busy = false;
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("Shared display worker stopped; native edits have been retained.".into()),
        }
    }

    pub fn submit(&mut self, snapshot: Snapshot) -> Result<(), String> {
        if !self.busy {
            self.jobs.try_send(snapshot).map_err(|e| e.to_string())?;
            self.busy = true;
        }
        Ok(())
    }
}
