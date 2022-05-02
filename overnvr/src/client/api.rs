use super::*;

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
        pipeline.set_state(gst::State::Playing)?;
        Result::<()>::Ok(())
    }).await?;

    let mut base = None;

    while let Some(recording) = cursor.try_next().await? {
        let file_start = recording.start;

        let (_pipeline, mut r) = 
            InputPipeline::open_raw(&recording.file, if from >= file_start { Some(from - file_start)} else {None}).await?;
        
        while let Some(s) = r.next().await {
            if let Some(buffer) = s.buffer() {
                if let Some(pts) = buffer.pts() {
                    let mut buf = buffer.to_owned();
                    let buffer = buf.get_mut().unwrap();

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
                    
                    sink.send(sample).await?; 
                    
                }
            }
        }
    }

    appsrc.end_of_stream()?;

    bus.timed_pop_filtered(None, &[gst::MessageType::Eos, gst::MessageType::Error]);

    pipeline.set_state(gst::State::Null)?;

    let f = tokio::fs::read(&fname).await?;
    tokio::fs::remove_file(&fname).await?;
    Ok(f)
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
        
        async move {
            Client::new(dvr, addr).await
        }.into_actor(self).then(|c, act, _ctx| {
            if let Ok(c) = c {
                act.client.replace(c);
            }
            fut::ready(())
        }).wait(ctx);

    }

    fn stopping(&mut self, _: &mut Self::Context) -> Running {
        self.client.take();
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
                        ctx.address().do_send(m);
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