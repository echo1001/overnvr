use std::path::PathBuf;
use std::str::FromStr;
use std::{collections::HashMap, hash::Hash};
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use crate::dvr::DVR;
use crate::source::SourceWeak;
use crate::surface::EncodedImage;
use rumqttc::{AsyncClient, QoS};
use serde::{Serialize, Deserialize, Serializer, Deserializer};

use mongodb::bson::oid::ObjectId;
use tokio::sync::{Mutex, watch::{channel, Sender, Receiver}};
use tokio_util::sync::{CancellationToken, DropGuard};

use geojson;
use geo_types::{Geometry, point};
use geo::prelude::*;
use std::convert::TryFrom;

use crate::detector::{BufferResult, BatchImage};
use crate::Result;
use crate::detector::model::DetectorConfig;

pub mod utils;
mod mqtt;

pub use utils::{JsonDuration, MS};

use self::mqtt::Attr;


#[derive(Deserialize,Clone)]
pub struct MQTTCredentials {
    pub username: String,
    pub password: String
}

#[derive(Deserialize,Clone)]
pub struct MQTT {

    pub client_id: String,
    pub host: String,
    pub port: u16,
    pub credentials: Option<MQTTCredentials>
}


#[derive(Deserialize, Clone)]
pub struct Storage {
    pub recordings: PathBuf,
    pub snapshots: PathBuf
}

#[derive(Deserialize, Clone)]
pub struct Config {
    pub database_url: String,
    pub database_name: String,
    pub enable_detector: bool,
    pub storage: Storage,
    pub detector: DetectorConfig,
    pub mqtt: MQTT
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Event {
    pub _id: ObjectId, // TODO: Store in database
    pub camera_id: ObjectId,
    pub start: JsonDuration<MS>,
    pub end: JsonDuration<MS>,
    pub thumb_start: String,
    pub region_name: String
}


#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Region {
    pub camera_id: ObjectId,
    pub region_name: String,
    pub classes: Vec<String>,
    pub min_confidence: f32,
    pub hold_time: JsonDuration<MS>,
    #[serde(deserialize_with = "deserialize_geojson", serialize_with = "serialize_geojson")]
    pub poly: geo_types::Geometry<f32>,

    #[serde(skip)]
    pub last_trigger: Option<JsonDuration<MS>>,
    #[serde(skip)]
    pub last_event: Option<Event>,
}

impl Into<RegionRef> for Region {
    fn into(self) -> RegionRef {
        RegionRef {
            camera_id: self.camera_id.clone(),
            region_name: self.region_name.clone()
        }
    }
}

impl Hash for Region {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.camera_id.hash(state);
        self.region_name.hash(state);
    }
}

impl Default for Region {
    fn default () -> Self {
        Region { 
            camera_id: ObjectId::new(), 
            region_name: "".to_owned(), 
            classes: Vec::new(), 
            min_confidence: 0 as f32, 
            hold_time: Default::default(), 
            poly: (point!(x: 1.0, y: 1.0)).into(),
            last_trigger: None,
            last_event: None
        }
    }
}

fn serialize_geojson<S>(o: &Geometry<f32>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer, {
    let geojson: geojson::Geometry = o.into();
    geojson.serialize(s)
}

fn deserialize_geojson<'de, D>(d: D) -> Result<Geometry<f32>, D::Error>
where
    D: Deserializer<'de>,
{
    let result = geojson::Geometry::deserialize(d)?;
    
    Ok(Geometry::<f32>::try_from(result).unwrap() )
}


#[derive(Serialize, Deserialize)]
pub struct Detection {
    pub source: ObjectId,
    pub start: JsonDuration<MS>,
    pub class_id: String,
    pub confidence: f32,
    #[serde(deserialize_with = "deserialize_geojson", serialize_with = "serialize_geojson")]
    pub region: geo_types::Geometry<f32>,
}

#[derive(Serialize, Deserialize)]
pub struct Detection_Json {
    pub start: JsonDuration<MS>,
    #[serde(deserialize_with = "deserialize_geojson", serialize_with = "serialize_geojson")]
    pub region: geo_types::Geometry<f32>,
}


#[derive(Deserialize, Serialize)]
pub struct DetectionFilter {
    pub source: ObjectId,
    pub at: JsonDuration<MS>,
}


#[derive(Deserialize)]
pub struct EventFilter {
    pub start: Option<i64>
}


#[derive(Deserialize)]
pub struct RangeFilter {
    #[serde(deserialize_with = "source_list_deserialize")]
    pub source: Vec<ObjectId>,
    
    pub from: JsonDuration<MS>,
    pub to: JsonDuration<MS>,
}

#[derive(Deserialize)]
pub struct ExportFilter {    
    pub from: JsonDuration<MS>,
    pub to: JsonDuration<MS>,
}

#[derive(Deserialize)]
pub struct JumpFilter {
    #[serde(deserialize_with = "source_list_deserialize")]
    pub source: Vec<ObjectId>,    
    pub from: JsonDuration<MS>,
}

#[derive(Serialize, Clone, Copy)]
pub struct EventGroup {
    pub start: JsonDuration<MS>,
    pub end: JsonDuration<MS>,
}

fn source_list_deserialize<'de, D>(deserializer: D) -> Result<Vec<ObjectId>, D::Error>
where
    D: Deserializer<'de>,
{
    let str_sequence = String::deserialize(deserializer)?;
    let list = str_sequence.split(',');

    let mut oids = Vec::new();
    for s in list {
        oids.push(ObjectId::from_str(s).map_err(|_| serde::de::Error::custom("Not a valid oid"))?);
    }
    Ok(oids)
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct RegionRef {
    pub camera_id: ObjectId,
    pub region_name: String,
}

impl Hash for RegionRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.camera_id.hash(state);
        self.region_name.hash(state);
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Alert {
    pub _id: ObjectId,
    pub alert_name: String,
    pub regions: Vec<RegionRef>
}

impl PartialEq<RegionRef> for Region {
    fn eq(&self, other: &RegionRef) -> bool {
        self.camera_id == other.camera_id && self.region_name == other.region_name
    }
}

impl Alert {
    pub async fn setup(&self, dvr: &DVR, regions: &HashMap<ObjectId, Vec<Region>>) -> Result<()> {
        let mut regions_alert = dvr.region_alert.lock().await;
        for s in &self.regions {
            let alerts = if let Some(alerts) = regions_alert.get_mut(s) {
                alerts
            } else {
                regions_alert.insert(s.clone(), Vec::new());
                regions_alert.get_mut(s).unwrap()
            };

            alerts.retain(|&x| x != self._id);
            alerts.push(self._id.clone());
        }

        /* Add Motion Sensor */
        let motion_sensor = mqtt::BinarySensor {
            name: self.alert_name.clone(),
            uniq_id: self._id.to_string(),
            state_topic: format!("homeassistant/binary_sensor/{}/state", self._id.to_string()),
            device_class: "motion".to_string(),
            avty_t: "overnvr_rust/available".to_string(),
            json_attr_t: format!("homeassistant/binary_sensor/{}/attr", self._id.to_string()),
            device: mqtt::Device {
                identifiers: self._id.to_string(),
                name: self.alert_name.clone(),
                sw_version: "v0.0.1".to_string(),
                model: "OverNVR Rust".to_string(),
                manufacturer: "echo1001".to_string()
            }
        };

        dvr.mqtt.publish(format!("homeassistant/binary_sensor/{}/config", self._id.to_string()), 
            QoS::AtLeastOnce, 
            true, 
            serde_json::to_string(&motion_sensor)?).await?;

        /* Image */
        let camera = mqtt::Camera {
            name: self.alert_name.clone(),
            uniq_id: format!("{}_image", self._id.to_string()),
            t: format!("camera/{}_image/image", self._id.to_string()),
            avty_t: "overnvr_rust/available".to_string(),
            json_attr_t: format!("homeassistant/binary_sensor/{}/attr", self._id.to_string()),
            device: mqtt::Device {
                identifiers: self._id.to_string(),
                name: self.alert_name.clone(),
                sw_version: "v0.0.1".to_string(),
                model: "OverNVR Rust".to_string(),
                manufacturer: "echo1001".to_string()
            }
        };

        dvr.mqtt.publish(format!("homeassistant/camera/{}_image/config", self._id.to_string()), 
            QoS::AtLeastOnce, 
            true, 
            serde_json::to_string(&camera)?).await?;

        self.send_update(dvr, regions, None).await?;

        Ok(())

    }

    pub async fn teardown(&self, dvr: &DVR) -> Result<()> {
        let mut regions_alert = dvr.region_alert.lock().await;
        for s in &self.regions {
            if let Some(alerts) = regions_alert.get_mut(s) {
                alerts.retain(|&x| x != self._id);
            }
        }

        dvr.mqtt.publish(format!("homeassistant/binary_sensor/{}/config", self._id.to_string()), 
            QoS::AtLeastOnce, 
            true, 
            "".to_owned()).await?;

        dvr.mqtt.publish(format!("homeassistant/camera/{}_image/config", self._id.to_string()), 
            QoS::AtLeastOnce, 
            true, 
            "".to_owned()).await?;

        Ok(())
    }

    pub async fn send_update(&self, dvr: &DVR, regions: &HashMap<ObjectId, Vec<Region>>, image: Option<Arc<EncodedImage>>) -> Result<()>{
        
        let mut states = HashMap::new();
        let mut triggered = false;

        for r in &self.regions {
            states.insert(r.region_name.clone(), Attr::State(false));
        }
        states.insert("last_event".to_owned(), Attr::Event("".to_owned()));

        for r in &self.regions {
            if let Some(cr) = regions.get(&r.camera_id) {
                for cr in cr {
                    if cr == r {
                        if let Some(event) = &cr.last_event {
                            triggered = true;
                            states.insert(cr.region_name.clone(), Attr::State(true));
                            states.insert("last_event".to_owned(), Attr::Event(event._id.to_string()));
                        }
                    }
                }
            }
        }

        if let Some(image) = image {
            dvr.mqtt.publish(format!("camera/{}_image/image", self._id.to_string()), 
                QoS::AtLeastOnce, 
                true, 
                image.to_vec()
            ).await?;
        }

        dvr.mqtt.publish(format!("homeassistant/binary_sensor/{}/attr", self._id.to_string()), 
            QoS::AtLeastOnce, 
            true, 
            serde_json::to_string(&states)?
        ).await?;

        dvr.mqtt.publish(format!("homeassistant/binary_sensor/{}/state", self._id.to_string()), 
            QoS::AtLeastOnce, 
            true, 
            match triggered {true => "ON", false => "OFF"}
        ).await?;

        Ok(())
    }
/*
    async fn setup_mqtt(&self, mqtt: &AsyncClient) -> Result<()> {


        self.send_update(mqtt, false, states, None).await?;


        Ok(())
    }
    async fn teardown_mqtt(&self, mqtt: &AsyncClient) -> Result<()> {
    }*/
}
