# Build stage
FROM ekidd/rust-musl-builder:nightly-2020-08-15 AS build

COPY --chown=rust:rust . ./

ENV RUST_LOG=info
RUN cargo fmt --all -- --check
RUN cargo clippy --all -- -D warnings
RUN cargo test --all

ARG debug

ENV BUILD_FEATURES=${debug:+"--features log-level-trace"}

RUN cargo build --release --no-default-features $BUILD_FEATURES

# Copy stage
FROM alpine:latest
RUN apk --no-cache add tini && \
    addgroup -g 1000 appuser && \
    adduser -S -u 1000 -g appuser -G appuser appuser
USER appuser
COPY --from=build /home/rust/src/target/x86_64-unknown-linux-musl/release/rusoto-sqs-k8s-demo /app/

# Setup the environment
ENTRYPOINT ["/sbin/tini", "--"]

# Runtime
ENV RUST_BACKTRACE=1
ENV RUST_LOG=debug

CMD ["/app/rusoto-sqs-k8s-demo"]
