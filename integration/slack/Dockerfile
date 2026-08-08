# syntax=docker/dockerfile:1
# Slack proxy. Installs into a venv in the builder, copies the venv into a
# clean runtime image — no pip, no build tools, no writable site-packages.

FROM python:3.13-slim AS builder

WORKDIR /app
ENV PIP_DISABLE_PIP_VERSION_CHECK=1

COPY pyproject.toml ./
COPY src/ src/

RUN python -m venv /opt/venv \
    && /opt/venv/bin/pip install --no-cache-dir .

# ---------------------------------------------------------------------------
# Runtime stage
# ---------------------------------------------------------------------------
FROM python:3.13-slim

RUN useradd -u 10001 -s /bin/false -M arlo

COPY --from=builder /opt/venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH" PYTHONUNBUFFERED=1

USER 10001

CMD ["arlo-slack"]
