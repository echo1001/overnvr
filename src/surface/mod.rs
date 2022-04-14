use crate::ffi;
use crate::Result;
use std::ffi::c_void;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::Mutex;
use core::slice;

use anyhow::bail;

use gst::Buffer;

use gst_sys::{
    gst_buffer_map, 
    gst_buffer_unmap,
    GST_MAP_READ
};

use ffi::{
    NvBufSurface, 
    NvBufSurfaceCreate, 
    NvBufSurfaceCreateParams, 
    NvBufSurfaceDestroy,
    NvBufSurfaceUnMapEglImage,
    NVBUF_COLOR_FORMAT_RGBA, 
    NVBUF_LAYOUT_PITCH, 
    NVBUF_MEM_CUDA_UNIFIED,

    NvBufSurfTransformConfigParams,
    NvBufSurfTransformRect,
    NvBufSurfTransformParams,
    NvBufSurfTransformSetSessionParams,
    NvBufSurfTransform,

    NvBufSurfTransformInter_Nearest,
    NvBufSurfTransform_None,
    NVBUFSURF_TRANSFORM_CROP_DST, 
    NVBUFSURF_TRANSFORM_CROP_SRC, 
    NVBUFSURF_TRANSFORM_FILTER,
    NvBufSurfTransformCompute_Default,

    surface_encoded,
    surface_encode,
    surface_len,
    surface_data,
    surface_free,

    CUgraphicsResource,
    cuGraphicsUnregisterResource,
    CUstream_st
};

#[cfg(target_arch = "aarch64")]
use ffi::{
    NvBufSurfaceMapEglImage,
    NVBUF_MEM_DEFAULT, 
    cuGraphicsEGLRegisterImage,
    cuGraphicsResourceGetMappedEglFrame,
    CU_GRAPHICS_MAP_RESOURCE_FLAGS_NONE,
};


struct InnerSurface {
    surf: *mut NvBufSurface,
    #[allow(dead_code)]
    buf: Option<Buffer>,
    owned: bool,
    encoded: Mutex<Option<Arc<EncodedImage>>>,
    data_ptr: Option<*mut c_void>,
    cuda_resource: Option<CUgraphicsResource>
}

unsafe impl Sync for InnerSurface {}
unsafe impl Send for InnerSurface {}

impl Drop for InnerSurface {
    fn drop(&mut self) {
        if let Some(resource) = self.cuda_resource {
            unsafe {
                cuGraphicsUnregisterResource (resource);
                NvBufSurfaceUnMapEglImage (self.surf, 0);
            }
        }
        if self.owned { 
            unsafe {
                NvBufSurfaceDestroy(self.surf);
            }
        }
    }
}
impl InnerSurface {
    fn new(width: u32, height: u32, gl: bool) -> Result<InnerSurface> {
        let mut params = NvBufSurfaceCreateParams{
            colorFormat: NVBUF_COLOR_FORMAT_RGBA,
            gpuId: 0,
            height,
            width,
            isContiguous: true,
            layout: NVBUF_LAYOUT_PITCH,
            #[cfg(target_arch = "aarch64")]
            memType: NVBUF_MEM_DEFAULT,
            #[cfg(not(target_arch = "aarch64"))]
            memType: NVBUF_MEM_CUDA_UNIFIED,
            size: 0,
        };

        let surf = unsafe {
            let mut surf = std::mem::MaybeUninit::zeroed();
            let r = NvBufSurfaceCreate(surf.as_mut_ptr(), 1, &mut params);
            if r == -1 {
                bail!("Could not create surface");
            }
            surf.assume_init()
        };

        #[allow(unused_mut)]
        let mut cuda_resource: Option<CUgraphicsResource> = None;

        let ptr = unsafe {
            #[cfg(not(target_arch = "aarch64"))]
            {
                (*(*surf).surfaceList).dataPtr
            }
            #[cfg(target_arch = "aarch64")]
            {
                if gl {

                    unsafe {
                        ffi::cudaSetDevice(0);
                    };
                    let r = NvBufSurfaceMapEglImage(surf, 0);

                    if r != 0 {
                        panic!("Unable to map EGL image: {}", r);
                    }
                    
                    let mut p_cuda_resource = std::mem::MaybeUninit::zeroed();
                    let mut egl_frame = std::mem::MaybeUninit::zeroed();
                    
                    let r = cuGraphicsEGLRegisterImage(
                        p_cuda_resource.as_mut_ptr(),
                        (*(*surf).surfaceList).mappedAddr.eglImage, 
                        CU_GRAPHICS_MAP_RESOURCE_FLAGS_NONE
                    );
                    if r != 0 {
                        panic!("Unable to register EGL image: {}", r);
                    }

                    let p_cuda_resource = p_cuda_resource.assume_init();
                    cuda_resource.replace(p_cuda_resource);

                    let r = cuGraphicsResourceGetMappedEglFrame(
                        egl_frame.as_mut_ptr(),
                        p_cuda_resource,
                        0,
                        0
                    );
                    if r != 0 {
                        panic!("Unable to map EGL frame: {}", r);
                    }

                    let egl_frame = egl_frame.assume_init();

                    egl_frame.frame.pPitch[0]
                    
                } else { (*(*surf).surfaceList).dataPtr }
            }
        };

        Ok(InnerSurface {
            surf,
            buf: None,
            owned: true,
            encoded: Mutex::new(None),
            data_ptr: Some(ptr),
            cuda_resource
        })
    }

    fn from_buffer(buffer: Buffer) -> Result<InnerSurface> {
        let surf = unsafe {
            let buffer_ptr = buffer.as_mut_ptr();
            let mut map_info = std::mem::MaybeUninit::zeroed();
            if gst_buffer_map(buffer_ptr, map_info.as_mut_ptr(), GST_MAP_READ) == 0 {
                bail!("Unable to read buffer");
            }
            gst_buffer_unmap(buffer_ptr, map_info.as_mut_ptr());
            (*map_info.as_mut_ptr()).data as *mut NvBufSurface
        };

        Ok(InnerSurface {
            surf,
            buf: Some(buffer),
            owned: false,
            encoded: Mutex::new(None),
            data_ptr: None, 
            cuda_resource: None
        })
    }
    #[allow(dead_code)]
    fn encode(&self) -> Result<Arc<EncodedImage>> {
        if let Ok(mut encoded_lock) = self.encoded.lock() {
            if let Some(encoded) = &*encoded_lock {
                return Ok(encoded.clone());
            }
            
            let encoded = unsafe {
                let mut encoded = std::mem::MaybeUninit::zeroed();
                if surface_encode(self.surf, encoded.as_mut_ptr()) != 0 {
                    bail!("encode error");
                }
                encoded.assume_init()
            };
            let result = Arc::new(EncodedImage{
                image: encoded
            });
    
            encoded_lock.replace(result.clone());
    
            return Ok(result)

        }
        bail!("not able to lock");
    }

    fn place(&self, other: &Self) -> Result<()> {
        unsafe {

            if let Ok(mut encoded) = other.encoded.lock() {
                let mut config = NvBufSurfTransformConfigParams {
                    compute_mode: NvBufSurfTransformCompute_Default,
                    cuda_stream: 0 as *mut CUstream_st,
                    gpu_id: 0
                };

                if NvBufSurfTransformSetSessionParams(&mut config) != 0 {
                    bail!("unable to initialize transform");
                }

                let this_surf = *(*self.surf).surfaceList;
                let other_surf = *(*other.surf).surfaceList;
                
                let mut from_rect = NvBufSurfTransformRect {
                    height: this_surf.height,
                    width: this_surf.width,
                    left: 0,
                    top: 0
                };

                let mut to_rect = NvBufSurfTransformRect {
                    height: other_surf.height,
                    width: other_surf.width,
                    left: 0,
                    top: 0
                };

                let mut params = NvBufSurfTransformParams {
                    src_rect: &mut from_rect,
                    dst_rect: &mut to_rect,
                    transform_filter: NvBufSurfTransformInter_Nearest,
                    transform_flip: NvBufSurfTransform_None,
                    transform_flag: NVBUFSURF_TRANSFORM_CROP_DST | NVBUFSURF_TRANSFORM_CROP_SRC | NVBUFSURF_TRANSFORM_FILTER
                };

                if NvBufSurfTransform(self.surf,other.surf, &mut params) != 0 {
                    bail!("unable to transform surface");
                }

                encoded.take();
            }
        }
        Ok(())
    }

    fn crop(&self, other: &Self, left: u32, top: u32, width: u32, height: u32) -> Result<()> {
        unsafe {
            let mut config = NvBufSurfTransformConfigParams {
                compute_mode: NvBufSurfTransformCompute_Default,
                cuda_stream: 0 as *mut CUstream_st,
                gpu_id: 0
            };

            if NvBufSurfTransformSetSessionParams(&mut config) != 0 {
                bail!("unable to initialize transform");
            }

            let this_surf = *(*self.surf).surfaceList;
            let other_surf = *(*other.surf).surfaceList;
            
            let mut from_rect = NvBufSurfTransformRect {
                height,
                width,
                left,
                top
            };

            let mut to_rect = NvBufSurfTransformRect {
                height: other_surf.height,
                width: other_surf.width,
                left: 0,
                top: 0
            };

            let mut params = NvBufSurfTransformParams {
                src_rect: &mut from_rect,
                dst_rect: &mut to_rect,
                transform_filter: NvBufSurfTransformInter_Nearest,
                transform_flip: NvBufSurfTransform_None,
                transform_flag: NVBUFSURF_TRANSFORM_CROP_DST | NVBUFSURF_TRANSFORM_CROP_SRC | NVBUFSURF_TRANSFORM_FILTER
            };

            if NvBufSurfTransform(self.surf,other.surf, &mut params) != 0 {
                bail!("unable to transform surface");
            }

            if let Ok(mut encoded) = other.encoded.lock() {
                encoded.take();
            }
        }
        Ok(())

    }
}

#[derive(Clone)]
pub struct Surface {
    inner: Arc<InnerSurface>
}

unsafe impl Sync for Surface {}
unsafe impl Send for Surface {}

impl Surface {
    pub(crate) async fn new(width: u32, height: u32, gl: bool) -> Result<Surface> {
        let surf = tokio::task::spawn_blocking(move || {
            let inner = InnerSurface::new(width, height, gl)?;

            Result::<Surface>::Ok(Surface {
                inner: Arc::new(inner)
            })
        }).await??;

        Ok(surf)
    }

    pub(crate) async fn from_buffer(buffer: Buffer) -> Result<Surface> {
        let surf = tokio::task::spawn_blocking(move || {
            let inner = InnerSurface::from_buffer(buffer)?;

            Result::<Surface>::Ok(Surface {
                inner: Arc::new(inner)
            })
        }).await??;

        Ok(surf)
    }

    #[allow(dead_code)]
    pub(crate) async fn encode(&self) -> Result<Arc<EncodedImage>> {
        let inner = self.inner.clone();

        let encoded = tokio::task::spawn_blocking(move || {
            let encoded = inner.encode()?;
            Result::<Arc<EncodedImage>>::Ok(encoded)
        }).await??;

        Ok(encoded)
    }

    pub(crate) async fn place(&self, other: &Self) -> Result<()> {
        let source_inner = self.inner.clone();
        let dst_inner = other.inner.clone();

        tokio::task::spawn_blocking(move || {
            source_inner.place(&*dst_inner)?;
            Result::<()>::Ok(())
        }).await??;

        Ok(())
    }

    pub(crate) async fn crop(&self, other: &Self, left: u32, top: u32, width: u32, height: u32) -> Result<()> {
        let source_inner = self.inner.clone();
        let dst_inner = other.inner.clone();

        tokio::task::spawn_blocking(move || {
            source_inner.crop(&*dst_inner, left, top, width, height)?;
            Result::<()>::Ok(())
        }).await??;

        Ok(())
    }

    pub(crate) fn get_ptr(&self) -> Option<*mut c_void> {
        return self.inner.data_ptr;
    }

    pub(crate) fn pitch(&self) -> u32 {
        unsafe { (*(*self.inner.surf).surfaceList).planeParams.pitch[0] }
    }

    pub(crate) fn width(&self) -> u32 {
        unsafe { (*(*self.inner.surf).surfaceList).width }
    }

    pub(crate) fn height(&self) -> u32 {
        unsafe { (*(*self.inner.surf).surfaceList).height }
    }

    pub(crate) fn pts(&self) -> Option<gst::ClockTime> {
        if let Some(buffer) = &self.inner.buf {
            return buffer.pts()
        }
        None
    }
}

impl Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Surface")
    }
}

pub struct EncodedImage {
    image: surface_encoded,
}

impl std::ops::Deref for EncodedImage {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { slice::from_raw_parts(surface_data(self.image), surface_len(self.image) as usize) }
    }
}

impl std::ops::DerefMut for EncodedImage {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { slice::from_raw_parts_mut(surface_data(self.image), surface_len(self.image) as usize) }
    }
}

impl Drop for EncodedImage {
    fn drop(&mut self) {
        unsafe {
            surface_free(self.image);
        }
    }
}

unsafe impl Sync for EncodedImage {}
unsafe impl Send for EncodedImage {}