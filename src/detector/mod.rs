pub mod model;
use crate::Result;
use crate::surface::{Surface, EncodedImage};

use glib::clone::Downgrade;
use model::DetectorConfig;
use std::collections::HashMap;
use std::ffi::CStr;
use std::fmt::{Pointer, Write};
use std::sync::{Arc, Weak};
use std::sync::atomic::AtomicU64;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::ffi::{
    NvDsInferContextInitParams,
    NvDsInferContext_Create,
    NvDsInferContext_GetNetworkInfo,
    NvDsInferContext_Destroy,
    NvDsInferContext_QueueInputBatch,
    NvDsInferContext_DequeueOutputBatch,
    NvDsInferContext_ReleaseBatchOutput,


    NvDsInferContextHandle,
    NvDsInferNetworkInfo,
    NvDsInferDetectionParams,
    NvDsInferDetectionParams__bindgen_ty_1,
    NvDsInferFormat_RGBA,
    NvDsInferContextBatchInput,
    NvDsInferContextBatchOutput,

    NvDsInferNetworkType_Detector,
    NVDSINFER_CLUSTER_NMS,
    NvDsInferLogLevel

};

pub struct InferContext {
    context: NvDsInferContextHandle,
    net_info: NvDsInferNetworkInfo,
    config: DetectorConfig,    
}

impl Drop for InferContext {
    fn drop(&mut self) {
        unsafe {
            NvDsInferContext_Destroy(self.context);
        }
    }
}
unsafe impl Send for InferContext {}
unsafe impl Sync for InferContext {}

unsafe extern "C" fn logger(
    _: NvDsInferContextHandle,
    _: ::std::os::raw::c_uint,
    _: NvDsInferLogLevel,
    log_message: *const ::std::os::raw::c_char,
    _: *mut ::std::os::raw::c_void,
) {
    let str = CStr::from_ptr(log_message);
    let str_slice: &str = str.to_str().unwrap();
    let str_buf: String = str_slice.to_owned();
    
    println!("{}", str_buf);
}

unsafe extern "C" fn release_batch(_: *mut std::ffi::c_void) {

}

impl InferContext {
    pub async fn new(config: DetectorConfig) -> Result<InferContext> {
        let infer = tokio::task::spawn_blocking(move || {
            let mut params = unsafe { std::mem::zeroed::<NvDsInferContextInitParams>() };
            
            params.gpuID                                = config.gpuID;
            params.networkScaleFactor                   = config.networkScaleFactor;
            params.customNetworkConfigFilePath          = config.customNetworkConfigFilePath;
            params.modelFilePath                        = config.modelFilePath;
            params.modelEngineFilePath                  = config.modelEngineFilePath;
            params.maxBatchSize                         = config.maxBatchSize;
            params.networkMode                          = config.networkMode;
            params.numDetectedClasses                   = config.numDetectedClasses;
            params.clusterMode                          = config.clusterMode;
            params.customBBoxParseFuncName              = config.customBBoxParseFuncName;
            params.customLibPath                        = config.customLibPath;
            params.customEngineCreateFuncName           = config.customEngineCreateFuncName;
            params.outputBufferPoolSize = 10;
            params.uniqueID = 1;
            params.networkType = NvDsInferNetworkType_Detector;
            params.workspaceSize = 4096;

            let mut classes: Vec<NvDsInferDetectionParams> = Vec::new();
            for _ in 0..params.numDetectedClasses {
                classes.push(NvDsInferDetectionParams {
                    __bindgen_anon_1: NvDsInferDetectionParams__bindgen_ty_1 {
                        preClusterThreshold: 0.2,
                    },
                    postClusterThreshold: 0.0,
                    eps: 0.0,
                    groupThreshold: 0,
                    minBoxes: 0,
                    minScore: 0.0,
                    nmsIOUThreshold: 0.3,
                    topK: -1,
                })
            }
            if let Some(all) = config.classes.get("all".into()) {
                for i in classes.iter_mut() {
                    i.__bindgen_anon_1.preClusterThreshold = all.pre_cluster;

                    i.nmsIOUThreshold = all.nms_iou;
                }
            }
            let mut classes_boxed = classes.into_boxed_slice();
            params.perClassDetectionParams = classes_boxed.as_mut_ptr();

            unsafe {
                let mut handle = std::mem::MaybeUninit::zeroed();
                NvDsInferContext_Create(handle.as_mut_ptr(), &mut params, 0 as *mut std::ffi::c_void, Some(logger));

                let handle = handle.assume_init();
                let mut net_info = std::mem::MaybeUninit::zeroed();
                NvDsInferContext_GetNetworkInfo(handle, net_info.as_mut_ptr());
                let net_info = net_info.assume_init();
                
                Result::<InferContext>::Ok(
                    InferContext{ 
                        context: handle,
                        net_info,
                        config,
                    }
                )
            }
        }).await??;

        Ok(infer)
    }
}

pub struct BatchOutput {
    ctx: Arc<InferContext>,
    batch: NvDsInferContextBatchOutput
}
unsafe impl Sync for BatchOutput {}
unsafe impl Send for BatchOutput {}
impl Drop for BatchOutput {
    fn drop(&mut self) {
        unsafe {
            NvDsInferContext_ReleaseBatchOutput(self.ctx.context, &mut self.batch);
        }
    }
}

pub struct Detection {
    pub top: f32,
    pub left: f32,
    pub width: f32,
    pub height: f32,
    pub confidence: f32,
    pub class_id: String,

    pub rect: geo_types::Geometry<f32>,
    pub foot: geo_types::Geometry<f32>,
}

impl std::fmt::Debug for Detection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("Detection(class_id={}, confidence={})", self.class_id, self.confidence ))
    }
}

pub struct BufferResult<T: Sync + Send + Clone> {
    pub detections: Vec<Detection>,
    pub image: Arc<BatchImage<T>>,
    encoded: Mutex<Option<Arc<EncodedImage>>>
}
 
impl<T: Sync + Send + Clone> BufferResult<T> {
    pub fn get_source(&self) -> T {
        self.image.source.clone()
    }

    pub fn get_pts(&self) -> Option<gst::ClockTime> {
        self.image.source_frame.pts()
    }

    pub async fn crop(&self, surf: &Surface) -> Result<Arc<EncodedImage>> {
        let mut encoded = self.encoded.lock().await;
        if let Some(encoded) = &*encoded {
            Ok(encoded.clone())
        } else {
            let max_width = self.image.source_frame.width() as f32;
            let max_height = self.image.source_frame.height() as f32;

            println!("{} {}", max_width, max_height);
            let mut x0 = self.image.source_frame.width() as f32;
            let mut y0 = self.image.source_frame.height() as f32;
            let mut x1: f32 = 0.0;
            let mut y1: f32 = 0.0;
            
            for d in &self.detections {
                if d.confidence >= 0.7 {
                    x0 = x0.min(d.left);
                    y0 = y0.min(d.top);

                    x1 = x1.max(d.left + d.width);
                    y1 = y1.max(d.top + d.height);
                }
            }

            let mut left = x0;
            let mut top = y0;
            let mut width = x1 - x0;
            let mut height = y1 - y0;

            let box_size = width.max(height) * 1.2;
            let mut cx = left + (width / 2.0);
            let mut cy = top + (height / 2.0);
            let bc = box_size / 2.0;

            cx = (cx - bc).max(0.0) + bc;
            cy = (cy - bc).max(0.0) + bc;

            cx = (cx + bc).min(max_width) - bc;
            cy = (cy + bc).min(max_height) - bc;

            width = box_size;
            height = box_size;
            if (cx - bc) < 0.0 {
                width = max_width;
                cx = width / 2.0;
            }
        
            if (cy - bc) < 0.0 {
                height = max_height;
                cy = height / 2.0;
            }
        
            left = cx - (width / 2.0);
            top = cy - (height / 2.0);

            
            self.image.source_frame.crop(&surf, left as u32, top as u32, width as u32, height as u32).await?;
            let _encoded = surf.encode().await?;
            encoded.replace(_encoded.clone());
            Ok(_encoded)
        }
    }
}

impl<T: Sync + Send + Clone> std::fmt::Debug for BufferResult<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("BufferResult(pts={:?}, detection={:?})", self.get_pts(), self.detections))
    }
}

pub struct DetectorInner<T: 'static + Sync + Send + Clone> {
    startup_task: Mutex<Option<JoinHandle<Result<()>>>>,
    input_task:   Mutex<Option<JoinHandle<Result<()>>>>,
    output_task:  Mutex<Option<JoinHandle<Result<()>>>>,

    result_sender: Sender<Vec<Arc<BufferResult<T>>>>,
    result_receiver: Mutex<Receiver<Vec<Arc<BufferResult<T>>>>>,

    pool_sender: Mutex<Option<Sender<Surface>>>,
    pool_receiver: Mutex<Option<Receiver<Surface>>>,

    frame_ready: Notify,

    sources: Mutex<HashMap<u64, Weak<Source<T>>>>,
    next_source_id: AtomicU64

}

pub struct BatchImage<T: Sync + Send + Clone> {
    infer_frame: Surface,
    source_frame: Surface,
    pool: Sender<Surface>,
    #[allow(dead_code)]
    source: T
}

impl<T: Sync + Send + Clone> Drop for BatchImage<T> {
    fn drop(&mut self) {
        let _ = self.pool.try_send(self.infer_frame.clone());
    }
}

#[derive(Clone)]
pub struct Detector<T: 'static + Sync + Send + Clone> {
    inner: Arc<DetectorInner<T>>
}

impl<T: 'static + Sync + Send + Clone> Detector<T> {
    pub fn new() -> Detector<T> {
        let (s, r) = channel(10);

        Detector {
            inner: Arc::new(DetectorInner {
                startup_task: Mutex::new(None),
                input_task: Mutex::new(None),
                output_task: Mutex::new(None),
                
                result_sender: s,
                result_receiver: Mutex::new(r),

                pool_receiver: Mutex::new(None),
                pool_sender: Mutex::new(None),

                frame_ready: Notify::new(),

                sources: Mutex::new(HashMap::new()),
                next_source_id: AtomicU64::new(0)
            })
        }
    }

    

    pub async fn queue_batch(ctx: &Arc<InferContext>, batch: Vec<Arc<BatchImage<T>>>) -> Result<()>{


        tokio::task::spawn_blocking({

            let infer = ctx.clone();
            move || {
                let mut ptrs = Vec::new();
                let mut pitch: u32 = 0; 
                for b in &batch {
                    if let Some(ptr) = b.infer_frame.get_ptr() {
                        ptrs.push(ptr);
                    }
                    pitch = b.infer_frame.pitch();
                }
    
                let num = ptrs.len();
                let mut vb = ptrs.into_boxed_slice();
                let vbptr = vb.as_mut_ptr();

                let mut input = NvDsInferContextBatchInput{
                    inputFormat: NvDsInferFormat_RGBA,
                    inputFrames: vbptr,
                    numInputFrames: num as u32,
                    returnFuncData: 0 as *mut std::ffi::c_void,
                    returnInputFunc: Some(release_batch),
                    // surf2->surfaceList[0].planeParams.pitch[0];
                    inputPitch: pitch,
                };
                unsafe {
                    NvDsInferContext_QueueInputBatch(infer.context, &mut input);
                }
            }
        }).await?;

        Ok(())
    }

    pub async fn dequeue_batch(ctx: &Arc<InferContext>) -> Result<BatchOutput>{


        let result = tokio::task::spawn_blocking({
            let infer = ctx.clone();
            move || {
                
                let batch_output = unsafe {
                    let mut batch_output = std::mem::MaybeUninit::zeroed();
                    NvDsInferContext_DequeueOutputBatch(infer.context, batch_output.as_mut_ptr());
                    batch_output.assume_init()
                };

                BatchOutput {
                    ctx: infer,
                    batch: batch_output
                }
            }

        }).await?;

        Ok(result)
    }

    pub async fn start(&self, config: DetectorConfig) {
        let mut start_future = self.inner.startup_task.lock().await;
        if let Some(handle) = start_future.take() {
            handle.abort();
        }

        let inner = self.inner.clone();

        let jh = tokio::spawn(async move {
            let max_batch = config.maxBatchSize as usize;
            let infer = Arc::new(InferContext::new(config).await?);
            let (s, r) = channel(20);

            for _ in 0 .. 20 {
                s.try_send(Surface::new(infer.net_info.width, infer.net_info.height, true).await?)?;
            }

            inner.pool_receiver.lock().await.replace(r);
            inner.pool_sender.lock().await.replace(s);

            let (s, mut r) = channel(max_batch);

            let input_task = tokio::spawn({
                let inner = inner.clone();
                let infer = infer.clone();
                async move {
                    loop {
                        let mut batch = Vec::new();
                        {
                            let source_lock = inner.sources.lock().await;

                            for val in source_lock.values() {
                                if let Some(s) = val.upgrade() {
                                    if let Some(b) = s.image.lock().await.take() {
                                        batch.push(b);
                                    }
                                }
                            }              
                        }
                        if batch.len() > 0 {
                            let _ = s.send(batch.clone()).await;
                            Self::queue_batch(&infer, batch).await?;
                        } else {
                            inner.frame_ready.notified().await;
                        }
                    }
                }
            });

            let result_sender = inner.result_sender.clone();

            let output_task = tokio::spawn({
                let infer = infer.clone();
                async move {
                    loop {
                        let output_infer = Self::dequeue_batch(&infer).await?;
                        if let Some(output_batch) = r.recv().await {

                            let mut results = Vec::new();
                            
                            for i in 0 .. output_batch.len() {
                                let image = output_batch[i].clone();
                                let detections = unsafe {
                                    (*output_infer.batch.frames.offset(i as isize)).__bindgen_anon_1.detectionOutput
                                };

                                let source_width  = image.source_frame.width() as f32;
                                let source_height = image.source_frame.height() as f32;
                                
                                let infer_width  = image.infer_frame.width() as f32;
                                let infer_height = image.infer_frame.height() as f32;

                                let width_scale = source_width / infer_width;
                                let height_scale = source_height / infer_height;

                                let mut detections_list: Vec<Detection> = Vec::new();

                                for id in 0 .. detections.numObjects {
                                    let detection = unsafe{*detections.objects.offset(id as isize)};

                                    let left = detection.left * width_scale;
                                    let top = detection.top * height_scale;

                                    let width = detection.width * width_scale;
                                    let height = detection.height * height_scale;

                                    let mut x1 = left / source_width;
                                    let mut y1 = top / source_height;
                                    let mut x2 = x1 + (width / source_width);
                                    let mut y2 = y1 + (height / source_height);

                                    x1 = x1.max(0.);
                                    y1 = y1.max(0.);
                                    x2 = x2.min(1.);
                                    y2 = y2.min(1.);

                                    let polygon = geo_types::Geometry::from(geo_types::Polygon::new(
                                        geo_types::LineString::from(vec![
                                            (x1, y1),
                                            (x2, y1),
                                            (x2, y2),
                                            (x1, y2),
                                            (x1, y1),
                                        ]),
                                        vec![],
                                    ));

                                    let foot = geo_types::Geometry::from(geo_types::LineString::from(vec![
                                        (x1, y2),
                                        (x2, y2),
                                    ]));

                                    detections_list.push(Detection {
                                        class_id: infer.config.class_keys[detection.classIndex as usize].clone(),
                                        confidence: detection.confidence,
                                        left, top, width, height,
                                        rect: polygon, foot
                                    })
                                }

                                results.push(
                                    Arc::new(BufferResult {
                                        detections: detections_list,
                                        image,
                                        encoded: Mutex::new(None)
                                    })
                                )
                            }

                            let _ = result_sender.try_send(results);
                        }
                    }
                }
            });

            inner.input_task.lock().await.replace(input_task);
            inner.output_task.lock().await.replace(output_task);

            Result::<()>::Ok(())
        });

        start_future.replace(jh);
    }

    #[allow(dead_code)]
    pub async fn stop(&self) {
        if let Some(task) = self.inner.startup_task.lock().await.take() {
            task.abort();
        }
        if let Some(task) = self.inner.input_task.lock().await.take() {
            task.abort();
        }
        if let Some(task) = self.inner.output_task.lock().await.take() {
            task.abort();
        }

        self.inner.pool_receiver.lock().await.take();
        self.inner.pool_sender.lock().await.take();
    }

    pub async fn add_source(&self, source: &T) -> Arc<Source<T>> {
        let next_id = self.inner.next_source_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let source = Arc::new(Source {
            my_id: next_id,
            detector: Arc::downgrade(&self.inner),
            image: Mutex::new(None),
            source: source.clone()
        });

        self.inner.sources.lock().await.insert(next_id, Arc::downgrade(&source));

        source
    }

    pub async fn get_next(&self) -> Option<Vec<Arc<BufferResult<T>>>> {
        return self.inner.result_receiver.lock().await.recv().await;
    }
}

impl<T: Sync + Send + Clone> Drop for DetectorInner<T> {
    fn drop(&mut self) {
        
    }
}

pub struct Source<T: 'static + Sync + Send + Clone> {
    my_id: u64,
    detector: Weak<DetectorInner<T>>,
    image: Mutex<Option<Arc<BatchImage<T>>>>,
    source: T
}

impl<T: 'static + Sync + Send + Clone> Drop for Source<T> {
    fn drop(&mut self) {
        if let Some(detector) = self.detector.upgrade() {
            let id = self.my_id;
            tokio::spawn(async move {
                let mut sources = detector.sources.lock().await;
                sources.remove(&id);
            });
        }
    }
}

impl<T: Sync + Send + Clone> Source<T> {


    pub async fn send_frame(&self, sample: &gst::Sample) -> Result<()> {
        if let Some(inner) = Weak::upgrade(&self.detector) {
            if let Some(buffer) = sample.buffer() { 
                
                let mut pool = inner.pool_receiver.lock().await;
                let pool_return = inner.pool_sender.lock().await;

                if let Some(pool_r) = &mut *pool {
                    if let Some(pool_s) = &*pool_return {
                        let infer_surface = pool_r.try_recv()?;
                        let sender = pool_s.clone();
        
                        drop(pool);
                        drop(pool_return);

                        let buffer = buffer.to_owned();
                        let surface = Surface::from_buffer(buffer).await?;
        
                        surface.place(&infer_surface).await?;

                        let batch_image = BatchImage {
                            infer_frame: infer_surface,
                            source_frame: surface,
                            pool: sender,
                            source: self.source.clone()
                        };

                        self.image.lock().await.replace(Arc::new(batch_image));
                        inner.frame_ready.notify_one();
                    }
                }
            }
        }

        Ok(())
    }
}