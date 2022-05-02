use super::*;

pub struct InputPipeline {
    pipeline: gst::Pipeline
}

impl InputPipeline {
    pub async fn open_raw(fname: &String, at: Option<JsonDuration>) -> Result<(InputPipeline, gst_app::app_sink::AppSinkStream)> {
        let pipeline = gst::parse_launch("filesrc name=file ! matroskademux name=demux ! appsink sync=0 wait-on-eos=0 max-buffers=5 emit-signals=1 async=0 enable-last-sample=0 name=sink")?;
        let pipeline: gst::Pipeline = pipeline.downcast().unwrap();
        pipeline.by_name("file").unwrap().set_property("location", fname);
        let appsink: gst_app::AppSink = pipeline.by_name("sink").unwrap().downcast().unwrap();

        let demux = pipeline.by_name("demux").unwrap();

        let mut stream = appsink.stream();

        pipeline.call_async_future(|pipeline| {
            pipeline.set_state(gst::State::Playing)?;
            Result::<()>::Ok(())
        }).await?;

        if let Some(d) = at {
            stream.next().await;
            
            let mut flags = gst::SeekFlags::empty();
            flags.insert(gst::SeekFlags::KEY_UNIT);
            flags.insert(gst::SeekFlags::SNAP_BEFORE);
            flags.insert(gst::SeekFlags::FLUSH);

            let d: Duration = d.into();
            let time: gst::ClockTime = gst::ClockTime::try_from(d)?;

            demux.seek_simple(flags, time)?;
        }

        Ok((InputPipeline { 
            pipeline
        }, stream))
    }
}

impl Drop for InputPipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}