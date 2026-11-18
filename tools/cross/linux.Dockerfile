# Builds and tests the workspace's server crates on Linux, with what the
# status bar icon needs to compile (GTK 3, an appindicator library, xdo).
FROM rust:1-bookworm
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends \
    libgtk-3-dev libxdo-dev libayatana-appindicator3-dev pkg-config git ca-certificates >/dev/null \
 && rm -rf /var/lib/apt/lists/*
ENV CARGO_TARGET_DIR=/target
WORKDIR /src
