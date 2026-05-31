# ==============================================================================
# Stage 1: Build Cloud-Blender-Render from source
# ==============================================================================
FROM ubuntu:22.04 AS builder
ENV DEBIAN_FRONTEND=noninteractive
WORKDIR /build

# Install build dependencies
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev libclang-dev \
      curl ca-certificates git && \
    rm -rf /var/lib/apt/lists/*

# Install Rust toolchain
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

# Install Node.js directly (no nvm - simpler for Docker)
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && \
    apt-get install -y nodejs

# Build frontend (copy package files first for layer caching)
COPY frontend/package.json frontend/package-lock.json ./frontend/
RUN cd frontend && npm install

COPY frontend/ ./frontend/
RUN cd frontend && npm run build

# Build Rust backend (copy manifests first for dependency caching)
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release 2>/dev/null || true && \
    rm -rf src

COPY src/ ./src/
RUN touch src/main.rs && cargo build --release

# ==============================================================================
# Stage 2: Runtime image
# ==============================================================================
FROM ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
WORKDIR /workspace

# System prerequisites & libraries
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      wget gnupg ca-certificates software-properties-common \
      build-essential pkg-config libssl-dev libclang-dev git \
      libxi6 libfontconfig1 libxrender1 \
      libboost-all-dev libgl1-mesa-dev libglu1-mesa \
      libsm-dev libxkbcommon-x11-dev imagemagick \
      python3 python3-pip python3-dev curl \
      inotify-tools && \
    rm -rf /var/lib/apt/lists/*

# Environment setup
ENV LD_LIBRARY_PATH="/usr/local/cuda/lib64:/usr/lib/x86_64-linux-gnu"
ENV PATH="/usr/local/cuda/bin:${PATH}"
ENV NVIDIA_VISIBLE_DEVICES=all
ENV NVIDIA_DRIVER_CAPABILITIES=all

# Prepare /app and put all binaries inside
RUN mkdir /app

# Install Node.js via nvm
RUN curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh | bash && \
    /bin/bash -c "source ~/.nvm/nvm.sh && nvm install 22" && \
    /bin/bash -c "source ~/.nvm/nvm.sh && nvm use 22"

# Add nvm and node to PATH for all users
ENV NVM_DIR="/root/.nvm"
ENV PATH="$NVM_DIR/versions/node/v22.19.0/bin:$PATH"

# Install Jupyter Lab and common packages
RUN pip3 install --no-cache-dir \
    jupyterlab \
    notebook \
    numpy \
    pandas \
    matplotlib \
    seaborn \
    pillow \
    requests \
    jupyter-archive

# Configure JupyterLab to start in dark mode
RUN mkdir -p ~/.jupyter/lab/user-settings/@jupyterlab/apputils-extension/ && \
    echo '{"theme": "JupyterLab Dark"}' > ~/.jupyter/lab/user-settings/@jupyterlab/apputils-extension/themes.jupyterlab-settings

# DragonflyDB binary
RUN wget https://dragonflydb.gateway.scarf.sh/latest/dragonfly-x86_64.tar.gz && \
    tar -xf dragonfly-x86_64.tar.gz && \
    chmod u+x dragonfly-x86_64 && \
    mv dragonfly-x86_64 /app/ && \
    rm dragonfly-x86_64.tar.gz

# Blender CLI
RUN wget https://download.blender.org/release/Blender5.1/blender-5.1.0-linux-x64.tar.xz && \
    mkdir /app/blender && \
    tar xf blender-5.1.0-linux-x64.tar.xz -C /app/blender --strip-components 1 && \
    rm blender-5.1.0-linux-x64.tar.xz

# Cloud-Blender-Render binary (compiled from source in builder stage)
COPY --from=builder /build/target/release/Cloud-Blender-Render /app/Cloud-Blender-Render
RUN chmod u+x /app/Cloud-Blender-Render

# Copy Python script for Cycles OptiX denoise logic
COPY cycles_optix_denoise_logic.py /app/

# Copy extension watcher script
COPY watch_extensions.sh /app/

# Copy start.sh and make executable
COPY start.sh /start.sh
RUN chmod +x /start.sh

# Expose ports
EXPOSE 8888 6379 4000

# Entrypoint
ENTRYPOINT ["/start.sh"]