use bindgen;
use cc;

use std::env;
use std::path::PathBuf;

fn main() {

    println!("cargo:rustc-link-search=/opt/cerbero/build/dist/linux_x86_64/lib/");
    println!("cargo:rustc-link-search=/opt/nvidia/deepstream/deepstream/lib/");
    println!("cargo:rustc-link-search=/usr/local/cuda/lib64/");
    println!("cargo:rustc-link-lib=cudart");

    println!("cargo:rustc-link-lib=nvdsgst_helper");
    println!("cargo:rustc-link-lib=nvdsgst_meta");
    println!("cargo:rustc-link-lib=nvds_meta");
    println!("cargo:rustc-link-lib=nvds_infer");
    println!("cargo:rustc-link-lib=nvbufsurface");
    println!("cargo:rustc-link-lib=nvbufsurftransform");
    println!("cargo:rustc-link-lib=cuda");
    println!("cargo:rustc-link-lib=dl");
    println!("cargo:rustc-link-lib=pthread");
    
    let mut build =  cc::Build::new();
    build
        .compiler("cc")
        .cpp(true)
        .flag("-std=c++11")
        .flag_if_supported("-Wa,-mbig-obj")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-missing-field-initializers")
        .include("/opt/nvidia/deepstream/deepstream/sources/includes/")
        .include("/usr/local/cuda/include/")
        .include("/usr/include/opencv/")
        .file("src/wrapper.cpp");

    build.compile("wrapper");

    println!("cargo:rerun-if-changed=src/wrapper.hpp");
    println!("cargo:rerun-if-changed=src/wrapper.cpp");
    println!("cargo:rerun-if-changed=build.rs");

    let bindings = bindgen::Builder::default()
        .clang_arg("-I/opt/nvidia/deepstream/deepstream/sources/includes/")
        .clang_arg("-I/usr/local/cuda/include/")
        .clang_arg("-I/usr/lib/llvm-6.0/lib/clang/6.0.0/include/")
        .header("src/wrapper.hpp")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        .allowlist_type("NvBufSurf.*")
        .allowlist_type("CUgraphicsMapResourceFlags_enum")
        .allowlist_function("NvBufSurf.*")
        .allowlist_function("surface_.*")
        .allowlist_function("cuGraphicsUnregisterResource")
        .allowlist_function("cuGraphicsEGLRegisterImage")
        .allowlist_function("cuGraphicsResourceGetMappedEglFrame")
        .allowlist_function("cudaSetDevice")
        .allowlist_function("NvDsInferContext_.*")
        .layout_tests(false)
        .disable_header_comment()
        .generate_comments(false)
        
        .opaque_type("INvDsInferContext")
        .prepend_enum_name(false)
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");

    /*println!("cargo:rustc-link-lib=opencv_stitching");
    println!("cargo:rustc-link-lib=opencv_aruco");
    println!("cargo:rustc-link-lib=opencv_bgsegm");
    println!("cargo:rustc-link-lib=opencv_bioinspired");
    println!("cargo:rustc-link-lib=opencv_ccalib");
    println!("cargo:rustc-link-lib=opencv_dnn_objdetect");
    println!("cargo:rustc-link-lib=opencv_dnn_superres");
    println!("cargo:rustc-link-lib=opencv_dpm");
    println!("cargo:rustc-link-lib=opencv_highgui");
    println!("cargo:rustc-link-lib=opencv_face");
    println!("cargo:rustc-link-lib=opencv_freetype");
    println!("cargo:rustc-link-lib=opencv_fuzzy");
    println!("cargo:rustc-link-lib=opencv_hfs");
    println!("cargo:rustc-link-lib=opencv_img_hash");
    println!("cargo:rustc-link-lib=opencv_line_descriptor");
    println!("cargo:rustc-link-lib=opencv_quality");
    println!("cargo:rustc-link-lib=opencv_reg");
    println!("cargo:rustc-link-lib=opencv_rgbd");
    println!("cargo:rustc-link-lib=opencv_saliency");
    println!("cargo:rustc-link-lib=opencv_shape");
    println!("cargo:rustc-link-lib=opencv_stereo");
    println!("cargo:rustc-link-lib=opencv_structured_light");
    println!("cargo:rustc-link-lib=opencv_phase_unwrapping");
    println!("cargo:rustc-link-lib=opencv_superres");
    println!("cargo:rustc-link-lib=opencv_optflow");
    println!("cargo:rustc-link-lib=opencv_surface_matching");
    println!("cargo:rustc-link-lib=opencv_tracking");
    println!("cargo:rustc-link-lib=opencv_datasets");
    println!("cargo:rustc-link-lib=opencv_text");
    println!("cargo:rustc-link-lib=opencv_dnn");
    println!("cargo:rustc-link-lib=opencv_plot");
    println!("cargo:rustc-link-lib=opencv_ml");
    println!("cargo:rustc-link-lib=opencv_videostab");
    println!("cargo:rustc-link-lib=opencv_videoio");
    println!("cargo:rustc-link-lib=opencv_ximgproc");
    println!("cargo:rustc-link-lib=opencv_video");
    println!("cargo:rustc-link-lib=opencv_xobjdetect");
    println!("cargo:rustc-link-lib=opencv_objdetect");
    println!("cargo:rustc-link-lib=opencv_calib3d");
    println!("cargo:rustc-link-lib=opencv_features2d");
    println!("cargo:rustc-link-lib=opencv_flann");
    println!("cargo:rustc-link-lib=opencv_xphoto");
    println!("cargo:rustc-link-lib=opencv_photo");*/
    println!("cargo:rustc-link-lib=opencv_imgcodecs");
    println!("cargo:rustc-link-lib=opencv_imgproc");
    println!("cargo:rustc-link-lib=opencv_core");
}
