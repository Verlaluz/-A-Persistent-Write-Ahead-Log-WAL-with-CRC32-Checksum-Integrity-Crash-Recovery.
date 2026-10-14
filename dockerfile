FROM rust:latest
WORKDIR /usr/src/write_ahead_log
COPY . .
RUN cargo build --release
ENTRYPOINT ["./target/release/write_ahead_log"]