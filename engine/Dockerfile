# NylonME 引擎镜像 — 多阶段构建
# 构建上下文为仓库根目录:  docker build -f engine/Dockerfile -t nylonme .

# ---------- 构建阶段 ----------
FROM rust:1-bookworm AS builder
WORKDIR /build

# 先只拷贝清单，利用 Docker 层缓存依赖编译
COPY engine/Cargo.toml engine/Cargo.lock ./engine/
COPY engine/crates ./engine/crates
COPY proto ./proto
COPY docs/api/openapi.json ./docs/api/openapi.json

WORKDIR /build/engine
RUN cargo build --release -p nylon-engine

# ---------- 运行阶段 ----------
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -m -d /home/nylon nylon

COPY --from=builder /build/engine/target/release/nylon-engine /usr/local/bin/nylon-engine

USER nylon
ENV NYLON_DATA_DIR=/home/nylon/data \
    NYLON_HTTP_ADDR=0.0.0.0:50052
VOLUME ["/home/nylon/data"]
EXPOSE 50051 50052

# gRPC :50051  REST/UI :50052
ENTRYPOINT ["nylon-engine"]
CMD ["serve", "0.0.0.0:50051"]
