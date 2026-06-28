# --- Frontend build ---
FROM node:20-bookworm AS frontend
WORKDIR /fe
COPY frontend/package.json frontend/package-lock.json* ./
RUN npm install --no-audit --no-fund
COPY frontend ./
RUN npm run build

# --- Rust build ---
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml ./
COPY Cargo.lock* ./
COPY src ./src
COPY crates ./crates
COPY --from=frontend /fe/dist ./frontend/dist
RUN cargo build --release --bin booth

# --- Runtime ---
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/booth /usr/local/bin/booth
EXPOSE 8080
CMD ["booth", "--config", "/app/config.toml"]
