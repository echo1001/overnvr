use crate::Result;
use anyhow::bail;
use chrono::Utc;
use rand::rngs::{OsRng};
use rand::RngCore;
use mongodb::{
    bson::{doc, oid::ObjectId},
    IndexModel, Collection, options::{FindOneOptions, FindOptions}, Cursor
};
use sha2::Digest;
use sha2::Sha512;
use crate::model::*;

use std::time::Duration;

use futures_util::TryStreamExt;

#[derive(Clone)]
pub struct Database {
    #[allow(unused)]
    db: mongodb::Database,
    recordings: Collection<Recording>,
    detections: Collection<Detection>,
    events: Collection<Event>,
    cameras: Collection<SourceConfig>,
    regions: Collection<Region>,
    alerts: Collection<Alert>,
    users: Collection<User>

}

impl Database {
    pub async fn new(url: &String, name: &String) -> Result<Database> {
        let client = mongodb::Client::with_uri_str(url.clone()).await?;
        let database = client.database(name);


        let recordings = database.collection::<Recording>("recordings");

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

        let detections = database.collection::<Detection>("detections");

        let index = IndexModel::builder()
            .keys(doc! {"source": 1, "start": 1})
            .build();
        detections.create_index(index, None).await?;


        let events = database.collection::<Event>("events");

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

        let cameras = database.collection("cameras");
        let regions = database.collection("regions");
        let alerts = database.collection("alerts");

        let index = IndexModel::builder()
            .keys(doc! {"username": 1})
            .build();

        let users = database.collection("users");
        users.create_index(index, None).await?;

        let database = Database {
            db: database,
            recordings,
            detections,
            events,
            cameras,
            regions,
            alerts,
            users
        };

        database.check_default().await?;

        Ok(database)
    }

    /* User */
    pub async fn check_default(&self) -> Result<()> {
        if self.users.find_one(None, None).await?.is_none() {
            self.create_user("admin".to_owned(), "admin".to_owned(), true).await?;
        }

        Ok(())
    }

    pub async fn create_user(&self, username: String, password: String, admin: bool) -> Result<()> {
        let mut salt = [0u8; 64];
        OsRng.fill_bytes(&mut salt);

        let mut hasher = Sha512::new();
        hasher.update(password.as_bytes());
        hasher.update(&salt);
        let password = hasher.finalize().to_vec();

        let user = User {
            username, 
            pass: password,
            salt: salt.to_vec(),
            admin,
            ..Default::default()
        };

        self.users.insert_one(user, None).await?;

        Ok(())
    }

    pub async fn login(&self, username: String, password: String) -> Result<Option<UserAuth>> {
        if let Some(user) = self.users.find_one(doc!{"username": username}, None).await? {
            let mut hasher = Sha512::new();
            hasher.update(password.as_bytes());
            hasher.update(&user.salt);
            let result = hasher.finalize().to_vec();

            if user.pass == result {
                Ok(Some(user.into()))
            } else {
                Ok(None)
            }
        }
        else {
            Ok(None)
        }
    }

    pub async fn check_auth(&self, user: &mut UserAuth) -> Result<bool> {
        if Utc::now() > user.expires {
            return Ok(false);
        }
        if let Some(user_data) = self.users.find_one(doc!{"username": &user.username}, None).await? {
            user.admin = user_data.admin;
            Ok(true)
        }
        else {
            Ok(false)
        }
    }

    pub async fn check_token(&self, user: &WSToken) -> Result<bool> {
        if Utc::now() > user.expires {
            return Ok(false);
        }
        if let Some(_) = self.users.find_one(doc!{"username": &user.username}, None).await? {
            Ok(true)
        }
        else {
            Ok(false)
        }
    }

    /* Events */
    pub async fn add_event(&self, event: Event) -> Result<()> {
        self.events.insert_one(event, None).await?;

        Ok(())
    }

    pub async fn get_event(&self, id: ObjectId) -> Result<Option<Event>> {
        let event = self.events.find_one(doc!{"_id": id}, None).await?;
        Ok(event)
    }

    pub async fn update_event(&self, event: &Event, pts: &Duration) -> Result<()> {
        let query = doc! {"_id": event._id.clone()};
        let update = doc! {"$set": {"end": pts.as_millis() as i64}};

        self.events.update_one(query, update, None).await?;

        Ok(())
    }

    pub async fn list_events(&self, start: Option<JsonDuration>) -> Result<Vec<Event>> {
        let mut events = Vec::new();

        let find_options = FindOptions::builder().sort(doc! { "start": -1 }).limit(60).build();
        let mut query = doc!{};
        
        if let Some(start) = start {
            query.insert("start", doc!{
                    "$lt": start
                }
            );
        }

        let mut cursor = self.events.find(query, find_options).await?;
        while let Some(event) = cursor.try_next().await? {
            events.push(event);
        }

        Ok(events)
    }

    pub async fn list_events_range(&self, source: Vec<ObjectId>, from: JsonDuration, to: JsonDuration) -> Result<Vec<EventGroup>> {      
        let mut events = Vec::new();
        
        let query = doc! {
            "camera_id": {
                "$in": source
            },
            "$or": [
                {
                    "start": {
                        "$lt": to
                    }
                },
                {
                    "end": {
                        "$lt": to
                    }
                },
            ]
        };


        let find_options = FindOptions::builder().sort(doc! { "end": -1 }).build();

        let mut pending: Option<EventGroup> = None;
        let buffer = std::time::Duration::from_secs(30);

        let mut cursor = self.events.find(query, find_options).await?;
        while let Some(event) = cursor.try_next().await? {
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
            if event.start < from && event.end < from {
                break;
            }
        }
        if let Some(pending_o) = pending {
            events.push(pending_o);
        }

        Ok(events)
    }

    pub async fn list_event_previous(&self, source: Vec<ObjectId>, from: JsonDuration) -> Result<Option<EventGroup>> {      
        let query = doc! {
            "camera_id": {
                "$in": source
            },
            "$or": [
                {
                    "start": {
                        "$lt": from
                    }
                },
                {
                    "end": {
                        "$lt": from
                    }
                },
            ]
        };

        let find_options = FindOptions::builder().sort(doc! { "end": -1 }).build();

        let mut pending: Option<EventGroup> = None;
        let buffer = std::time::Duration::from_secs(30);
        let buffer2 = std::time::Duration::from_secs(10);

        let mut cursor = self.events.find(query, find_options).await?;
        while let Some(event) = cursor.try_next().await? {
            if let Some(mut pending_o) = pending {
                if (event.start <= pending_o.end && event.start >= pending_o.start) ||
                   (event.end <= pending_o.end && event.end >= pending_o.start) ||
                   ((event.end + buffer) <= pending_o.end) && ((event.end + buffer) >= pending_o.start) {
                    pending_o.start = if pending_o.start < event.start {pending_o.start} else {event.start};
                    pending_o.end = if pending_o.end > event.end {pending_o.end} else {event.end};
                    pending.replace(pending_o);
                } 
                else {
                    if (pending_o.start <= from) && ((pending_o.end + buffer2) >= from) {
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

        Ok(pending)
    }

    pub async fn list_event_next(&self, source: Vec<ObjectId>, from: JsonDuration) -> Result<Option<EventGroup>> {      
        let query = doc! {
            "camera_id": {
                "$in": source
            },
            "$or": [
                {
                    "start": {
                        "$gt": from
                    }
                },
                {
                    "end": {
                        "$gt": from
                    }
                },
            ]
        };

        let find_options = FindOptions::builder().sort(doc! { "start": 1 }).build();

        let mut pending: Option<EventGroup> = None;
        let buffer = std::time::Duration::from_secs(30);
        let buffer2 = std::time::Duration::from_secs(10);

        let mut cursor = self.events.find(query, find_options).await?;
        while let Some(event) = cursor.try_next().await? {
            if let Some(mut pending_o) = pending {
                if (event.start <= pending_o.end && event.start >= pending_o.start) ||
                   (event.end <= pending_o.end && event.end >= pending_o.start) ||
                   ((event.end + buffer) <= pending_o.end) && ((event.end + buffer) >= pending_o.start) {
                    pending_o.start = if pending_o.start < event.start {pending_o.start} else {event.start};
                    pending_o.end = if pending_o.end > event.end {pending_o.end} else {event.end};
                    pending.replace(pending_o);
                } 
                else {
                    if ((pending_o.start - buffer2) <= from) && ((pending_o.end) >= from) {
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

        Ok(pending)
    }

    /* Detections */
    pub async fn add_detections(&self, detections: Vec<Detection>) -> Result<()> {
        self.detections.insert_many(detections, None).await?;

        Ok(())
    }

    pub async fn get_detections(&self, source: Vec<ObjectId>, at: JsonDuration) -> Result<Vec<Detection>> {
        let range: JsonDuration = std::time::Duration::from_secs(2).into();
        if at <= Duration::from_secs(10) {
            bail!("Invalid range");
        }
        let start = at - range;
        let end = at + range;
        
        let query = doc! {
            "source": {
                "$in": source
            },
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

        let mut cursor = self.detections.find(query, None).await?;
        while let Some(detection) = cursor.try_next().await? {
            detections.push(detection);
        }

        Ok(detections)
    }

    /* Recordings */
    pub async fn add_recording(&self, source: &ObjectId, location: &String, time: u64) {
        let recording = Recording {
            _id: ObjectId::new(),
            end: std::time::Duration::from_secs(0).into(),
            file: location.clone(),
            size: 0,
            source: source.clone(),
            start: std::time::Duration::from_nanos(time).into()
        };

        self.recordings.insert_one(&recording, None).await;
    }

    pub async fn finish_recording(&self, source: &ObjectId, location: &String, time: u64, size: u64) {
        let file = doc! {
            "file": location,
            "source": source
        };

        let end: JsonDuration = std::time::Duration::from_nanos(time).into();
        let update = doc! {
            "$set": {
                "size": size as i64,
                "end": end
            }
        };
        self.recordings.update_one(file, update, None).await;
    }


    pub async fn get_recordings(&self, source: &ObjectId, from: &JsonDuration, to: Option<&JsonDuration>) -> Result<Cursor<Recording>> {       
            
        let recording_filter = doc! {
            "start": {
                "$lte": from
            },
            "source": source
        };
        let find_options = FindOneOptions::builder().sort(doc! { "start": -1 as i32 }).build();

        let recording = self.recordings.find_one(recording_filter, find_options).await?;
        if let Some(recording) = recording {
            let mut and_ops = Vec::new();
            and_ops.push(doc!{"start": {
                "$gte": recording.start
            }});

            if let Some(to) = to {
                and_ops.push(doc!{
                    "start": {
                        "$lte": to
                    },
                });
            }

            let recording_filter = doc! {
                "$and": and_ops,
                "source": source
            };
            let find_options = FindOptions::builder().sort(doc! { "start": 1 as i32 }).build();
            Ok(self.recordings.find(recording_filter, find_options).await?)
            
        } else {
            bail!("No recordings found");
        }
    }

    /* Cameras */
    pub async fn list_cameras(&self) -> Result<Vec<SourceConfig>> {
        let mut cameras = Vec::new();

        let mut cursor = self.cameras.find(doc! {"enable": true}, None).await?;
        while let Some(camera) = cursor.try_next().await? {
            cameras.push(camera);
        }

        Ok(cameras)
    }

    pub async fn get_camera(&self, id: ObjectId) -> Result<Option<SourceConfig>> {
        let camera = self.cameras.find_one(doc!{"_id": id}, None).await?;
        Ok(camera)
    }

    pub async fn update_camera(&self, config: &SourceConfig) -> Result<()> {

        let camera_bson = mongodb::bson::to_document(config).unwrap();
        let update = doc! {
            "$set": camera_bson
        };

        self.cameras.update_one(doc!{"_id": config._id}, update, None).await?;

        Ok(())
    }

    /* Region */
    pub async fn list_regions_camera(&self, id: ObjectId) -> Result<Vec<Region>> {
        let mut regions = Vec::new();

        let mut cursor = self.regions.find(doc!{"camera_id": id}, None).await?;
        while let Some(region) = cursor.try_next().await? {
            regions.push(region);
        }

        Ok(regions)
    }

    pub async fn list_regions(&self) -> Result<Vec<Region>> {
        let mut regions = Vec::new();

        let mut cursor = self.regions.find(None, None).await?;
        while let Some(region) = cursor.try_next().await? {
            regions.push(region);
        }

        Ok(regions)
    }

    pub async fn update_regions(&self, id: ObjectId, regions: &mut Vec<Region>) -> Result<()> {
        self.regions.delete_many(doc!{"camera_id": id}, None).await?;

        for r in regions.iter_mut() {
            r.camera_id = id;
        }

        self.regions.insert_many(regions, None).await?;

        Ok(())
    }

    /* Alerts */
    pub async fn list_alerts(&self) -> Result<Vec<Alert>> {
        let mut alerts = Vec::new();

        let mut cursor = self.alerts.find(None, None).await?;
        while let Some(alert) = cursor.try_next().await? {
            alerts.push(alert);
        }

        Ok(alerts)
    }

    pub async fn add_alert(&self, alert: &Alert) -> Result<()> {
        self.alerts.insert_one(alert, None).await?;
        Ok(())
    }

    pub async fn update_alert(&self, alert: &Alert) -> Result<()> {
        let alert_bson = mongodb::bson::to_document(&alert).unwrap();
        self.alerts.update_one(doc!{"_id": alert._id}, doc!{"$set": alert_bson}, None).await?;
        Ok(())
    }

    pub async fn delete_alert(&self, id: &ObjectId) -> Result<()> {
        self.alerts.delete_one(doc!{"_id": id}, None).await?;
        Ok(())
    }
}