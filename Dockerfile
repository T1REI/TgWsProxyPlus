FROM rust:1.96-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs icon.ico ./
COPY src ./src
COPY assets ./assets
RUN cargo build --release --locked --no-default-features

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/TgWsProxyPlus /usr/local/bin/TgWsProxyPlus
COPY LICENSE LICENSE.upstream THIRD_PARTY_NOTICES.txt /usr/share/doc/TgWsProxyPlus/
USER 65534:65534
VOLUME /data
EXPOSE 1443
ENTRYPOINT ["/usr/local/bin/TgWsProxyPlus"]
CMD ["--console", "--config", "/data/config.json"]
