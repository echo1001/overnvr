use crate::Result;
use crate::model::JsonDuration;
use crate::track;
use crate::SourceLink;
use crate::dvr::{DVRWeak, DVR};

use actix::WeakAddr;
use actix::prelude::*;
use actix_web::{middleware, web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_web_actors::ws;

use gst::prelude::*;
use mongodb::bson::oid::ObjectId;
use once_cell::sync::OnceCell;

use serde::{Serialize, Deserialize};

use tokio::task::JoinHandle;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::sync::watch;

use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidateInit};
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
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::SinkExt;

use futures::stream::TryStreamExt;
use mongodb::{bson::doc};

#[derive(Serialize, Deserialize)]
struct Info {
    id: String,
    startpts: JsonDuration,
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
    pub at: JsonDuration
}

#[derive(Serialize, Deserialize, Message)]
#[rtype(result = "()")]
enum ClientMessage {
    SDP(RTCSessionDescription),
    ICE(RTCIceCandidateInit),
    AddTrack(Vec<TrackInfo>),
    RemoveTrack(Vec<TrackInfo>),
    Live(StartLive),
    From(StartPast),
    StartInfo(Info)
}

#[derive(Serialize, Deserialize, Message)]
#[rtype(result = "()")]
enum ServerMessage {
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
    addr: WeakAddr<ClientHandler>,
    sender: Arc<RTCRtpSender>
}

impl Drop for TrackInner {
    fn drop(&mut self) {
        self.packet_future.abort();
    }
}

#[derive(Clone)]
struct Track(Arc<TrackInner>);

struct InputPipeline {
    pipeline: gst::Pipeline
}

impl InputPipeline {
    async fn open(fname: &String, at: Option<JsonDuration>) -> Result<(InputPipeline, gst_app::app_sink::AppSinkStream)> {
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

            let d: Duration = d.into();
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
    }
}
struct Player {
    pipeline: gst::Pipeline,
    input_task: JoinHandle<Result<()>>
}

impl Player {
    async fn open(source: String, at: JsonDuration, dvr: DVRWeak) -> Result<(Player, gst_app::app_sink::AppSinkStream)> {
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
                let s = FromStr::from_str(source.as_str())?;
                dvr.database.get_recordings( &s, &at, None).await?
            } else {
                return Result::<()>::Ok(());
            };

            while let Some(recording) = cursor.try_next().await? {
                let id = recording._id.clone();
                let start = recording.start;
                
                let file_start = recording.start;

                let (pipeline, mut r) = InputPipeline::open(&recording.file, if at >= file_start { Some(at - file_start)} else {None}).await?;
                
                while let Some(s) = r.next().await {
                    if let Some(buffer) = s.buffer() {
                        let mut buf = buffer.to_owned();
                        let buffer = buf.get_mut().unwrap();
                        if let Some(pts) = buffer.pts() {
                            let pts: Duration = pts.into();
                            let at: Duration = at.into();

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
    }
}

pub async fn export(source: ObjectId, from: JsonDuration, to: JsonDuration, dvr: &DVR) -> Result<Vec<u8>> {

        
        let mut cursor = 
            dvr.database.get_recordings(&source, &from, Some(&to)).await?;

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
            
            let file_start = recording.start;

            let (pipeline, mut r) = InputPipeline::open(&recording.file, if from >= file_start { Some(from - file_start)} else {None}).await?;
            
            while let Some(s) = r.next().await {
                if let Some(buffer) = s.buffer() {
                    if let Some(pts) = buffer.pts() {
                        let mut buf = buffer.to_owned();
                        let buffer = buf.get_mut().unwrap();
                        if let Some(pts) = buffer.pts() {
                            let pts: std::time::Duration = pts.into();
                            if to < pts {
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

    async fn start_playback(&self, dvr: DVRWeak, mut rn: watch::Receiver<bool>, source: String, at: JsonDuration) {
        let mut current_future = self.0.current_future.lock().await;
        current_future.take();

        let track = self.0.track.clone();
        let mut ready = self.0.ready.clone();
        let id = self.0.id.clone();
        let tx = self.0.addr.clone();

        let f = tokio::spawn(async move {
            {
                if *ready.borrow() == false {
                    ready.changed().await?;
                }
            }
            if *rn.borrow() == false {
                rn.changed().await?;
            }
            track.wait().await;
            track.rebase().await;

            let (p, mut s) = Player::open(source, at, dvr.clone()).await?;

            let info = Info{
                id: id.clone(),
                startpts: at.into(),
                startrtp: track.get_timestamp().await
            };
            
            if let Some(tx) = tx.upgrade() {
                if let Err(_) = tx.send(ServerMessage::StartInfo(info)).await {
                    return Ok(());
                }
            } else {
                return Ok(());
            }


            let sent_one = false;
            let mut last_duration = None;
            let first_one: Option<std::time::Duration> = None;
            while let Some(s) = s.next().await {

                let buf = s.buffer().unwrap();
                let v = buf.map_readable()?.to_vec();

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
        let tx = self.0.addr.clone();

        let f = tokio::spawn(async move {
            {
                if *ready.borrow() == false {
                    ready.changed().await?;
                }
            }
            
            if *rn.borrow() == false {
                rn.changed().await?;
            }
            
            track.wait().await;
            track.rebase().await;

            let (b, mut r) = source_link.add().await;
            

            for s in b {
                let buf = s.buffer().unwrap().to_owned();
                let map = buf.map_readable()?;
                let v = map.to_vec();

                //if let Some(pts) = buf.pts() {
                if !buf.flags().contains(gst::BufferFlags::DELTA_UNIT){
                    track
                        .write_sample(&Sample {
                            data: bytes::Bytes::from(v),
                            duration: std::time::Duration::from_secs(0),
                            ..Default::default()
                        })
                        .await?;

                    break;
                }

                //} 
            }

            track.wait().await;
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

                    let info = Info{
                        id: id.clone(),
                        startpts: pts.into(),
                        startrtp: track.get_timestamp().await
                    };

                    if let Some(tx) = tx.upgrade() {
                        if let Err(_) = tx.send(ServerMessage::StartInfo(info)).await {
                            return Ok(());
                        }
                    } else {
                        return Ok(());
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
    ready: watch::Receiver<bool>,
    dvr: DVRWeak,
    addr: WeakAddr<ClientHandler>
}

impl Drop for ClientInner {
    fn drop(&mut self) {
        let peer_connection = self.peer_connection.clone();
        tokio::spawn(async move {
            
            let _ = peer_connection.close().await;
        });
    }
}

#[derive(Clone)]
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
        
        let offer = self.peer_connection.create_offer(None).await?;

        self.peer_connection.set_local_description(offer).await?;

        if let Some(local_desc) = self.peer_connection.local_description().await {
            if let Some(addr) = self.addr.upgrade() {
                let _ = addr.send(ServerMessage::SDP(local_desc)).await;

            }
        }

        Ok(())
    }

    async fn handle_reply(&self, reply: RTCSessionDescription) -> Result<()> {
        self.peer_connection.set_remote_description(reply).await?;
        let tracks = self.tracks.lock().await;
        for v in tracks.values() {
            v.0.send_ready.lock().await.send(true)?;
        }
        

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
                while let Ok((p, a)) = rtp_sender.read_rtcp().await {
                    for p in p {
                        
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
                addr: self.addr.clone(),
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

    pub async fn new(dvr: DVRWeak, addr: WeakAddr<ClientHandler>) -> Result<Client> {
        let (sn, rn) = watch::channel(false);

        let api = Self::get_api().await?;
    
        let config = RTCConfiguration {
            ice_servers: vec![
                
                RTCIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                    ..Default::default()
                },

                RTCIceServer {
                    urls: vec!["turn:157.245.116.36:3478".to_owned()],
                    username: "applesauce".to_owned(),
                    credential_type: webrtc::ice_transport::ice_credential_type::RTCIceCredentialType::Password,
                    credential: "jd6ESz7AfZGMeyPxtsm6".to_owned()
                }
            
            ],
            bundle_policy: RTCBundlePolicy::MaxBundle,
            ..Default::default()
        };
    
        let peer_connection = Arc::new(api.new_peer_connection(config).await?);

        peer_connection.on_ice_candidate({
            let addr = addr.clone();
            Box::new(move |candidate| {
                let addr = addr.clone();
                Box::pin(async move {
                    if let Some(candidate) = candidate {
                        if let Ok(candidate) = candidate.to_json().await {
                            if let Some(addr) = addr.upgrade() {
                                let _ = addr.send(ServerMessage::ICE(candidate)).await;

                            }

                        }
                    }
                })
            }
            )
        }).await;


        peer_connection.on_peer_connection_state_change({
            Box::new(move |state| {
                
                match state {
                    RTCPeerConnectionState::Connected => {
                        let _ = sn.send(true);
                    }
                    _ => {}
                };
                Box::pin(async move {
                })
            }
            )
        }).await;
        
        let c = Client(Arc::new(ClientInner{
            peer_connection: peer_connection.clone(),
            tracks: Mutex::new(HashMap::new()),
            ready: rn,
            dvr,
            addr
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
        

        Ok(c)
    }
}

pub struct ClientHandler{
    pub dvr: DVRWeak,
    pub client: Option<Client>
}

impl Actor for ClientHandler {
    type Context = ws::WebsocketContext<Self>;
    fn started(&mut self, ctx: &mut Self::Context) {
        let addr = ctx.address().downgrade();
        let dvr = self.dvr.clone();
        
        let o = async move {
            Client::new(dvr, addr).await
        }.into_actor(self).then(|c, act, _ctx| {
            if let Ok(c) = c {
                act.client.replace(c);
            }
            fut::ready(())
        }).wait(ctx);

    }

    fn stopping(&mut self, ctx: &mut Self::Context) -> Running {
        if let Some(c) = self.client.take() {
        }
        Running::Stop
    }
}

impl Handler<ServerMessage> for ClientHandler {
    type Result = ();
    fn handle(&mut self, msg: ServerMessage, ctx: &mut Self::Context) -> Self::Result {
        let json = serde_json::to_string(&msg).unwrap();
        ctx.text(json);
    }
}

impl Handler<ClientMessage> for ClientHandler {
    type Result = ();
    fn handle(&mut self, msg: ClientMessage, ctx: &mut Self::Context) -> Self::Result {
        if let Some(c) = &self.client {
            let c = c.clone();
            async move {
                match msg {
                    ClientMessage::SDP(s) => {
                        if let Err(err) = c.handle_reply(s).await {
                            println!("{:?}", err);
                        }
                    }
                    
                    ClientMessage::ICE(candidate) => {
                        if let Err(err) = c.handle_ice(candidate).await {
                            println!("{:?}", err);
                        }
                    }
                    ClientMessage::AddTrack(s) => {
                        if let Err(err) = c.add_track(s).await {
                            println!("{:?}", err);
                        }
                    }
                    ClientMessage::RemoveTrack(s) => {
                        if let Err(err) = c.remove_track(s).await {
                            println!("{:?}", err);
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
                            tracks[&s.id].start_playback(c.dvr.clone(), c.ready.clone(), s.source, s.at).await;
                        }
                    }
                    
                    _ => ()
                }
            }.into_actor(self).wait(ctx);
        }
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for ClientHandler {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        if let Ok(msg) = msg {
            match msg {
                ws::Message::Text(text) => {
                    //let _ = self.inner.tx.try_send(text.trim().to_owned());
                    if let Ok(m) = serde_json::from_str::<ClientMessage>(&text) {
                        let f = ctx.address().do_send(m);
                    }
                },
                ws::Message::Ping(bytes) => ctx.pong(&bytes),
                ws::Message::Close(reason) => {
                    ctx.close(reason);
                    ctx.stop();
                }
                _ => {}
            }
        } else {
            ctx.stop();
        }
    }

}