use crate::dvr::{DVRWeak, DVR};
use crate::{Result, surface::Surface};
use chrono::{Utc, DateTime};
use futures::FutureExt;
use futures::future::BoxFuture;
use glib::clone::Upgrade;
use serde::{Serialize, Deserialize};
use tokio::io::AsyncWriteExt;
use tokio::{sync::Mutex, task::JoinHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{str::FromStr, collections::HashMap};
use std::sync::{Arc, Weak};
use mongodb::bson::oid::ObjectId;
use mongodb::bson::{doc, Bson};

use gst::prelude::*;

use gst_app::{AppSink, AppSrc};

use futures_util::StreamExt;

mod source_link;
pub use source_link::SourceLink;

#[derive(Serialize, Deserialize)]
pub struct Stream {
    pub url: String,
    pub detect: bool
}

//impl Hash

#[derive(Serialize, Deserialize)]
pub struct SourceConfig {
    pub _id: ObjectId,
    pub name: String,
    pub detect: bool,
    pub enable: bool,
    pub streams: HashMap<String, Stream>
}

#[derive(Serialize, Deserialize)]
pub struct Recording {
    pub _id: ObjectId,
    pub source: ObjectId,
    pub file: String, 
    pub start: u64,
    pub end: u64,
    pub size: u64
}

pub struct PipelineHolder {
    pipeline: gst::Pipeline,
    image_task: Option<JoinHandle<Result<()>>>,
    raw_task: Option<JoinHandle<Result<()>>>,
    bus_task: Option<JoinHandle<Result<()>>>,

}

impl PipelineHolder {
    async fn start(&self) -> Result<()> {
        tokio::task::spawn_blocking({
            let pipeline = self.pipeline.clone();
            move || {
                pipeline.set_state(gst::State::Playing)?;
                Result::<()>::Ok(())
            }
        }).await?
    }
}

impl Drop for PipelineHolder {
    fn drop(&mut self) {
        let pipeline = self.pipeline.clone();
        tokio::task::spawn_blocking(move || {
            let _ = pipeline.set_state(gst::State::Null);
            println!("shutdown");
        });

        if let Some(task) = self.image_task.take() {
            task.abort();
        }

        if let Some(task) = self.raw_task.take() {
            task.abort();
        }

        if let Some(task) = self.bus_task.take() {
            task.abort();
        }
    }
}
pub struct SourceInner {
    pub pipeline: Mutex<Option<PipelineHolder>>,
    pub _id: ObjectId,
    pub stream_name: String,
    pub url: String,
    pub detect: bool,
    running: AtomicBool,
    dvr: DVRWeak,
    pub snapshot: Option<Surface>,
}

impl Drop for SourceInner {
    fn drop(&mut self) {
        println!("dead");
    }
}

pub struct Source {
    pub inner: Arc<SourceInner>,
}

const STREAM_ONLY: &str = r##"
    rtspsrc name=src latency=2000 tcp-timeout=2000000 buffer-mode=3
    src. ! queue ! rtph264depay ! queue ! tee name=raw_tee

    raw_tee. ! queue max-size-buffers=30 leaky=downstream ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au ! 
        appsink name=raw_sink max_buffers=1 drop=0 sync=0 emit-signals=1 async=0 enable-last-sample=0 wait-on-eos=0
"##;
// ! videorate max-rate=5

#[cfg(target_arch = "aarch64")]
const DECODE: &str = r##"
    raw_tee. ! queue max-size-buffers=30 leaky=downstream ! h264parse ! nvv4l2decoder bufapi-version=true enable-max-performance=true num-extra-surfaces=20 ! queue ! videorate max-rate=5 !
    appsink name=video_sink max_buffers=1 drop=1 sync=0 emit-signals=1 async=0 enable-last-sample=0 wait-on-eos=0

    splitmuxsink max-size-bytes=1073741824 name=rec muxer=matroskamux
    raw_tee. ! queue max-size-buffers=30 leaky=downstream ! h264parse config-interval=-1 ! rec.video
"##;

#[cfg(not(target_arch = "aarch64"))]
const DECODE: &str = r##"
    raw_tee. ! queue max-size-buffers=30 leaky=downstream ! h264parse ! nvv4l2decoder num-extra-surfaces=20 ! queue ! videorate max-rate=5 !
    appsink name=video_sink max_buffers=1 drop=1 sync=0 emit-signals=1 async=0 enable-last-sample=0 wait-on-eos=0

    splitmuxsink max-size-bytes=1073741824 name=rec muxer=matroskamux
    raw_tee. ! queue max-size-buffers=30 leaky=downstream ! h264parse config-interval=-1 ! rec.video
"##;
    //2147483648
    //


#[derive(Clone)]
pub struct SourceWeak (Weak<SourceInner>);

struct RestartToken(SourceWeak);
impl Drop for RestartToken {
    fn drop(&mut self) {
        let a = Source::restart(self.0.clone());
        tokio::spawn(async move {
            let _ = a.await;
        });
    }
}

impl SourceWeak {
    pub fn upgrade(&self) -> Option<Source> {
        self.0.upgrade().map(|o| Source{inner:o})
    }
}

impl Source {

    pub fn downgrade(&self) -> SourceWeak {
        SourceWeak(Arc::downgrade(&self.inner))
    }

    pub async fn new(_id: &ObjectId, stream_name: &String, url: &String, detect: bool, dvr: DVRWeak) -> Result<Source> {
        
        Ok(Source { 
            inner: Arc::new(SourceInner {
                pipeline: Mutex::new(None),
                _id: _id.clone(),
                url: url.clone(),
                stream_name: stream_name.clone(),
                detect,
                running: AtomicBool::new(false),
                dvr,
                snapshot: if detect {Some(Surface::new(1280, 720, false).await?)} else {Default::default()}
            })
        })
    }

    pub async fn stop(&self) {
        self.inner.running.store(false, Ordering::SeqCst);
        let mut pipelinelock = self.inner.pipeline.lock().await;
        drop(pipelinelock.take());
    }

    pub fn restart(self_weak: SourceWeak) -> BoxFuture<'static, Result<()>> {
        
        async move {
            if let Some(this) = self_weak.upgrade() {
                if !this.inner.running.load(Ordering::SeqCst) {
                    return Result::<()>::Ok(());
                }

                println!("Stopping {}", this.inner._id);
                this.stop().await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                println!("Starting {}", this.inner._id);
                this.run().await?;

                println!("Done");
            }
            Result::<()>::Ok(())
        }.boxed()
    }

    pub async fn run(&self) -> Result<()> {
        self.inner.running.store(true, Ordering::SeqCst);

        let mut inner_pipline = self.inner.pipeline.lock().await;
        let mut pipeline_string = STREAM_ONLY.to_owned();

        if self.inner.detect {
            pipeline_string.push_str(DECODE);
        }

        let pipeline: gst::Pipeline = tokio::task::spawn_blocking(move || {
            Result::<gst::Pipeline>::Ok(gst::parse_launch(pipeline_string.as_str())?.downcast().unwrap())
        }).await??;

        let clock = gst::SystemClock::obtain();
        clock.set_property_from_str("clock-type", "0");
        pipeline.set_clock(Some(&clock))?;
        pipeline.set_start_time(None);

        let mut bus_stream = pipeline.bus().unwrap().stream();

        let raw_appsink: AppSink = pipeline.by_name("raw_sink").unwrap().downcast().unwrap();
        let mut raw_stream = raw_appsink.stream();

        pipeline.by_name("src").unwrap().set_property("location", self.inner.url.clone());

        let mut holder = PipelineHolder {
            pipeline: pipeline.clone(),
            image_task: None,
            raw_task: None,
            bus_task: None,
        };

        holder.start().await?;
        let id = self.inner._id.clone();
        let weak_self = self.downgrade();
        if self.inner.detect {
            let image_appsink: AppSink = pipeline.by_name("video_sink").unwrap().downcast().unwrap();
            let mut image_stream = image_appsink.stream();
            if let Some(dvr) = self.inner.dvr.upgrade() {
                let link = vec![
                    dvr.detector.add_source(&weak_self).await,
                ];

                holder.image_task.replace(tokio::spawn({
                    let weak_self = weak_self.clone();
                    let to = std::time::Duration::from_secs(10);

                    async move {
                        let token = RestartToken(weak_self.clone());
                        let mut last_snapshot = std::time::Instant::now();
                        while let Some(s) = tokio::time::timeout(to, image_stream.next()).await? {
                            let ts = std::time::Instant::now();
                            for l in &link {
                                l.send_frame(&s).await;
                            }
                            
                            if (ts - last_snapshot).as_millis() >= 1000 {
                                //println!("{:?}", s.caps());
                                if let Some(buffer) = s.buffer() {
                                    if let Some(me) = weak_self.upgrade() {
                                        if let Some(surf) = &me.inner.snapshot {
                                            let f = Surface::from_buffer(buffer.to_owned()).await?;
                                            f.place(surf).await?;
                                            last_snapshot = ts;

                                        }
                                    }
                                }
                            }
                        }
                        Result::<()>::Ok(())
                    }
                }));
            }
    
            let rec: gst::Element = pipeline.by_name("rec").unwrap();
            rec.connect("format-location-full", false, {
                let weak_self = weak_self.clone();
                move |v| {
                    let sample: gst::Sample = v[2].get().expect("We didn't get a sample :(");
                    if let Some(buffer) = sample.buffer() {
                        if let Some(pts) = buffer.pts() {
                            let d: std::time::Duration = pts.into();
                            let t = std::time::UNIX_EPOCH + d;
                            let datetime: DateTime<Utc> = t.into();

                            if let Some(this) = weak_self.upgrade() {
                                if let Some(dvr) = this.inner.dvr.upgrade() {
                                    let mut path = dvr.config.storage.recordings.clone();
                                    path.push(this.inner._id.to_string());
                                    if let Err(e) = std::fs::create_dir_all(path.clone()) {
                                        println!("{:?}", e);
                                        return None
                                    }
        
                                    path.push(datetime.format("%Y%m%d_%H%M%S").to_string());
                                    path.set_extension("mkv");
        
                                    return path.to_str().map(|s| s.to_value());
                                }
                            }
                        }
                    }
                    None
                }
            });
        }



        let link = SourceLink::get_link(self.inner.stream_name.clone()).await;
        holder.raw_task.replace(tokio::spawn({
            let weak_self = weak_self.clone();
            async move {
                let token = RestartToken(weak_self);
                let to = std::time::Duration::from_secs(10);
                while let Some(s) = tokio::time::timeout(to, raw_stream.next()).await? {
                    link.push(s).await;
                }
                Result::<()>::Ok(())
            }
        }));

        holder.bus_task.replace(tokio::spawn({
            let weak_self = weak_self.clone();
            async move {
                while let Some(message) = bus_stream.next().await {
                    match message.view() {
                        gst::MessageView::Element(e) => {
                            let st = e.structure().unwrap();
                            let name = st.name();
                            if name == "splitmuxsink-fragment-opened" {
                                let location: String = st.get("location").unwrap();
                                let time: u64 = st.get("running-time").unwrap();
                                if let Some(this) = weak_self.upgrade() {
                                    if let Some(dvr) = this.inner.dvr.upgrade() {
                                        let recording = Recording {
                                            _id: ObjectId::new(),
                                            end: 0,
                                            file: location,
                                            size: 0,
                                            source: this.inner._id.clone(),
                                            start: time
                                        };

                                        dvr.database.collection::<Recording>("recordings").insert_one(&recording, None).await;
                                    }
                                }

                            }
                            if name == "splitmuxsink-fragment-closed" {
                                let location: String = st.get("location").unwrap();
                                let time: u64 = st.get("running-time").unwrap();
                                if let Some(this) = weak_self.upgrade() {
                                    if let Some(dvr) = this.inner.dvr.upgrade() {
                                        if let Ok(data) = std::fs::metadata(&location) {
                                            let file = doc! {
                                                "file": location
                                            };

                                            let update = doc! {
                                                "$set": {
                                                    "size": data.len() as i64,
                                                    "end": time as i64
                                                }
                                            };
                                            dvr.database.collection::<Recording>("recordings").update_one(file, update, None).await;
                                        }
                                    }
                                }

                            }
                        }
                        _ => ()
                    }
                }

                Result::<()>::Ok(())
            }
        }));

        inner_pipline.replace(holder);

        Ok(())
    }

}

/*impl Drop for Source {
    fn drop(&mut self) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            inner.pipeline.lock().await.take();
        });
    }
}*/