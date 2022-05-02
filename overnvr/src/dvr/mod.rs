use crate::Result;
use crate::detector::{Detector, BufferResult};
use crate::source::{Source, SourceWeak};
use crate::surface::Surface;

use mongodb::bson::oid::ObjectId;
use overnvr_deepstream::surface::EncodedImage;

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use std::collections::HashMap;
use std::time::Duration;
use std::{sync::{Arc, Weak}, ops::Deref};

use geo::prelude::*;
use crate::model::{Config, Detection, Region, Alert, RegionRef};

use crate::model::{Event, JsonDuration, SourceConfig};

use rumqttc::{AsyncClient as MqttClient, LastWill, MqttOptions, QoS};
use crate::database::Database;


pub struct DVRInner {
    pub(crate) config: Config,
    pub(crate) database: Database,

    pub(crate) sources: Mutex<HashMap<ObjectId, Vec<Source>>>,
    pub(crate) detector: Detector<SourceWeak>,
    pub(crate) regions: Mutex<HashMap<ObjectId, Vec<Region>>>,
    pub(crate) alerts: Mutex<HashMap<ObjectId, Alert>>,
    pub(crate) region_alert: Mutex<HashMap<RegionRef, Vec<ObjectId>>>,
    pub(crate) mqtt: MqttClient,
}

#[derive(Clone)]
pub struct DVR(Arc<DVRInner>);

#[derive(Clone)]
pub struct DVRWeak(Weak<DVRInner>);

impl Deref for DVR {
    type Target = DVRInner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DVR {
    pub fn downgrade(&self) -> DVRWeak {
        DVRWeak(Arc::downgrade(&self.0))
    }
}

impl DVRWeak {
    pub fn upgrade(&self) -> Option<DVR> {
        self.0.upgrade().map(|d| DVR(d))
    }
}

impl DVR {
    
}

impl DVR {
    pub async fn new(config: Config) -> Result<DVR> {
        let database = Database::new(&config.database_url, &config.database_name).await?;

        let detector = Detector::new();

        let mut mqtt_options = MqttOptions::new(config.mqtt.client_id.clone(), config.mqtt.host.clone(), config.mqtt.port);
        mqtt_options.set_keep_alive(Duration::from_secs(5));
        mqtt_options.set_last_will(LastWill::new("overnvr_rust/available", "offline".to_string(), QoS::AtLeastOnce, true));
        
        if let Some(credentials) = &config.mqtt.credentials {
            mqtt_options.set_credentials(credentials.username.clone(), credentials.password.clone());
        }
        let (mqtt, mut mqtteventloop) = MqttClient::new(mqtt_options, 10);

        tokio::task::spawn(async move {
            loop {
                mqtteventloop.poll().await.unwrap();
            }
        });

        Ok(DVR(Arc::new(DVRInner {
            config,
            database,
            sources: Mutex::new(HashMap::new()),
            detector,
            regions: Default::default(),
            alerts: Default::default(),
            region_alert: Default::default(),
            mqtt
        })))
    }
    
    pub async fn trigger_region(&self, r: &mut Region, result: &Arc<BufferResult<SourceWeak>>, surf: &Surface, pts: JsonDuration, id: ObjectId) -> Result<()> {
        let encoded = result.crop(surf).await?;
        let path = self.config.storage.snapshots.clone();
        
        std::fs::create_dir_all(&path)?;

        let event = ObjectId::new();
        let mut path_crop = path.clone();
        let fname = format!("{}.jpg", event.to_string());
        path_crop.push(fname.clone());

        //let path_full = path.clone();
        //path_full.crop.push(format!("{}_full.jpg", event.to_string()));

        let mut file = tokio::fs::File::create(&path_crop).await?;
        file.write_all(&*encoded).await?;

        let event = Event {
            _id: ObjectId::new(), 
            start: pts.into(),
            end: pts.into(), 
            camera_id: id,
            thumb_start: fname,
            region_name: r.region_name.clone()
        };
        r.last_event.replace(event.clone());


        let dvrweak = self.downgrade();
        tokio::spawn(async move {
            if let Some(dvr) = dvrweak.upgrade() {
                if let Err(e) = dvr.database.add_event(event).await {
                    println!("{:?}", e);
                }

            }
        });

        let dvrweak = self.downgrade();
        let region_ref = r.clone().into();
        tokio::spawn(async move {
            if let Some(dvr) = dvrweak.upgrade() {
                if let Some(alert_ids) = dvr.region_alert.lock().await.get(&region_ref) {
                    let regions = dvr.regions.lock().await;
                    let alerts = dvr.alerts.lock().await;

                    for a in alert_ids {
                        if let Some(alert) = alerts.get(a) {
                            let _ = alert.send_update(&dvr, &regions, Some(encoded.clone())).await;
                        }
                    }
                }
            }

        });
        

        Ok(())
    }

    pub async fn update_region(&self, r: &mut Region, pts: JsonDuration) -> Result<()> {
        if let Some(e) = &r.last_event {
            let e = e.clone(); 
            let dvrweak = self.downgrade();
            tokio::spawn(async move {
                if let Some(dvr) = dvrweak.upgrade() {
                    let _ = dvr.database.update_event(&e, &pts).await;    
                }
            });
        }
        Ok(())
    }

    pub async fn clear_region(&self, r: &mut Region) -> Result<()> {
        // TODO: MQTT
        r.last_event.take();
        r.last_trigger.take();

        let dvrweak = self.downgrade();
        let region_ref = r.clone().into();
        tokio::spawn(async move {
            if let Some(dvr) = dvrweak.upgrade() {
                if let Some(alert_ids) = dvr.region_alert.lock().await.get(&region_ref) {
                    let regions = dvr.regions.lock().await;
                    let alerts = dvr.alerts.lock().await;

                    for a in alert_ids {
                        if let Some(alert) = alerts.get(a) {
                            let _ = alert.send_update(&dvr, &regions, None).await;
                        }
                    }
                }
            }

        });

        Ok(())
    }

    pub async fn start_detector(&self) -> Result<()> {
        self.detector.start(self.config.detector.clone()).await;
        let surf = Surface::new(512, 512, false).await?;
        let mut last_update = std::time::Instant::now();
        let pause = std::time::Duration::from_secs(5);
        let mut count = 0 as usize;
        //println!("Started");
        while let Some(r) = self.detector.get_next().await {
            let when = std::time::Instant::now();
            let diff = when - last_update;

            count = count + r.len();
            if diff > pause {
                println!("{} FPS", (count as f64) / diff.as_secs_f64());
                last_update = when;
                count = 0;
            }

            let mut detections = Vec::new();
            let mut regions = self.regions.lock().await;

            for f in &r {

                if let Some(source) = f.get_source().upgrade() {
                    let source_id = &source.inner._id;

                    if let Some(pts) = f.get_pts() {
                        let pts = pts.into();
                        for d in &f.detections {
                            detections.push(Detection {
                                class_id: d.class_id.clone(),
                                confidence: d.confidence,
                                region: d.rect.clone(),
                                source: source.inner._id.clone(),
                                start: pts
                            })
                        }

                        if let Some(regions) = regions.get_mut(&source_id) {
                            for region in regions.iter_mut() {
                                for d in &f.detections {
                                    if region.classes.contains(&d.class_id) && region.min_confidence <= d.confidence && region.poly.intersects(&d.foot) {
                                        match region.last_trigger {
                                            Some(_) => {
                                                // Update
                                                let _ = self.update_region(region, pts).await;
                                            }
                                            None => {
                                                // Trigger
                                                let _ = self.trigger_region(region, &f, &surf, pts, source.inner._id.clone()).await;
                                                println!("{:?} Trigger {}", pts, region.region_name);
                                            }
                                        }
                                        region.last_trigger.replace(pts);
                                    }
                                }
                                match region.last_trigger {
                                    Some(p) => {
                                        if (pts - p) >= region.hold_time {
                                            //region.last_trigger.take();
                                            println!("{:?} Clear {}", pts, region.region_name);
                                            let _ = self.clear_region(region).await;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                }
            }
            let _ = self.database.add_detections(detections).await;
        }
        Ok(())
    }

    pub async fn start(&self) -> Result<()> {
        let g_app = glib::MainLoop::new(None, false);
        tokio::task::spawn_blocking(move || {
            g_app.run();
        });

        self.mqtt.publish("overnvr_rust/available".to_string(), 
            QoS::AtLeastOnce, 
            true, 
            "online").await?;

        {
            let mut sources = self.sources.lock().await;

            for camera in self.database.list_cameras().await? {
                let mut streams = Vec::new();
                for (k, v) in &camera.streams {
                    let stream_id = format!("{}_{}", camera._id.to_string(), k);
                    let source = Source::new(
                        &camera._id,
                        &stream_id,
                        &v.url,
                        v.detect,
                        self.downgrade()
                    ).await?;
                    let source_weak = source.downgrade();
                    tokio::spawn({
                        let source_weak = source_weak.clone();
                        async move {
                            if let Some(source) = source_weak.upgrade() {
                                let _ = source.run().await;
                            }
                        }
                    });
                    streams.push(source);
                    
                }
                sources.insert(camera._id, streams);
                //let stream_id = format!()
            }
        }
        
        {

            let mut region_list = self.regions.lock().await;

            for region in self.database.list_regions().await? {
                let camera_id = region.camera_id.clone();
                if let Some(r) = region_list.get_mut(&camera_id) {
                    r.push(region);
                } else {
                    let mut r = Vec::new();
                    r.push(region);
                    region_list.insert(camera_id.clone(), r);
                }
            }

            let mut alert_list = self.alerts.lock().await;
            
            for alert in self.database.list_alerts().await? {
                let _ = alert.setup(&self, &*region_list).await;
                alert_list.insert(alert._id.clone(), alert);
            }
        }
        
        let self_weak = self.downgrade();
        tokio::spawn(async move {
            if let Some(self_strong) = self_weak.upgrade() {
                let _ = self_strong.start_detector().await;
            }
        });
        
        Ok(())
    }

    pub async fn camera_image(&self, camera: ObjectId) -> Result<Arc<EncodedImage>> {      
        let cameras = self.sources.lock().await;
        if let Some(camera) = cameras.get(&camera) {
            for c in camera {
                if let Some(surf) = &c.inner.snapshot {
                    let encoded = surf.encode().await?;
                    return Ok(encoded);
                }
            }
        }

        anyhow::bail!("Unable to create snapshot")
    }

    pub async fn remove_camera(&self, camera: &ObjectId) {
        let mut sources = self.sources.lock().await; 
        drop(sources.remove(camera));
    }

    pub async fn update_camera(&self, camera_config: &SourceConfig) -> Result<()> {      

        let mut sources = self.sources.lock().await; 

        let mut streams = Vec::new();
        for (k, v) in &camera_config.streams {
            let stream_id = format!("{}_{}", camera_config._id.to_string(), k);
            let source = Source::new(
                &camera_config._id,
                &stream_id,
                &v.url,
                v.detect,
                self.downgrade()
            ).await?;
            let source_weak = source.downgrade();
            tokio::spawn({
                let source_weak = source_weak.clone();
                async move {
                    if let Some(source) = source_weak.upgrade() {
                        let _ = source.run().await;
                    }
                }
            });
            streams.push(source);
            
        }
        sources.insert(camera_config._id, streams);

        Ok(())
    }

    pub async fn update_regions(&self, camera: ObjectId, regions: &Vec<Region>) -> Result<()> {

        let mut my_regions = self.regions.lock().await;
        if let Some(my_regions) = my_regions.get_mut(&camera) {
            for r in my_regions.iter_mut() {
                let _ = self.clear_region(r).await;
            }
        }

        my_regions.insert(camera, regions.clone());

        Ok(())
    }

    pub async fn remove_alert(&self, id: ObjectId) {
        let mut alerts = self.alerts.lock().await;
        if let Some(alert) = alerts.remove(&id) {
            let _regions = self.regions.lock().await;

            let _ = alert.teardown(&self).await;
        }
    }

    pub async fn add_alert(&self, alert: Alert) {
        let mut alerts = self.alerts.lock().await;
        let regions = self.regions.lock().await;

        let alert = alert.clone();
        let _ = alert.setup(&self, &regions).await;
        alerts.insert(alert._id.clone(), alert);
    }

}