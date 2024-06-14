####################################################################################################
## Builder
####################################################################################################
FROM rust:1.79 AS builder

RUN rustup target add x86_64-unknown-linux-musl
RUN apt update && apt install -y musl-tools musl-dev

RUN addgroup --gid 10001 --system nonroot \
 && adduser  --uid 10000 --system --ingroup nonroot --home /home/nonroot nonroot

WORKDIR /regan

COPY ./ .
RUN cargo build --target x86_64-unknown-linux-musl --release

####################################################################################################
## Final image
####################################################################################################
FROM scratch

COPY --from=builder /etc/passwd /etc/passwd
COPY --from=builder /etc/group /etc/group

WORKDIR /regan

COPY --from=builder /regan/target/x86_64-unknown-linux-musl/release/server ./
COPY --from=builder /regan/static/ ./static/

USER nonroot:nonroot

ENTRYPOINT ["/regan/server"]

CMD ["--image-dir", "/var/regan/images/",\ 
     "--port" , "80", \
     "--output", "/var/regan/results.db"]
