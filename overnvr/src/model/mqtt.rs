use serde::Serialize;

#[derive(Debug, Default, Serialize, Clone)]
pub struct Device {
    pub identifiers: String,
    pub name: String,
    pub sw_version: String,
    pub model: String,
    pub manufacturer: String,

}

#[derive(Debug, Default, Serialize, Clone)]
pub struct BinarySensor {
    pub name: String,
    pub uniq_id: String,
    pub state_topic: String,
    pub device_class: String,
    pub avty_t: String,
    pub json_attr_t: String,
    pub device: Device
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct Camera {
    pub name: String,
    pub uniq_id: String,
    pub t: String,
    pub avty_t: String,
    pub json_attr_t: String,
    pub device: Device
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Attr {
    State(bool),
    Event(String)
}
