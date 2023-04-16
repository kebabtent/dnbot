FROM rust:1.68-buster as builder

WORKDIR /opt/galaxyofdreams
COPY Cargo.toml Cargo.lock ./
COPY common/Cargo.toml common/
COPY voice/Cargo.toml voice/
RUN mkdir galaxyofdreams \
    && echo "fn main() {}" > galaxyofdreams/main.rs \
    && mkdir -p common/src voice/src \
    && touch common/src/lib.rs voice/src/lib.rs \
    && cargo build --release --bin galaxyofdreams \
    && rm -rf galaxyofdreams common voice
COPY galaxyofdreams galaxyofdreams/
COPY common common/
COPY voice voice/
RUN touch galaxyofdreams/main.rs common/src/lib.rs voice/src/lib.rs \
    && cargo build --release --bin galaxyofdreams

FROM debian:buster-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && apt-get install -y openssl \
 && apt-get install -y ffmpeg
RUN update-ca-certificates
COPY --from=builder /opt/galaxyofdreams/target/release/galaxyofdreams /usr/local/bin/galaxyofdreams
CMD ["galaxyofdreams"]
