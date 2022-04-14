use std::path::PathBuf;

use serde::{Serialize, Deserialize, Deserializer};

#[derive(Deserialize, Clone, Copy, Serialize)]
pub struct ClassSettings {
    pub pre_cluster: f32,
    pub nms_iou: f32,
}

#[derive(Deserialize, Clone)]
pub struct DetectorConfig {
    pub gpuID: ::std::os::raw::c_uint,
    pub networkScaleFactor: f32,

    #[serde(deserialize_with = "deserialize_path")]
    pub customNetworkConfigFilePath: [::std::os::raw::c_char; 4096usize],
    #[serde(deserialize_with = "deserialize_path")]
    pub modelFilePath: [::std::os::raw::c_char; 4096usize],
    #[serde(deserialize_with = "deserialize_path")]
    pub modelEngineFilePath: [::std::os::raw::c_char; 4096usize],
    pub maxBatchSize: ::std::os::raw::c_uint,
    pub networkMode: crate::ffi::NvDsInferNetworkMode,
    pub numDetectedClasses: ::std::os::raw::c_uint,
    pub clusterMode: crate::ffi::NvDsInferClusterMode,

    #[serde(deserialize_with = "deserialize_string")]
    pub customBBoxParseFuncName: [::std::os::raw::c_char; 1024usize],
    
    #[serde(deserialize_with = "deserialize_path")]
    pub customLibPath: [::std::os::raw::c_char; 4096usize],

    #[serde(deserialize_with = "deserialize_string")]
    pub customEngineCreateFuncName: [::std::os::raw::c_char; 1024usize],
    
    pub class_keys: Vec<String>,
    pub classes: std::collections::HashMap<String, ClassSettings>,
    /*#[serde()]
    pub gpu_id: u32,
    pub scale_factor: f32,
    pub network_mode: u32,
    pub num_classes: u32,
    pub max_batch_size: u32,
    pub output_pool_size: u32,
    pub workspace_size: u64,
    pub engine_path: PathBuf,
    pub onnx_path: PathBuf,
    pub custom_lib: PathBuf,
    pub custom_parse: String,
    pub class_keys: Vec<String>,
    pub classes: std::collections::HashMap<String, ClassSettings>,

    #[serde(deserialize_with = "deserialize_path")]
    pub test: [::std::os::raw::c_char; 4096usize],*/
}

fn deserialize_path<'de, D>(deserializer: D) -> Result<[::std::os::raw::c_char; 4096usize], D::Error>
where
	D: Deserializer<'de>,
{
    let data = String::deserialize(deserializer)?;
    
    let mut fp = std::env::current_dir().unwrap();
    fp.push(data);
    let p = fp.to_str().unwrap();

    let mut out = [0 as ::std::os::raw::c_char; 4096usize];

    unsafe {
        let o: *mut u8 = std::mem::transmute(&mut out);
        std::ptr::copy(p.as_ptr(), o, p.len())
    }
    Ok(out)
}

fn deserialize_string<'de, D>(deserializer: D) -> Result<[::std::os::raw::c_char; 1024usize], D::Error>
where
	D: Deserializer<'de>,
{
    let data = String::deserialize(deserializer)?;

    let mut out = [0 as ::std::os::raw::c_char; 1024usize];

    unsafe {
        let o: *mut u8 = std::mem::transmute(&mut out);
        std::ptr::copy(data.as_ptr(), o, data.len())
    }
    Ok(out)
}