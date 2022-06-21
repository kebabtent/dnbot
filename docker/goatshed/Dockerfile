FROM rust:1.57-buster as builder

WORKDIR /opt/goatshed
COPY Cargo.toml Cargo.lock ./
COPY common/Cargo.toml common/
COPY modules/Cargo.toml modules/
COPY voice/Cargo.toml voice/
RUN mkdir goatshed \
    && echo "fn main() {}" > goatshed/main.rs \
    && mkdir -p common/src modules/src voice/src \
    && touch common/src/lib.rs modules/src/lib.rs voice/src/lib.rs \
    && cargo build --release --bin goatshed \
    && rm -rf goatshed common modules voice
COPY goatshed goatshed/
COPY common common/
COPY modules modules/
COPY voice voice/
RUN touch goatshed/main.rs common/src/lib.rs modules/src/lib.rs voice/src/lib.rs \
    && cargo build --release --bin goatshed

FROM debian:buster-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && apt-get install -y openssl \
 && apt-get install -y ffmpeg
RUN update-ca-certificates
COPY --from=builder /opt/goatshed/target/release/goatshed /usr/local/bin/goatshed
CMD ["goatshed"]
