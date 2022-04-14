FROM nvcr.io/nvidia/deepstream:6.0.1-devel

# Gstreamer
RUN apt-get update && apt-get upgrade -y

RUN apt-get install -y \
    build-essential \
    libclang-dev \
    libegl1-mesa-dev \
    pkg-config\
    libgstreamer1.0-0 \
    libgstreamer1.0-0-dbg \
    libgstreamer1.0-dev \
    gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad \
    gstreamer1.0-plugins-ugly \
    libgstreamer-plugins-base1.0-dev \
    curl clang

ENV LD_LIBRARY_PATH=/opt/nvidia/deepstream/deepstream/lib/

run cd /opt && git clone https://github.com/marcoslucianops/DeepStream-Yolo yolo && cd yolo/nvdsinfer_custom_impl_Yolo/ && \
    OPENCV=1 CUDA_VER=11.3 make
    
# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
COPY . /opt/overnvr
ENV PATH=$PATH:/root/.cargo/bin
RUN cd /opt/overnvr && cargo build --release
CMD /opt/overnvr/target/release/overnvr