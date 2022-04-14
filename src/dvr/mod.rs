use crate::Result;
use crate::detector::{Detector, BufferResult};
use crate::source::{Source, SourceConfig, SourceWeak};
use crate::surface::Surface;
use mongodb::IndexModel;
use mongodb::bson::oid::ObjectId;
use mongodb::options::FindOptions;
use serde::{Deserialize, Serialize};

use mongodb::bson::doc;

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use std::{sync::{Arc, Weak}, ops::Deref};

use futures::stream::TryStreamExt;
use geo::prelude::*;

use warp::Filter;
use warp::reply::json;


use crate::model::{Config, Detection, Detection_Json, DetectionFilter, RangeFilter, Region, ExportFilter, Alert, RegionRef};

use crate::model::{Event, EventGroup, JumpFilter, EventFilter, JsonDuration, MS};

use rumqttc::{AsyncClient as MqttClient, LastWill, MqttOptions, QoS};



pub struct DVRInner {
    pub(crate) config: Config,
    pub(crate) database: mongodb::Database,
    pub(crate) sources: Mutex<HashMap<ObjectId, Vec<Source>>>,
    pub(crate) detector: Detector<SourceWeak>,
    pub(crate) regions: Mutex<HashMap<ObjectId, Vec<Region>>>,
    pub(crate) alerts: Mutex<HashMap<ObjectId, Alert>>,
    pub(crate) region_alert: Mutex<HashMap<RegionRef, Vec<ObjectId>>>,
    pub(crate) mqtt: MqttClient,
}

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
    pub async fn new(config: Config) -> Result<DVR> {
        let client = mongodb::Client::with_uri_str(config.database_url.clone()).await?;
        let database = client.database(config.database_name.as_str());

        let recordings = database.collection::<()>("recordings");

        let index = IndexModel::builder()
            .keys(doc! {"source": 1, "start": 1})
            .build();
        recordings.create_index(index, None).await?;

        let index = IndexModel::builder()
            .keys(doc! {"source": 1, "file": 1})
            .build();
        recordings.create_index(index, None).await?;

        let index = IndexModel::builder()
            .keys(doc! {"start": 1})
            .build();
        recordings.create_index(index, None).await?;

        let detections = database.collection::<()>("detections");

        let index = IndexModel::builder()
            .keys(doc! {"source": 1, "start": 1})
            .build();
        detections.create_index(index, None).await?;


        let events = database.collection::<()>("events");

        let index = IndexModel::builder()
            .keys(doc! {"camera_id": -1, "start": -1})
            .build();
        events.create_index(index, None).await?;

        let index = IndexModel::builder()
            .keys(doc! {"camera_id": 1, "start": -1, "end": -1})
            .build();
        events.create_index(index, None).await?;

        let index = IndexModel::builder()
            .keys(doc! {"camera_id": -1, "end": -1})
            .build();
        events.create_index(index, None).await?;

        let index = IndexModel::builder()
            .keys(doc! {"start": -1})
            .build();
        events.create_index(index, None).await?;

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
    
    pub async fn trigger_region(&self, r: &mut Region, result: &Arc<BufferResult<SourceWeak>>, surf: &Surface, pts: JsonDuration<MS>, id: ObjectId) -> Result<()> {
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
                if let Err(e) = dvr.database.collection::<Event>("events").insert_one(event, None).await {
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
                            alert.send_update(&dvr, &regions, Some(encoded.clone())).await;
                        }
                    }
                }
            }

        });
        

        Ok(())
    }

    pub async fn update_region(&self, r: &mut Region, pts: JsonDuration<MS>) -> Result<()> {
        if let Some(e) = &r.last_event {
            let e = e.clone(); 
            let dvrweak = self.downgrade();
            tokio::spawn(async move {
                if let Some(dvr) = dvrweak.upgrade() {
                    let query = doc! {"_id": e._id.clone()};
                    let update = doc! {"$set": {"end": pts.as_millis() as i64}};
                    dvr.database.collection::<Event>("events").update_one(query, update, None).await;
    
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
                            alert.send_update(&dvr, &regions, None).await;
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
        let mut pause = std::time::Duration::from_secs(5);
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

            //println!("a {}", r.len());
            let mut detections = Vec::new();
            //let mut to_update = HashMap::new();
            let mut regions = self.regions.lock().await;

            //println!("b {}", r.len());

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
                                            self.clear_region(region).await;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                }
            }
            let _ = self.database.collection::<Detection>("detections").insert_many(detections, None).await;
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

            let cameras = self.database.collection::<SourceConfig>("cameras");
            let mut cameras = cameras.find(doc!{"enable": true}, None).await?;
            while let Some(camera) = cameras.try_next().await? {
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
            let regions = self.database.collection::<Region>("regions");
            let mut regions = regions.find(None, None).await?;

            while let Some(region) = regions.try_next().await? {
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
            let alerts = self.database.collection::<Alert>("alerts");
            let mut alerts = alerts.find(None, None).await?;

            while let Some(mut alert) = alerts.try_next().await? {
                alert.setup(&self, &*region_list).await;
                alert_list.insert(alert._id.clone(), alert);
            }
        }

        let self_weak = self.downgrade();
        tokio::spawn(async move {
            if let Some(self_strong) = self_weak.upgrade() {
                let _ = self_strong.start_detector().await;
            }
        });

        let dvr_weak = self.downgrade();

        let with_dvr = move || {
            let dvr_weak = dvr_weak.clone();
            warp::any().map(move || dvr_weak.clone().upgrade().unwrap())
        };
        

        let ws = warp::path("ws")
            .and(with_dvr())
            .and(warp::ws())
            .map(Self::start_client);

        let detections = warp::get()
            .and(warp::path("detections"))
            .and(with_dvr())
            .and(warp::query::<DetectionFilter>())
            .and_then(Self::get_detections);

        let events = warp::path::end()
            .and(warp::get())
            .and(with_dvr())
            .and(warp::query::<EventFilter>())
            .and_then(Self::list_events);

        let get_event = warp::get()
            .and(with_dvr())
            .and(warp::path!(ObjectId))
            .and(warp::path::end())
            .and_then(Self::get_event);

        let range = warp::get()
            .and(warp::path("range"))
            .and(with_dvr())
            .and(warp::query::<RangeFilter>())
            .and_then(Self::list_events_range);

        let previous = warp::get()
            .and(warp::path("previous"))
            .and(with_dvr())
            .and(warp::query::<JumpFilter>())
            .and_then(Self::list_event_previous);

        let next = warp::get()
            .and(warp::path("next"))
            .and(with_dvr())
            .and(warp::query::<JumpFilter>())
            .and_then(Self::list_event_next);

        let camera_list = warp::path::end()
            .and(warp::get())
            .and(with_dvr())
            .and_then(Self::list_cameras);

        let get_camera = warp::get()
            .and(with_dvr())
            .and(warp::path!(ObjectId))
            .and(warp::path::end())
            .and_then(Self::get_camera);

        fn json_body_camera() -> impl Filter<Extract = (SourceConfig,), Error = warp::Rejection> + Clone {
            warp::body::content_length_limit(1024 * 16)
                .and(warp::body::json())
        }

        let update_camera = warp::post()
            .and(with_dvr())
            .and(warp::path!(ObjectId))
            .and(warp::path::end())
            .and(json_body_camera())
            .and_then(Self::update_camera);

        let export = warp::get()
            .and(with_dvr())
            .and(warp::path::param::<ObjectId>())
            .and(warp::path::path("export"))
            //.and(warp::path!(ObjectId / "export" / String ".mkv"))
            //.and(warp::path::end())
            .and(warp::query::<ExportFilter>())
            .and_then(Self::export_range);

        fn json_body_region() -> impl Filter<Extract = (Vec<Region>,), Error = warp::Rejection> + Clone {
            warp::body::content_length_limit(1024 * 16)
                .and(warp::body::json())
        }

        let camera_image = warp::get()
            .and(with_dvr())
            .and(warp::path!(ObjectId / "image.jpg"))
            .and_then(Self::camera_image);
        
        let region_list = warp::get()
            .and(with_dvr())
            .and(warp::path!(ObjectId / "regions"))
            .and_then(Self::list_regions);
        
        
        let region_update = warp::post()
            .and(with_dvr())
            .and(warp::path!(ObjectId / "regions"))
            .and(json_body_region())
            .and_then(Self::save_regions);

        let alert_list = warp::get()
            .and(warp::path::end())
            .and(with_dvr())
            .and_then(Self::alert_list);

        fn json_body_alert() -> impl Filter<Extract = (Alert,), Error = warp::Rejection> + Clone {
            warp::body::content_length_limit(1024 * 16)
                .and(warp::body::json())
        }

        let alert_add = warp::post()
            .and(warp::path::end())
            .and(with_dvr())
            .and(json_body_alert())
            .and_then(Self::alert_add);


        let alert_delete = warp::delete()
            .and(with_dvr())
            .and(warp::path::param::<ObjectId>())
            .and(warp::path::end())
            .and_then(Self::alert_delete);


        let alert_update = warp::post()
            .and(with_dvr())
            .and(warp::path::param::<ObjectId>())
            .and(warp::path::end())
            .and(json_body_alert())
            .and_then(Self::alert_update);
        
        let thumb = warp::path("thumb").and(warp::fs::dir(self.config.storage.snapshots.clone()));

        let event = warp::path("event").and(events.or(range).or(previous).or(next).or(thumb).or(get_event));

        let cameras = warp::path("cameras").and(camera_list.or(camera_image).or(region_list).or(region_update).or(export).or(get_camera).or(update_camera));

        let alerts = warp::path("alerts").and(alert_list.or(alert_add).or(alert_delete).or(alert_update));
        
        let api = warp::path("api")
            .and(
                detections.or(cameras).or(event).or(alerts)
            );

        let root = ws.or(api);
        warp::serve(root).run(([0, 0, 0, 0], 8091)).await;
        Ok(())
    }

    fn start_client(self, ws: warp::ws::Ws) -> impl warp::Reply {
        ws.on_upgrade(move |ws| {
            Box::pin(async move {
                let _ = crate::client::Client::start(ws, self.downgrade()).await;
            })
        })
    }


    async fn get_camera(self, camera: ObjectId) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        let cameras_collection = self.database.collection::<SourceConfig>("cameras");
        let camera = cameras_collection.find_one(doc!{"_id": camera}, None).await.map_err(|_| warp::reject())?;

        Ok(json(&camera))
    }

    async fn export_range(self, camera: ObjectId, filter: ExportFilter) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        let data = crate::client::export(camera, *filter.from, *filter.to, &self).await.map_err(|_| warp::reject())?;
        
        Ok(data)
    }

    async fn update_camera(self, camera: ObjectId, mut camera_config: SourceConfig) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        let cameras_collection = self.database.collection::<SourceConfig>("cameras");
        //let camera = cameras_collection.find_one(doc!{"_id": camera}, None).await.map_err(|_| warp::reject())?;

        camera_config._id = camera;

        let mut sources = self.sources.lock().await; 
        drop(sources.remove(&camera));

        let mut streams = Vec::new();
        for (k, v) in &camera_config.streams {
            let stream_id = format!("{}_{}", camera_config._id.to_string(), k);
            let source = Source::new(
                &camera_config._id,
                &stream_id,
                &v.url,
                v.detect,
                self.downgrade()
            ).await.map_err(|_| warp::reject())?;
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
        let camera_bson = mongodb::bson::to_document(&camera_config).unwrap();
        let update = doc! {
            "$set": camera_bson
        };
        cameras_collection.update_one(doc!{"_id": camera}, update, None).await.map_err(|_| warp::reject())?;

        Ok(json(&camera_config))
    }

    async fn list_cameras(self) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        let mut cameras = Vec::new();

        let cameras_collection = self.database.collection::<SourceConfig>("cameras");
        let mut cursor = cameras_collection.find(doc!{"enable": true}, None).await.map_err(|_| warp::reject())?;
        while let Some(camera) = cursor.try_next().await.map_err(|_| warp::reject())? {
            cameras.push(camera);
        }

        Ok(json(&cameras))
    }

    async fn alert_list(self) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        let mut alerts = Vec::new();

        let alerts_collection = self.database.collection::<Alert>("alerts");
        let mut cursor = alerts_collection.find(None, None).await.map_err(|_| warp::reject())?;

        while let Some(region) = cursor.try_next().await.map_err(|_| warp::reject())? {
            alerts.push(region);
        }

        Ok(json(&alerts))
    }

    async fn alert_add(self, alert: Alert) -> std::result::Result<impl warp::Reply, warp::Rejection> {      

        let alerts_collection = self.database.collection::<Alert>("alerts");
        alerts_collection.insert_one(&alert, None).await.map_err(|_| warp::reject())?;

        {
            let alert = alert.clone();
            let mut alerts = self.alerts.lock().await;
            let regions = self.regions.lock().await;

            alert.setup(&self, &regions).await;

            alerts.insert(alert._id.clone(), alert);
        }

        Ok(json(&alert))
    }

    async fn alert_update(self, id: ObjectId, mut alert: Alert) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        alert._id = id.clone();

        let alerts_collection = self.database.collection::<Alert>("alerts");

        let alert_bson = mongodb::bson::to_document(&alert).unwrap();
        alerts_collection.update_one(doc!{"_id": id}, doc!{"$set": alert_bson}, None).await.map_err(|_| warp::reject())?;

        let mut alerts = self.alerts.lock().await;
        let regions = self.regions.lock().await;
        
        if let Some(alert) = alerts.remove(&id) {
            alert.teardown(&self).await;
        }

        {
            let alert = alert.clone();
            alert.setup(&self, &regions).await;
            alerts.insert(alert._id.clone(), alert);
        }

        Ok(json(&alert))
    }

    async fn alert_delete(self, id: ObjectId) -> std::result::Result<impl warp::Reply, warp::Rejection> {      

        let alerts_collection = self.database.collection::<Alert>("alerts");
        alerts_collection.delete_one(doc! {"_id": &id}, None).await.map_err(|_| warp::reject())?;

        let mut alerts = self.alerts.lock().await;
        if let Some(alert) = alerts.remove(&id) {
            let regions = self.regions.lock().await;

            alert.teardown(&self).await;
        }

        Ok(json(&serde_json::json!({"ok": true})))
    }

    async fn camera_image(self, camera: ObjectId) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        let cameras = self.sources.lock().await;
        if let Some(camera) = cameras.get(&camera) {
            for c in camera {
                if let Some(surf) = &c.inner.snapshot {
                    let encoded = surf.encode().await.map_err(|e| warp::reject())?;
                    let bytes = encoded.to_vec();
                    return Ok(bytes);
                }
            }
        }

        Err(warp::reject())
    }

    async fn list_regions(self, camera: ObjectId) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        let mut regions = Vec::new();

        let regions_collection = self.database.collection::<Region>("regions");
        let mut cursor = regions_collection.find(doc!{"camera_id": camera}, None).await.map_err(|_| warp::reject())?;

        while let Some(region) = cursor.try_next().await.map_err(|_| warp::reject())? {
            regions.push(region);
        }

        Ok(json(&regions))
    }

    async fn save_regions(self, camera: ObjectId, mut regions: Vec<Region>) -> std::result::Result<impl warp::Reply, warp::Rejection> {      

        let regions_collection = self.database.collection::<Region>("regions");
        regions_collection.delete_many(doc!{"camera_id": camera}, None).await.map_err(|_| warp::reject())?;

        for r in &mut regions {
            r.camera_id = camera;
        }

        regions_collection.insert_many(&regions, None).await.map_err(|_| warp::reject())?;

        let mut my_regions = self.regions.lock().await;
        if let Some(my_regions) = my_regions.get_mut(&camera) {
            for r in my_regions.iter_mut() {
                self.clear_region(r).await;
            }
        }

        my_regions.insert(camera, regions.clone());

        Ok(json(&regions))
    }

    async fn list_events(self, filter: EventFilter) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        let mut events = Vec::new();

        let events_collection = self.database.collection::<Event>("events");
        let find_options = FindOptions::builder().sort(doc! { "start": -1 }).limit(60).build();
        let mut query = doc!{};
        
        if let Some(start) = filter.start {
            query.insert("start", doc!{
                    "$lt": start
                }
            );
        }

        let mut cursor = events_collection.find(query, find_options).await.map_err(|_| warp::reject())?;
        while let Some(event) = cursor.try_next().await.map_err(|_| warp::reject())? {
            events.push(event);
        }

        Ok(json(&events))
    }

    async fn get_event(self, event_id: ObjectId) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        let events_collection = self.database.collection::<Event>("events");
        let event = events_collection.find_one(doc!{"_id": event_id}, None).await.map_err(|_| warp::reject())?;

        Ok(json(&event))
    }

    async fn list_events_range(self, filter: RangeFilter) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        let mut events = Vec::new();
        
        let query = doc! {
            "camera_id": {
                "$in": filter.source
            },
            "$or": [
                {
                    "start": {
                        "$lt": filter.to
                    }
                },
                {
                    "end": {
                        "$lt": filter.to
                    }
                },
            ]
        };

        let events_collection = self.database.collection::<Event>("events");

        let find_options = FindOptions::builder().sort(doc! { "end": -1 }).build();

        let mut pending: Option<EventGroup> = None;
        let buffer = std::time::Duration::from_secs(30);

        let mut cursor = events_collection.find(query, find_options).await.map_err(|e|warp::reject())?;
        while let Some(event) = cursor.try_next().await.map_err(|_| warp::reject())? {
            if let Some(mut pending_o) = pending {
                if (event.start <= pending_o.end && event.start >= pending_o.start) ||
                   (event.end <= pending_o.end && event.end >= pending_o.start) ||
                   ((event.end + buffer) <= pending_o.end) && ((event.end + buffer) >= pending_o.start) {
                    pending_o.start = if pending_o.start < event.start {pending_o.start} else {event.start};
                    pending_o.end = if pending_o.end > event.end {pending_o.end} else {event.end};
                    pending.replace(pending_o);
                } 
                else {
                    events.push(pending_o);
                    pending.replace(EventGroup {
                        start: event.start,
                        end: event.end
                    });
                }

            }
            else {
                pending.replace(EventGroup {
                    start: event.start,
                    end: event.end
                });
            }
            if event.start < filter.from && event.end < filter.from {
                break;
            }
        }
        if let Some(pending_o) = pending {
            events.push(pending_o);
        }

        Ok(json(&events))
    }

    async fn list_event_previous(self, filter: JumpFilter) -> std::result::Result<impl warp::Reply, warp::Rejection> {            
        let query = doc! {
            "camera_id": {
                "$in": filter.source
            },
            "$or": [
                {
                    "start": {
                        "$lt": filter.from
                    }
                },
                {
                    "end": {
                        "$lt": filter.from
                    }
                },
            ]
        };

        let events_collection = self.database.collection::<Event>("events");

        let find_options = FindOptions::builder().sort(doc! { "end": -1 }).build();

        let mut pending: Option<EventGroup> = None;
        let buffer = std::time::Duration::from_secs(30);
        let buffer2 = std::time::Duration::from_secs(10);

        let mut cursor = events_collection.find(query, find_options).await.map_err(|e|warp::reject())?;
        while let Some(event) = cursor.try_next().await.map_err(|_| warp::reject())? {
            if let Some(mut pending_o) = pending {
                if (event.start <= pending_o.end && event.start >= pending_o.start) ||
                   (event.end <= pending_o.end && event.end >= pending_o.start) ||
                   ((event.end + buffer) <= pending_o.end) && ((event.end + buffer) >= pending_o.start) {
                    pending_o.start = if pending_o.start < event.start {pending_o.start} else {event.start};
                    pending_o.end = if pending_o.end > event.end {pending_o.end} else {event.end};
                    pending.replace(pending_o);
                } 
                else {
                    if (pending_o.start <= filter.from) && ((pending_o.end + buffer2) >= filter.from) {
                        pending.replace(EventGroup {
                            start: event.start,
                            end: event.end
                        });

                    }
                    else {
                        break;

                    }
                }

            }
            else {
                pending.replace(EventGroup {
                    start: event.start,
                    end: event.end
                });
            }
        }

        Ok(json(&pending))
    }

    async fn list_event_next(self, filter: JumpFilter) -> std::result::Result<impl warp::Reply, warp::Rejection> {            
        let query = doc! {
            "camera_id": {
                "$in": filter.source
            },
            "$or": [
                {
                    "start": {
                        "$gt": filter.from
                    }
                },
                {
                    "end": {
                        "$gt": filter.from
                    }
                },
            ]
        };

        let events_collection = self.database.collection::<Event>("events");

        let find_options = FindOptions::builder().sort(doc! { "start": 1 }).build();

        let mut pending: Option<EventGroup> = None;
        let buffer = std::time::Duration::from_secs(30);
        let buffer2 = std::time::Duration::from_secs(10);

        let mut cursor = events_collection.find(query, find_options).await.map_err(|e|warp::reject())?;
        while let Some(event) = cursor.try_next().await.map_err(|_| warp::reject())? {
            if let Some(mut pending_o) = pending {
                if (event.start <= pending_o.end && event.start >= pending_o.start) ||
                   (event.end <= pending_o.end && event.end >= pending_o.start) ||
                   ((event.end + buffer) <= pending_o.end) && ((event.end + buffer) >= pending_o.start) {
                    pending_o.start = if pending_o.start < event.start {pending_o.start} else {event.start};
                    pending_o.end = if pending_o.end > event.end {pending_o.end} else {event.end};
                    pending.replace(pending_o);
                } 
                else {
                    if ((pending_o.start - buffer2) <= filter.from) && ((pending_o.end) >= filter.from) {
                        pending.replace(EventGroup {
                            start: event.start,
                            end: event.end
                        });

                    }
                    else {
                        break;

                    }
                }

            }
            else {
                pending.replace(EventGroup {
                    start: event.start,
                    end: event.end
                });
            }
        }

        Ok(json(&pending))
    }

    async fn get_detections(self, q: DetectionFilter) -> std::result::Result<impl warp::Reply, warp::Rejection> {      
        

        let start = q.at - std::time::Duration::from_secs(2);
        let end = q.at + std::time::Duration::from_secs(2);
        
        let query = doc! {
            "source": q.source,
            "$and": [
                {
                    "start": {"$gte": start}
                },
                {
                    "start": {"$lte": end}
                },
            ]
        };

        let mut detections: Vec<Detection> = Vec::new();

        let mut cursor = self.database.collection::<Detection>("detections").find(query, None).await.map_err(|_| warp::reject())?;
        while let Some(detection) = cursor.try_next().await.map_err(|_| warp::reject())? {
            detections.push(detection);
        }

        Ok(json(&detections))
    }
}