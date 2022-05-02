

use mongodb::bson::Bson;
use serde::{Deserialize, Serialize, Serializer};
use std::cmp::Ordering;
use std::fmt::Formatter;

use std::{time::Duration, ops::Deref};
use std::borrow::Cow;

//use poem::{http::HeaderValue};
use serde_json::Value;

/*use poem_openapi::{
    registry::{MetaSchema, MetaSchemaRef},
    types::{
        ParseFromJSON, ParseFromParameter, ParseResult,
        ToHeader, ToJSON, Type,
    },
};

*/

#[derive(Clone, Copy, Default)]
pub struct JsonDuration {
    duration: Duration
}


impl From<Duration> for JsonDuration {
    fn from(d: Duration) -> Self {
        Self{duration: d}
    }
}

impl From<gst::ClockTime> for JsonDuration {
    fn from(d: gst::ClockTime) -> Self {
        Self{duration: d.into()}
    }
}

impl From<JsonDuration> for Duration {
    fn from(d: JsonDuration) -> Self {
        d.duration
    }
}

impl PartialEq for JsonDuration {
    fn eq(&self, other: &Self) -> bool {
        PartialEq::eq(&self.duration, &other.duration)
    }
}


impl PartialEq<Duration> for JsonDuration {
    fn eq(&self, other: &Duration) -> bool {
        PartialEq::eq(&self.duration, other)
    }
}

impl PartialOrd for JsonDuration {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Duration::partial_cmp(&self.duration, &other.duration)
    }
}


impl PartialOrd<Duration> for JsonDuration {
    fn partial_cmp(&self, other: &Duration) -> Option<Ordering> {
        Duration::partial_cmp(&self.duration, other)
    }
}

impl std::ops::Sub for JsonDuration {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        JsonDuration{duration: self.duration.sub(rhs.duration)}
    }
}

impl std::ops::Sub<Duration> for JsonDuration {
    type Output = Self;
    fn sub(self, rhs: Duration) -> Self::Output {
        JsonDuration{duration: self.duration.sub(rhs)}
    }
}

impl std::ops::Add for JsonDuration {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        JsonDuration{duration: self.duration.add(rhs.duration)}
    }
}

impl std::ops::Add<Duration> for JsonDuration {
    type Output = Self;
    fn add(self, rhs: Duration) -> Self::Output {
        JsonDuration{duration: self.duration.add(rhs)}
    }
}

impl core::fmt::Debug for JsonDuration {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        self.duration.fmt(f)
    }
}

impl<'de> Deserialize<'de> for JsonDuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
            D: serde::Deserializer<'de> {
        let mut d: f64 = Deserialize::deserialize(deserializer)?;
        d *= 1000000.0;
        Ok(Self{
            duration: Duration::from_nanos(d as u64)
        })
    }
}

impl Serialize for JsonDuration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer {
        let d = self.duration.as_nanos() as f64 / 1000000.0;
        d.serialize(serializer)
    }
}

impl Deref for JsonDuration {
    type Target = Duration;
    fn deref(&self) -> &Self::Target {
        &self.duration
    }
}


impl From<JsonDuration> for Bson {
    fn from(d: JsonDuration) -> Self {
        (d.duration.as_nanos() as f64 / 1000000.0).into()
    }
}


/*
impl Type for JsonDuration {
    const IS_REQUIRED: bool = true;

    type RawValueType = Self;

    type RawElementValueType = Self;

    fn name() -> Cow<'static, str> {
        "duration".into()
    }

    fn schema_ref() -> MetaSchemaRef {
        MetaSchemaRef::Inline(Box::new(MetaSchema::new("integer")))
    }

    fn as_raw_value(&self) -> Option<&Self::RawValueType> {
        Some(self)
    }

    fn raw_element_iter<'a>(
        &'a self,
    ) -> Box<dyn Iterator<Item = &'a Self::RawElementValueType> + 'a> {
        Box::new(self.as_raw_value().into_iter())
    }
}

impl ParseFromJSON for JsonDuration {
    fn parse_from_json(value: Option<Value>) -> ParseResult<Self> {
        let v: JsonDuration = serde_json::from_value(value.unwrap_or_default())?;
        Ok(v)
    }
}

impl ParseFromParameter for JsonDuration {
    fn parse_from_parameter(value: &str) -> ParseResult<Self> {
        let i: f64 = value.parse()?;

        ParseResult::Ok(JsonDuration{duration: Duration::from_nanos((i * 1000000.0) as u64)})
    }
}


impl ToJSON for JsonDuration {
    fn to_json(&self) -> Option<Value> {
        Some(serde_json::to_value(self).unwrap())
    }
}

impl ToHeader for JsonDuration {
    fn to_header(&self) -> Option<HeaderValue> {
        HeaderValue::from_str(&(self.duration.as_nanos() as f64 / 1000000.0).to_string()).ok()
    }
}

*/