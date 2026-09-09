# Kannaka in a container. Fetches the static musl linux release binary (runs on
# any base) and drops it into a tiny alpine image.
#
#   docker build -t kannaka --build-arg VERSION=0.11.1 -f packaging/docker/Dockerfile .
#   docker run --rm kannaka --version
#   docker run --rm -v kannaka-data:/data kannaka remember "hello" --importance 0.8
#
# Multi-arch: `docker buildx build --platform linux/amd64,linux/arm64 ...`
# (TARGETARCH is provided automatically by buildx).

ARG VERSION=0.11.1

FROM alpine:3.20 AS fetch
ARG VERSION
ARG TARGETARCH
RUN apk add --no-cache curl ca-certificates \
 && case "${TARGETARCH:-amd64}" in \
      amd64) A=x86_64 ;; \
      arm64) A=aarch64 ;; \
      *) echo "unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && curl -fsSL -o /kannaka \
      "https://github.com/kannaka-labs/kannaka-memory/releases/download/v${VERSION}/kannaka-linux-${A}" \
 && chmod +x /kannaka \
 && /kannaka --version

FROM alpine:3.20
LABEL org.opencontainers.image.source="https://github.com/kannaka-labs/kannaka-memory"
LABEL org.opencontainers.image.description="Kannaka — wave-interference (HRM) memory for AI agents"
LABEL org.opencontainers.image.licenses="MIT"
COPY --from=fetch /kannaka /usr/local/bin/kannaka
ENV KANNAKA_DATA_DIR=/data
VOLUME ["/data"]
ENTRYPOINT ["kannaka"]
CMD ["--help"]
