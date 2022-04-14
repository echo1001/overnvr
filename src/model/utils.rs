

use mongodb::bson::Bson;
use serde::{Deserialize, Serialize, Serializer};
use std::cmp::Ordering;
use std::fmt::Formatter;

use std::{time::Duration, marker::PhantomData, ops::Deref};


pub trait ConvertNumber {
    fn from_number(n: f64) -> Duration;

    fn to_number(d: &Duration) -> f64;

}

#[derive(Clone, Copy, Default)]
pub struct JsonDuration<T> where T: ConvertNumber {
    duration: Duration,
    sertype: PhantomData<T>
}


impl<T: ConvertNumber> From<Duration> for JsonDuration<T> {
    fn from(d: Duration) -> Self {
        Self{duration: d, sertype: PhantomData}
    }
}

impl<T: ConvertNumber> From<gst::ClockTime> for JsonDuration<T> {
    fn from(d: gst::ClockTime) -> Self {
        Self{duration: d.into(), sertype: PhantomData}
    }
}

impl<T: ConvertNumber> PartialEq for JsonDuration<T> {
    fn eq(&self, other: &Self) -> bool {
        PartialEq::eq(&self.duration, &other.duration)
    }
}

impl<T: ConvertNumber> PartialOrd for JsonDuration<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Duration::partial_cmp(&self.duration, &other.duration)
    }
}

impl<T: ConvertNumber> std::ops::Sub for JsonDuration<T> {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        JsonDuration{duration: self.duration.sub(rhs.duration), sertype: PhantomData}
    }
}

impl<T: ConvertNumber> std::ops::Sub<Duration> for JsonDuration<T> {
    type Output = Self;
    fn sub(self, rhs: Duration) -> Self::Output {
        JsonDuration{duration: self.duration.sub(rhs), sertype: PhantomData}
    }
}

impl<T: ConvertNumber> std::ops::Add for JsonDuration<T> {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        JsonDuration{duration: self.duration.add(rhs.duration), sertype: PhantomData}
    }
}

impl<T: ConvertNumber> std::ops::Add<Duration> for JsonDuration<T> {
    type Output = Self;
    fn add(self, rhs: Duration) -> Self::Output {
        JsonDuration{duration: self.duration.add(rhs), sertype: PhantomData}
    }
}

impl<T: ConvertNumber> core::fmt::Debug for JsonDuration<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        self.duration.fmt(f)
    }
}

impl<'de, T: ConvertNumber> Deserialize<'de> for JsonDuration<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
            D: serde::Deserializer<'de> {
        let d = Deserialize::deserialize(deserializer)?;
        Ok(Self{
            duration: T::from_number(d),
            sertype: PhantomData
        })
    }
}

impl<T: ConvertNumber> Serialize for JsonDuration<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer {
        let d = T::to_number(&self.duration);
        d.serialize(serializer)
    }
}

impl<T: ConvertNumber> Deref for JsonDuration<T> {
    type Target = Duration;
    fn deref(&self) -> &Self::Target {
        &self.duration
    }
}


impl<T: ConvertNumber> From<JsonDuration<T>> for Bson {
    fn from(d: JsonDuration<T>) -> Self {
        T::to_number(&d).into()
    }
}

#[derive(Clone, Copy, Default)]
pub struct MS {}

impl ConvertNumber for MS {
    fn from_number(n: f64) -> Duration {
        Duration::from_millis(n as u64)
    }
    fn to_number(d: &Duration) -> f64 {
        d.as_secs_f64() * 1000.0
    }
}