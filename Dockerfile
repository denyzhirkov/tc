# -- build stage --
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /src
COPY . .

RUN cargo build --release --package tc-server && \
    strip target/release/tc-server

# -- runtime stage --
FROM scratch

COPY --from=builder /src/target/release/tc-server /tc-server

EXPOSE 7100/tcp 7101/udp

ENTRYPOINT ["/tc-server"]
