
use std::any::Any;
use std::time::Duration;

use webrtc::error::{flatten_errs, Result};

const RTP_OUTBOUND_MTU: usize = 1200;
use webrtc::media::Sample;
use tokio::sync::Mutex;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecParameters, RTPCodecType, RTCRtpCodecCapability};
use webrtc::track::track_local::{TrackLocalContext, TrackLocal, TrackLocalWriter};
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use crate::packetizer::*;

use async_trait::async_trait;

#[derive(Debug, Clone)]
struct TrackLocalStaticSampleInternal {
    packetizer: Option<PacketizerImpl>,
    base: Option<Duration>,
    sequencer: Option<Box<dyn webrtc::rtp::sequence::Sequencer + Send + Sync>>,
    clock_rate: f64,
}

/// TrackLocalStaticSample is a TrackLocal that has a pre-set codec and accepts Samples.
/// If you wish to send a RTP Packet use TrackLocalStaticRTP
#[derive(Debug)]
pub struct TrackLocalStaticSample {
    rtp_track: TrackLocalStaticRTP,
    internal: Mutex<TrackLocalStaticSampleInternal>,
}

impl TrackLocalStaticSample {
    /// returns a TrackLocalStaticSample
    pub fn new(codec: RTCRtpCodecCapability, id: String, stream_id: String) -> Self {
        let rtp_track = TrackLocalStaticRTP::new(codec, id, stream_id);

        TrackLocalStaticSample {
            rtp_track,
            internal: Mutex::new(TrackLocalStaticSampleInternal {
                packetizer: None,
                sequencer: None,
                base: None,
                clock_rate: 0.0f64,
            }),
        }
    }

    /// codec gets the Codec of the track
    pub fn codec(&self) -> RTCRtpCodecCapability {
        self.rtp_track.codec()
    }

    pub async fn rebase(&self) {
        let mut internal = self.internal.lock().await;
        internal.base.take();
        if let Some(packetizer) = &mut internal.packetizer {
            packetizer.rebase();
        }
    }

    /// write_sample writes a Sample to the TrackLocalStaticSample
    /// If one PeerConnection fails the packets will still be sent to
    /// all PeerConnections. The error message will contain the ID of the failed
    /// PeerConnections so you can remove them
    pub async fn write_sample(&self, sample: &Sample) -> Result<()> {
        let mut internal = self.internal.lock().await;

        if internal.packetizer.is_none() || internal.sequencer.is_none() {
            return Ok(());
        }

        // skip packets by the number of previously dropped packets
        if let Some(sequencer) = &internal.sequencer {
            for _ in 0..sample.prev_dropped_packets {
                sequencer.next_sequence_number();
            }
        }

        let clock_rate = internal.clock_rate;

        if internal.base == None {
            internal.base.replace(sample.duration);
        }

        let diff = sample.duration - internal.base.unwrap();

        let packets = if let Some(packetizer) = &mut internal.packetizer {

            let samples = (diff.as_secs_f64() * clock_rate) as u32;
            /*println!(
                "clock_rate={}, samples={}, {}",
                clock_rate,
                samples,
                sample.duration.as_secs_f64()
            );*/
            packetizer.packetize(&sample.data, samples).await?
        } else {
            vec![]
        };

        let mut write_errs = vec![];
        for p in packets {
            if let Err(err) = self.rtp_track.write_rtp(&p).await {
                write_errs.push(err);
            }
        }

        flatten_errs(write_errs)
    }

    pub async fn get_timestamp(&self) -> u32 {
        let internal = self.internal.lock().await;
        if let Some(packetizer) = &internal.packetizer {
            return packetizer.timestamp;
        }
        return 0;
    }
}

#[async_trait]
impl TrackLocal for TrackLocalStaticSample {
    /// Bind is called by the PeerConnection after negotiation is complete
    /// This asserts that the code requested is supported by the remote peer.
    /// If so it setups all the state (SSRC and PayloadType) to have a call
    async fn bind(&self, t: &TrackLocalContext) -> Result<RTCRtpCodecParameters> {
        let codec = self.rtp_track.bind(t).await?;

        let mut internal = self.internal.lock().await;

        // We only need one packetizer
        if internal.packetizer.is_some() {
            return Ok(codec);
        }

        let payloader = Box::new(webrtc::rtp::codecs::h264::H264Payloader::default());
        let sequencer: Box<dyn webrtc::rtp::sequence::Sequencer + Send + Sync> =
            Box::new(webrtc::rtp::sequence::new_random_sequencer());
        internal.packetizer = Some(new_packetizer(
            RTP_OUTBOUND_MTU,
            0, // Value is handled when writing
            0, // Value is handled when writing
            payloader,
            sequencer.clone(),
            codec.capability.clock_rate,
        ));
        internal.sequencer = Some(sequencer);
        internal.clock_rate = codec.capability.clock_rate as f64;

        Ok(codec)
    }

    /// unbind implements the teardown logic when the track is no longer needed. This happens
    /// because a track has been stopped.
    async fn unbind(&self, t: &TrackLocalContext) -> Result<()> {
        self.rtp_track.unbind(t).await
    }

    /// id is the unique identifier for this Track. This should be unique for the
    /// stream, but doesn't have to globally unique. A common example would be 'audio' or 'video'
    /// and StreamID would be 'desktop' or 'webcam'
    fn id(&self) -> &str {
        self.rtp_track.id()
    }

    /// stream_id is the group this track belongs too. This must be unique
    fn stream_id(&self) -> &str {
        self.rtp_track.stream_id()
    }

    /// kind controls if this TrackLocal is audio or video
    fn kind(&self) -> RTPCodecType {
        self.rtp_track.kind()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
