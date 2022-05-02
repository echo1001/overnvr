use super::*;


#[derive(Serialize, Deserialize)]
pub struct Info {
    pub id: String,
    pub startpts: JsonDuration,
    pub startrtp: u32
}

#[derive(Serialize, Deserialize)]
pub struct TrackInfo {
    pub id: String,
}

#[derive(Serialize, Deserialize)]
pub struct StartLive {
    pub id: String,
    pub stream: String,
    pub quality: String
}

#[derive(Serialize, Deserialize)]
pub struct StartPast {
    pub id: String,
    pub source: String,
    pub at: JsonDuration
}

#[derive(Serialize, Deserialize, Message)]
#[rtype(result = "()")]
pub enum ClientMessage {
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
pub enum ServerMessage {
    SDP(RTCSessionDescription),
    ICE(RTCIceCandidateInit),
    AddTrack(Vec<TrackInfo>),
    RemoveTrack(Vec<TrackInfo>),
    Live(StartLive),
    From(StartPast),
    StartInfo(Info)
}