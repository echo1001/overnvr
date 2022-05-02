#include <opencv2/opencv.hpp>
#include "wrapper.hpp"

int surface_encode(NvBufSurface *surf, surface_encoded *encoded) {

    auto out_vec = new std::vector<uchar>();

    if (NvBufSurfaceMap(surf, 0, 0, NVBUF_MAP_READ) != 0) {
        delete out_vec;
        return -1;
    }
    if (NvBufSurfaceSyncForCpu(surf, 0, 0) != 0) {
        //NvBufSurfaceUnMap(surf, 0, 0);
        //delete out_vec;
        //return -1;
    }
    try {
        auto surfp = surf->surfaceList[0];

        if (
            surfp.colorFormat == NVBUF_COLOR_FORMAT_NV12_709_ER ||
            surfp.colorFormat == NVBUF_COLOR_FORMAT_NV12_709 || 
            surfp.colorFormat == NVBUF_COLOR_FORMAT_NV12
            ) {
            auto out_mat = cv::Mat(surfp.planeParams.height[0], surfp.planeParams.width[0],
                                CV_8UC1, surfp.mappedAddr.addr[0],
                                surfp.planeParams.pitch[0]);
            cv::cvtColor(out_mat, out_mat, cv::ColorConversionCodes::COLOR_YUV2BGR_NV12);
            std::vector<uchar> &out_vec_ref = *out_vec;
            cv::imencode(".jpg", out_mat, out_vec_ref,  {cv::IMWRITE_JPEG_QUALITY, 95});
        }
        else {

            auto out_mat = cv::Mat(surfp.height, surfp.width,
                                    CV_8UC4, surfp.mappedAddr.addr[0],
                                    surfp.pitch);
            cv::cvtColor(out_mat, out_mat, cv::ColorConversionCodes::COLOR_RGBA2BGR);
            std::vector<uchar> &out_vec_ref = *out_vec;
            cv::imencode(".jpg", out_mat, out_vec_ref,  {cv::IMWRITE_JPEG_QUALITY, 95});
        }


    }
    catch(...) {
        NvBufSurfaceUnMap(surf, 0, 0);
        delete out_vec;
        return -1;
    }

    NvBufSurfaceUnMap(surf, 0, 0);
    *encoded = out_vec;

    return 0;
}

unsigned long surface_len(surface_encoded encoded) {
    std::vector<uchar>* vec = (std::vector<uchar>*)encoded;

    return vec->size();
}

unsigned char* surface_data(surface_encoded encoded) {
    std::vector<uchar>* vec = (std::vector<uchar>*)encoded;

    return vec->data();
}

void surface_free(surface_encoded encoded) {
    std::vector<uchar>* vec = (std::vector<uchar>*)encoded;

    delete vec;
}