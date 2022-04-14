use tokio::sync::broadcast::{Sender, Receiver, channel};
use tokio::sync::Mutex;
use gst::prelude::*;
use gst::Sample;
use std::collections::HashMap;
use std::sync::Arc;
use once_cell::sync::OnceCell;

static INSTANCE: OnceCell<Mutex<HashMap<String, SourceLink>>> = OnceCell::new();

struct SourceLinkInner {
    buffer: Mutex<Vec<Sample>>,
    tx: Mutex<Sender<Sample>>
}

#[derive(Clone)]
pub struct SourceLink(Arc<SourceLinkInner>);

impl SourceLink {
    pub fn new () -> SourceLink {
        let (tx, _) = channel(100);
        SourceLink(Arc::new(SourceLinkInner {
            buffer: Mutex::new(Vec::new()),
            tx: Mutex::new(tx)
        }))
    }

    fn link_list() -> &'static Mutex<HashMap<String, SourceLink>> {
        static INSTANCE: OnceCell<Mutex<HashMap<String, SourceLink>>> = OnceCell::new();
        INSTANCE.get_or_init(|| {
            Mutex::new(HashMap::new())
        })
    }

    pub async fn get_link(id: String) -> SourceLink {
        let mut links = Self::link_list().lock().await;

        if links.contains_key(&id) {
            links[&id].clone()
        } else {
            let link = Self::new();
            links.insert(id, link.clone());
            link
        }
    }

    pub async fn push(&self, sample: gst::Sample) {
        let mut buffer = self.0.buffer.lock().await;
        let mut tx = self.0.tx.lock().await;

        if let Some(buf) = sample.buffer() {
            if !buf.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                buffer.clear();
            }
        }
        buffer.push(sample.clone());
        tx.send(sample);
    }
 
    pub async fn add(&self) -> (Vec<Sample>, Receiver<Sample>) {
        let mut buffer = self.0.buffer.lock().await;
        let mut tx = self.0.tx.lock().await;
        let rx = tx.subscribe();
        (buffer.clone(), rx)
    }
}