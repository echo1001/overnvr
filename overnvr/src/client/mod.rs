use crate::Result;
use crate::model::JsonDuration;
use crate::track;
use crate::SourceLink;
use crate::dvr::{DVRWeak, DVR};

use actix::WeakAddr;
use actix::prelude::*;
use actix_web_actors::ws;

use gst::prelude::*;
use mongodb::bson::oid::ObjectId;
use once_cell::sync::OnceCell;

use serde::{Serialize, Deserialize};

use tokio::task::JoinHandle;
use tokio::sync::Mutex;
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

mod input;
mod api;
mod model;

use input::InputPipeline;
pub use api::{export, ClientHandler};
use model::*;


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

                let (pipeline, mut r) = InputPipeline::open_raw(&recording.file, if at >= file_start { Some(at - file_start)} else {None}).await?;
                
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

#[derive(Clone)]
pub struct Track(Arc<TrackInner>);


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
                while let Ok((_p, _a)) = rtp_sender.read_rtcp().await {
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
                        let _ = c.negotiate().await;
                    }
                })
            })
        }).await;
        

        Ok(c)
    }
}
