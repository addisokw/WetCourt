# Standalone crimes editor. Build context is the orchestrator/ dir (the cargo
# workspace root) so the shared crimes-core crate is available.
#
#   docker build -f crates/crimes-editor/Dockerfile -t crimes-editor ./orchestrator
#
# Two-stage like the booth image: build the SolidJS bundle, then embed it in the
# Rust binary via rust-embed.

# --- Frontend build ---
FROM node:20-bookworm AS frontend
WORKDIR /fe
COPY crates/crimes-editor/frontend/package.json crates/crimes-editor/frontend/package-lock.json* ./
RUN npm install --no-audit --no-fund
COPY crates/crimes-editor/frontend ./
RUN npm run build

# --- Rust build ---
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY crates ./crates
# rust-embed reads frontend/dist relative to the editor crate.
COPY --from=frontend /fe/dist ./crates/crimes-editor/frontend/dist
# Only the editor and its deps (crimes-core) compile; the booth crate is not in
# the dependency closure of this bin, so its own frontend/dist is never needed.
RUN cargo build --release --bin crimes-editor

# --- Runtime ---
FROM debian:bookworm-slim
COPY --from=build /src/target/release/crimes-editor /usr/local/bin/crimes-editor
EXPOSE 8080
CMD ["crimes-editor", "--crimes", "/app/crimes/wet_court_crimes.json", "--listen", "0.0.0.0:8080"]
