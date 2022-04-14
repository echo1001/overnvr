use crate::Result;
use crate::track;
use crate::SourceLink;
use crate::source::Recording;
use crate::dvr::{DVRWeak, DVR};

use anyhow::bail;

use gst::prelude::*;
use gst_sys::GST_MAP_READ;
use gst_sys::gst_buffer_map;
use gst_sys::gst_buffer_unmap;
use mongodb::bson::Bson;
use mongodb::bson::oid::ObjectId;
use mongodb::options::FindOneOptions;
use once_cell::sync::OnceCell;
use serde::{Serialize, Deserialize};

use tokio::task::JoinHandle;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{Sender, channel};
use tokio::sync::watch;

use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidateInit};
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::media::Sample;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::policy::bundle_policy::RTCBundlePolicy;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::RTCPFeedback;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::rtp_transceiver::rtp_sender::RTCRtpSender;
use webrtc::track::track_local::TrackLocal;

use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;
use std::collections::HashMap;
use std::sync::Weak;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;

use futures_util::StreamExt;
use futures_util::SinkExt;
use warp::ws::{WebSocket, Message};

use futures::stream::TryStreamExt;
use mongodb::{bson::doc, options::FindOptions};

#[derive(Serialize, Deserialize)]
struct Info {
    id: String,
    startpts: u64,
    startrtp: u32
}

#[derive(Serialize, Deserialize)]
struct TrackInfo {
    pub id: String,
}

#[derive(Serialize, Deserialize)]
struct StartLive {
    pub id: String,
    pub stream: String,
    pub quality: String
}

#[derive(Serialize, Deserialize)]
struct StartPast {
    pub id: String,
    pub source: String,
    pub at: f64
}

#[derive(Serialize, Deserialize)]
enum ClientMessage {
    SDP(RTCSessionDescription),
    ICE(RTCIceCandidateInit),
    AddTrack(Vec<TrackInfo>),
    RemoveTrack(Vec<TrackInfo>),
    Live(StartLive),
    From(StartPast),
    StartInfo(Info)
}

struct CurrentPlayback(JoinHandle<Result<()>>);
impl Drop for CurrentPlayback {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct TrackInner {
    id: String,
    track: Arc<track::TrackLocalStaticSample>,
    current_future: Mutex<Option<CurrentPlayback>>,
    packet_future: JoinHandle<Result<()>>,
    ready: watch::Receiver<bool>,
    send_ready: Mutex<watch::Sender<bool>>,
    tx: Sender<ClientMessage>,
    sender: Arc<RTCRtpSender>
}

impl Drop for TrackInner {
    fn drop(&mut self) {
        self.packet_future.abort();
        println!("dead");
    }
}

#[derive(Clone)]
struct Track(Arc<TrackInner>);

struct InputPipeline {
    pipeline: gst::Pipeline
}

impl InputPipeline {
    async fn open(fname: &String, at: Option<std::time::Duration>) -> Result<(InputPipeline, gst_app::app_sink::AppSinkStream)> {
        let pipeline = gst::parse_launch("filesrc name=file ! matroskademux name=demux ! appsink sync=0 wait-on-eos=0 max-buffers=5 emit-signals=1 async=0 enable-last-sample=0 name=sink")?;
        let pipeline: gst::Pipeline = pipeline.downcast().unwrap();
        pipeline.by_name("file").unwrap().set_property("location", fname);
        let appsink: gst_app::AppSink = pipeline.by_name("sink").unwrap().downcast().unwrap();

        let demux = pipeline.by_name("demux").unwrap();

        let mut stream = appsink.stream();

        pipeline.call_async_future(|pipeline| {
            pipeline.set_state(gst::State::Playing);
        }).await;

        if let Some(d) = at {
            stream.next().await;
            
            let mut flags = gst::SeekFlags::empty();
            flags.insert(gst::SeekFlags::KEY_UNIT);
            flags.insert(gst::SeekFlags::SNAP_BEFORE);
            flags.insert(gst::SeekFlags::FLUSH);

            let time: gst::ClockTime = gst::ClockTime::try_from(d)?;

            demux.seek_simple(flags, time);
        }

        Ok((InputPipeline { 
            pipeline
        }, stream))
    }
}

impl Drop for InputPipeline {
    fn drop(&mut self) {
        self.pipeline.set_state(gst::State::Null);
        println!("Shutdown Input");
    }
}
struct Player {
    pipeline: gst::Pipeline,
    input_task: JoinHandle<Result<()>>
}

impl Player {
    async fn open(source: String, at: std::time::Duration, dvr: DVRWeak) -> Result<(Player, gst_app::app_sink::AppSinkStream)> {
        let pipeline = gst::parse_launch("appsrc emit-signals=1 name=src ! h264parse ! video/x-h264,stream-format=byte-stream,alignment=au ! appsink sync=1 wait-on-eos=0 max-buffers=5 emit-signals=1 async=0 enable-last-sample=0 name=sink")?;
        let pipeline: gst::Pipeline = pipeline.downcast().unwrap();
        let appsink: gst_app::AppSink = pipeline.by_name("sink").unwrap().downcast().unwrap();
        let appsrc: gst_app::AppSrc = pipeline.by_name("src").unwrap().downcast().unwrap();
        let mut sink = appsrc.sink();
        let src = appsink.stream();

        //let inputTask 
        pipeline.call_async_future(|pipeline| {
            pipeline.set_state(gst::State::Playing);
        }).await;

        

        let input_task = tokio::spawn(async move {

            let mut cursor = if let Some(dvr) = dvr.upgrade() {
                let recordings = dvr.database.collection::<Recording>("recordings");
                let source = ObjectId::from_str(source.as_str())?;
                
                
                let recording_filter = doc! {
                    "start": {
                        "$lte": (at.as_nanos() as i64)
                    },
                    "source": source
                };
                let find_options = FindOneOptions::builder().sort(doc! { "start": -1 as i32 }).build();

                let recording = recordings.find_one(recording_filter, find_options).await?;
                if let Some(recording) = recording {
                    let recording_filter = doc! {
                        "start": {
                            "$gte": recording.start as i64
                        },
                        "source": source
                    };
                    let find_options = FindOptions::builder().sort(doc! { "start": 1 as i32 }).build();
                    recordings.find(recording_filter, find_options).await?
                    
                } else {
                    return Result::<()>::Ok(());
                }
            } else {
                return Result::<()>::Ok(());
            };

            while let Some(recording) = cursor.try_next().await? {
                let id = recording._id.clone();
                let start = recording.start;
                
                let file_start = std::time::Duration::from_nanos(recording.start as u64);

                let (pipeline, mut r) = InputPipeline::open(&recording.file, if at >= file_start { Some(at - file_start)} else {None}).await?;
                
                while let Some(s) = r.next().await {
                    if let Some(buffer) = s.buffer() {
                        let mut buf = buffer.to_owned();
                        let buffer = buf.get_mut().unwrap();
                        if let Some(pts) = buffer.pts() {
                            let pts: std::time::Duration = pts.into();
                            if pts < at {
                                if buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                                    continue;
                                }
                                let pts = gst::ClockTime::try_from(std::time::Duration::from_secs(0))?;
                                buffer.set_pts(pts);

                            } else {
                                let pts = gst::ClockTime::try_from(pts - at)?;
                                buffer.set_pts(pts);
                            }
                            let sample = gst::Sample::builder()
                                .buffer(&buf)
                                .caps(&s.caps().unwrap().to_owned())
                                .build();
                            
                            sink.send(sample).await; 
                        }
                    }
                }
            }
            Result::<()>::Ok(())
        });

        Ok((Player {
            pipeline,
            input_task
        }, src))
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.pipeline.set_state(gst::State::Null);
        self.input_task.abort();
        println!("Shutdown Player");
    }
}

pub async fn export(source: ObjectId, from: std::time::Duration, to: std::time::Duration, dvr: &DVR) -> Result<Vec<u8>> {

        
        let mut cursor = {
            let recordings = dvr.database.collection::<Recording>("recordings");            
            
            let recording_filter = doc! {
                "start": {
                    "$lte": (from.as_nanos() as i64)
                },
                "source": source
            };
            let find_options = FindOneOptions::builder().sort(doc! { "start": -1 as i32 }).build();

            let recording = recordings.find_one(recording_filter, find_options).await?;
            if let Some(recording) = recording {
                let recording_filter = doc! {
                    "$and": [
                        {
                            "start": {
                                "$gte": recording.start as i64
                            },
                        },
                        {
                            "start": {
                                "$lte": to.as_nanos() as i64
                            },
                        }
                    ],
                    "source": source
                };
                let find_options = FindOptions::builder().sort(doc! { "start": 1 as i32 }).build();
                recordings.find(recording_filter, find_options).await?
                
            } else {
                bail!("No recordings found");
            }
        };

        let pipeline = gst::parse_launch("appsrc emit-signals=1 name=src ! h264parse ! video/x-h264 ! mux.video_0 mp4mux name=mux ! filesink async=false name=sink location=out.mkv")?;
        let pipeline: gst::Pipeline = pipeline.downcast().unwrap();
        let appsrc: gst_app::AppSrc = pipeline.by_name("src").unwrap().downcast().unwrap();
        let mut sink = appsrc.sink();
        
        let bus = pipeline.bus().unwrap();
        let mut fname = dvr.config.storage.recordings.clone();
        fname.push(format!("{}.mp4", ObjectId::new()));
        pipeline.by_name("sink").unwrap().set_property("location", fname.to_str().to_value());


        pipeline.call_async_future(|pipeline| {
            pipeline.set_state(gst::State::Playing);
        }).await;
        let mut base = None;

        while let Some(recording) = cursor.try_next().await? {
            let id = recording._id.clone();
            let start = recording.start;
            
            let file_start = std::time::Duration::from_nanos(recording.start as u64);

            let (pipeline, mut r) = InputPipeline::open(&recording.file, if from >= file_start { Some(from - file_start)} else {None}).await?;
            
            while let Some(s) = r.next().await {
                if let Some(buffer) = s.buffer() {
                    if let Some(pts) = buffer.pts() {
                        let mut buf = buffer.to_owned();
                        let buffer = buf.get_mut().unwrap();
                        if let Some(pts) = buffer.pts() {
                            let pts: std::time::Duration = pts.into();
                            if pts > to {
                                break;
                            }

                            let base = if let Some(base) = base {
                                base
                            } else {
                                base.replace(pts);
                                pts
                            };

                            let pts = gst::ClockTime::try_from(pts - base)?;
                            buffer.set_pts(pts);

                            let sample = gst::Sample::builder()
                                .buffer(&buf)
                                .caps(&s.caps().unwrap().to_owned())
                                .build();
                            
                            sink.send(sample).await; 
                        }
                    }
                }
            }
        }
        appsrc.end_of_stream()?;

        bus.timed_pop_filtered(None, &[gst::MessageType::Eos, gst::MessageType::Error]);

        pipeline.set_state(gst::State::Null)?;
        //pipeline.state(None);

        //bus.timed_pop_filtered(std::time::Duration::from_secs(1), &[gst::MessageType::Eos, gst::MessageType::Error]);

        let f = tokio::fs::read(&fname).await?;
        tokio::fs::remove_file(&fname).await?;
        Ok(f)
}

impl Track {

    async fn start_playback(&self, dvr: DVRWeak, mut rn: watch::Receiver<bool>, source: String, at: std::time::Duration) {
        let mut current_future = self.0.current_future.lock().await;
        current_future.take();

        let track = self.0.track.clone();
        let mut ready = self.0.ready.clone();
        let id = self.0.id.clone();
        let tx = self.0.tx.clone();

        let f = tokio::spawn(async move {
            {
                if *ready.borrow() == false {
                    ready.changed().await?;
                }
            }
            if *rn.borrow() == false {
                rn.changed().await?;
            }
            track.rebase().await;

            let (p, mut s) = Player::open(source, at, dvr.clone()).await?;

            let info = Info{
                id: id.clone(),
                startpts: at.as_nanos() as u64,
                startrtp: track.get_timestamp().await
            };
            
            if let Err(_) = tx.send(ClientMessage::StartInfo(info)).await {
                return Ok(());
            }


            let mut sent_one = false;
            let mut last_duration = None;
            let mut first_one: Option<std::time::Duration> = None;
            while let Some(s) = s.next().await {

                let buf = s.buffer().unwrap();
                let v = buf.map_readable()?.to_vec();

                let duration = buf.pts();
                /*println!("{:?}", duration);*/
                let duration_expanded;
                if let Some(duration) = duration {
                    duration_expanded = duration;
                    last_duration = Some(duration);
                } else if let Some(duration) = last_duration {
                    duration_expanded = duration;
                } else {
                    continue;
                }

                //let actual_duration: std::time::Duration;

                let duration: std::time::Duration = duration_expanded.into();
                //actual_duration = duration - at;

                track
                    .write_sample(&Sample {
                        data: bytes::Bytes::from(v),
                        duration,
                        ..Default::default()
                    })
                    .await?; 
            }
            Result::<()>::Ok(())
        });

        current_future.replace(CurrentPlayback(f));
    }

    async fn start_live(&self, dvr: DVRWeak, mut rn: watch::Receiver<bool>, source: String) {
        let mut current_future = self.0.current_future.lock().await;
        current_future.take();

        let source_link = SourceLink::get_link(source).await;
        let track = self.0.track.clone();
        let mut ready = self.0.ready.clone();
        let id = self.0.id.clone();
        let tx = self.0.tx.clone();

        let f = tokio::spawn(async move {
            {
                if *ready.borrow() == false {
                    ready.changed().await?;
                }
            }
            println!("ready1");
            if *rn.borrow() == false {
                rn.changed().await?;
            }
            println!("ready2");
            track.rebase().await;

            let (b, mut r) = source_link.add().await;
            println!("Started");

            for s in b {
                let buf = s.buffer().unwrap().to_owned();
                let map = buf.map_readable()?;
                let v = map.to_vec();

                if let Some(pts) = buf.pts() {
                    track
                        .write_sample(&Sample {
                            data: bytes::Bytes::from(v),
                            duration: pts.into(),
                            ..Default::default()
                        })
                        .await?;
                    break;

                } 
            }
            track.rebase().await;
            
            let mut sent_one = false;
            let mut last_duration = None;
            let mut first_one: Option<std::time::Duration> = None;
            while let Ok(s) = r.recv().await {

                let buf = s.buffer().unwrap().to_owned();
                let map = buf.map_readable()?;
                let v = map.to_vec();

                let duration = buf.pts();
                let duration_expanded;
                if let Some(duration) = duration {
                    duration_expanded = duration;
                    last_duration = Some(duration);
                } else if let Some(duration) = last_duration {
                    duration_expanded = duration;
                } else {
                    continue;
                }

                let actual_duration: std::time::Duration;

                if let Some(first_one) = first_one {
                    let duration: std::time::Duration = duration_expanded.into();
                    actual_duration = duration - first_one;
                } else {
                    first_one = Some(duration_expanded.into());
                    actual_duration = std::time::Duration::from_secs(0);
                }
                

                track
                    .write_sample(&Sample {
                        data: bytes::Bytes::from(v),
                        duration: actual_duration,
                        ..Default::default()
                    })
                    .await?; 

                if !sent_one {
                    let pts = duration_expanded;
                    let pts: u64 = pts.nseconds();

                    let info = Info{
                        id: id.clone(),
                        startpts: pts,
                        startrtp: track.get_timestamp().await
                    };
                    
                    if let Err(_) = tx.send(ClientMessage::StartInfo(info)).await {
                        break;
                    }
                    sent_one = true;

                }
            }

            Result::<()>::Ok(())
        });

        current_future.replace(CurrentPlayback(f));
    }
}

pub struct ClientInner {
    peer_connection: Arc<RTCPeerConnection>,
    tracks: Mutex<HashMap<String, Track>>,
    tx: Sender<ClientMessage>,
    ready: watch::Receiver<bool>,
    dvr: DVRWeak
}

pub struct Client(Arc<ClientInner>);


#[derive(Clone)]
pub struct ClientWeak(Weak<ClientInner>);

impl ClientWeak {
    fn upgrade(&self) -> Option<Client> {
        self.0.upgrade().map(|c| Client(c))
    }
}

impl Deref for Client {
    type Target = ClientInner;
    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl Client {
    fn downgrade(&self) -> ClientWeak {
        ClientWeak(Arc::downgrade(&self.0))
    }

    async fn negotiate(&self) -> Result<()> {
        println!("Negotiate");
        let offer = self.peer_connection.create_offer(None).await?;

        self.peer_connection.set_local_description(offer).await?;

        if let Some(local_desc) = self.peer_connection.local_description().await {
            let _ = self.tx.send(ClientMessage::SDP(local_desc)).await;
        }

        Ok(())
    }

    async fn handle_reply(&self, reply: RTCSessionDescription) -> Result<()> {
        self.peer_connection.set_remote_description(reply).await?;
        let tracks = self.tracks.lock().await;
        for v in tracks.values() {
            v.0.send_ready.lock().await.send(true)?;
        }
        println!("Sent ready");

        Ok(())
    }

    async fn handle_ice(&self, candidate: RTCIceCandidateInit) -> Result<()> {
        self.peer_connection.add_ice_candidate(candidate).await?;
        Ok(())
    }

    async fn add_track(&self, new_tracks: Vec<TrackInfo>) -> Result<()> {
        let mut tracks = self.tracks.lock().await;

        for t in new_tracks {

            let video_rtcp_feedback = vec![
                RTCPFeedback {
                    typ: "goog-remb".to_owned(),
                    parameter: "".to_owned(),
                },
                RTCPFeedback {
                    typ: "ccm".to_owned(),
                    parameter: "fir".to_owned(),
                },
                RTCPFeedback {
                    typ: "nack".to_owned(),
                    parameter: "".to_owned(),
                },
                RTCPFeedback {
                    typ: "nack".to_owned(),
                    parameter: "pli".to_owned(),
                },
            ];

            let video_track = Arc::new(track::TrackLocalStaticSample::new(
                RTCRtpCodecCapability {
                    mime_type: MIME_TYPE_H264.to_owned(),
                    rtcp_feedback: video_rtcp_feedback.clone(),
                    ..Default::default()
                },
                "video".to_owned(),
                t.id.clone(),
            ));
            
            let rtp_sender = self.peer_connection
                .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>)
                .await?;
    
            let (s, r) = watch::channel(false);

            let f = tokio::spawn({let rtp_sender = rtp_sender.clone(); async move {
                let mut rtcp_buf = vec![0u8; 1500];
                while let Ok((p, a)) = rtp_sender.read_rtcp().await {
                    for p in p {
                        //println!("{:?}", p);
                    }
                }
                Result::<()>::Ok(())
            }});

            let track = Track(Arc::new(TrackInner {
                id: t.id.clone(),
                track: video_track,
                current_future: Mutex::new(None),
                packet_future: f,
                ready: r,
                send_ready: Mutex::new(s),
                tx: self.tx.clone(),
                sender: rtp_sender
            }));
        
    
            tracks.insert(t.id, track);

        }

        Ok(())
    }

    async fn remove_track(&self, remove_tracks: Vec<TrackInfo>) -> Result<()> {
        let mut tracks = self.tracks.lock().await;

        for t in remove_tracks {
            if let Some(track) = tracks.remove(&t.id) {
                self.peer_connection.remove_track(&track.0.sender).await?;
            }
        }
        Ok(())
    }



    async fn get_api() -> Result<Arc<webrtc::api::API>> {

        static INSTANCE: OnceCell<Mutex<Option<Arc<webrtc::api::API>>>> = OnceCell::new();

        let instance = INSTANCE.get_or_init(|| {
            Mutex::new(None)
        });

        let mut instance = instance.lock().await;

        if let Some(api) = &*instance {
            Ok(api.clone())
        } else {
            let mut m = MediaEngine::default();
    
            m.register_default_codecs()?;
        
            let mut registry = Registry::new();
        
            registry = register_default_interceptors(registry, &mut m)?;
        
            let api = APIBuilder::new()
                .with_media_engine(m)
                .with_interceptor_registry(registry)
                .build();
            
            let api = Arc::new(api);
            instance.replace(api.clone());

            Ok(api)
        }
    }

    pub async fn start(ws: WebSocket, dvr: DVRWeak) -> Result<()> {
        let (tm, mut rm) = channel(10);
        let (sn, rn) = watch::channel(false);

        let api = Self::get_api().await?;
    
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                ..Default::default()
            }],
            bundle_policy: RTCBundlePolicy::MaxBundle,
            ..Default::default()
        };
    
        let peer_connection = Arc::new(api.new_peer_connection(config).await?);

        peer_connection.on_ice_candidate({
            let tm = tm.clone();
            Box::new(move |candidate| {
                let tm = tm.clone();
                Box::pin(async move {
                    if let Some(candidate) = candidate {
                        if let Ok(candidate) = candidate.to_json().await {
                            let _ = tm.send(ClientMessage::ICE(candidate)).await;

                        }
                    }
                })
            }
            )
        }).await;


        peer_connection.on_peer_connection_state_change({
            Box::new(move |state| {
                println!("{:?}", state);
                match state {
                    RTCPeerConnectionState::Connected => {
                        let _ = sn.send(true);
                        println!("Sent connected")
                    }
                    _ => {}
                };
                Box::pin(async move {
                })
            }
            )
        }).await;



        let (mut t, mut r) = ws.split();
        
        let c = Client(Arc::new(ClientInner{
            peer_connection: peer_connection.clone(),
            tracks: Mutex::new(HashMap::new()),
            tx: tm,
            ready: rn,
            dvr
        }));

        let weak = c.downgrade();
        c.peer_connection.on_negotiation_needed({
            Box::new(move || {
                let weak = weak.clone();
                Box::pin(async move {
                    if let Some(c) = weak.upgrade() {
                        c.negotiate().await;
                    }
                })
            })
        }).await;

        let sender = tokio::spawn({
            async move {
                while let Some(m) = rm.recv().await {
                    t.send(Message::text(serde_json::to_string(&m)?)).await?;
                }
                Result::<()>::Ok(())
            }
        });
        
        tokio::spawn(async move {
            while let Some(m) = r.next().await {
                if let Ok(m) = m {
                    //println!("{:?}", m);
    
                    if m.is_close() {
                        break;
                    }
                    if m.is_text() {
                        if let Ok(t) = m.to_str() {
                            if let Ok(m) = serde_json::from_str(t) {
                                match m {
                                    ClientMessage::SDP(s) => {
                                        if let Err(err) = c.handle_reply(s).await {
                                            println!("{:?}", err);
                                        }
                                    }
                                    
                                    ClientMessage::ICE(candidate) => {
                                        if let Err(err) = c.handle_ice(candidate).await {
                                            println!("{:?}", err);
                                            break;
                                        }

                                    }
                                    ClientMessage::AddTrack(s) => {
                                        if let Err(err) = c.add_track(s).await {
                                            println!("{:?}", err);
                                            break;

                                        }
                                    }
                                    ClientMessage::RemoveTrack(s) => {
                                        if let Err(err) = c.remove_track(s).await {
                                            println!("{:?}", err);
                                            break;

                                        }
                                        println!("Removed Track");
                                    }
                                    ClientMessage::Live(s) => {
                                        let tracks = c.tracks.lock().await;
                                        if tracks.contains_key(&s.id) {
                                            tracks[&s.id].start_live(c.dvr.clone(), c.ready.clone(), format!("{}_{}", s.stream, s.quality)).await;
                                        }
                                    }
                                    ClientMessage::From(s) => {
                                        let tracks = c.tracks.lock().await;
                                        if tracks.contains_key(&s.id) {
                                            tracks[&s.id].start_playback(c.dvr.clone(), c.ready.clone(), s.source, std::time::Duration::from_millis(s.at as u64)).await;
                                        }
                                    }
                                    
                                    _ => ()
                                }
                            }
                        }
                    }

                }
            }
            println!("Closing");
            let mut tracks = c.tracks.lock().await;
            tracks.clear();
            if let Err(e) = c.peer_connection.close().await {
                println!("{:?}", e);
            }
            sender.abort();
            println!("Closed");

            Result::<()>::Ok(())

        });

        Ok(())
    }
}
