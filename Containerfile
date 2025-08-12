# Multi-stage build for wetware Rust application
FROM rust:alpine as builder

# Install build dependencies
RUN apk add --no-cache \
    pkgconfig \
    openssl-dev \
    musl-dev

# Set working directory
WORKDIR /usr/src/app

# Copy manifests
COPY Cargo.toml Cargo.lock* ./

# Create a dummy main.rs to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs

# Build dependencies only (this layer will be cached if dependencies don't change)
RUN cargo build --release

# Remove dummy main.rs and copy real source code
RUN rm src/main.rs
COPY src/ ./src/

# Build the application
RUN cargo build --release

# Runtime stage
FROM alpine:latest

# Install runtime dependencies
RUN apk add --no-cache \
    ca-certificates \
    libssl3

# Create non-root user
RUN addgroup -g 1000 wetware && \
    adduser -D -s /bin/sh -u 1000 -G wetware wetware

# Set working directory
WORKDIR /app

# Copy binary from builder stage
COPY --from=builder /usr/src/app/target/release/ww /app/ww

# Change ownership to non-root user
RUN chown wetware:wetware /app/ww

# Switch to non-root user
USER wetware

# Expose port (adjust if your app uses different ports)
EXPOSE 8080

# Set the binary as entrypoint
ENTRYPOINT ["/app/ww"]
