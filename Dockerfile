FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --release -p openevents-server -p openevents-sim

FROM debian:bookworm-slim
RUN useradd --system --home /data openevents && mkdir -p /data && chown openevents /data
COPY --from=build /src/target/release/openevents /src/target/release/openevents-sim /usr/local/bin/
COPY examples/profiles /profiles
COPY examples/schemas /schemas
USER openevents
ENV OPENEVENTS_DATA=/data OPENEVENTS_ADDR=0.0.0.0:8080
VOLUME /data
EXPOSE 8080
CMD ["openevents"]
