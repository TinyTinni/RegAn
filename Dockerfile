####################################################################################################
## Builder
####################################################################################################
FROM rust:1.95 AS builder

RUN rustup target add x86_64-unknown-linux-musl
RUN apt-get update && apt-get install -y musl-tools musl-dev \
    && rm -rf /var/lib/apt/lists/*

RUN addgroup --gid 10001 --system nonroot \
    && adduser  --uid 10000 --system --ingroup nonroot --home /home/nonroot nonroot

WORKDIR /regan

COPY Cargo.toml Cargo.lock ./
COPY image_collection/Cargo.toml image_collection/Cargo.toml
COPY server/Cargo.toml server/Cargo.toml
COPY simulation/Cargo.toml simulation/Cargo.toml
RUN mkdir image_collection/src server/src simulation/src && \
    echo "fn main() {}" > image_collection/src/lib.rs && \
    echo "fn main() {}" > server/src/main.rs && \
    echo "fn main() {}" > simulation/src/main.rs
RUN cargo build --target x86_64-unknown-linux-musl --release 2>&1 || true

COPY ./ .
RUN touch image_collection/src/lib.rs server/src/main.rs simulation/src/main.rs
RUN cargo build --target x86_64-unknown-linux-musl --release

####################################################################################################
## Final image
####################################################################################################

FROM scratch

EXPOSE 80

COPY --from=builder /etc/passwd /etc/passwd
COPY --from=builder /etc/group /etc/group

WORKDIR /regan

COPY --from=builder /regan/target/x86_64-unknown-linux-musl/release/server ./

USER nonroot:nonroot

ENTRYPOINT ["/regan/server"]

CMD ["--image-dir", "/var/regan/images/", "--port", "80", "--output", "/var/regan/results.db"]
