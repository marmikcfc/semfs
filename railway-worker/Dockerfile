FROM node:24-bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive
ENV WORKSPACE_BENCH_ROOT=/opt/Workspace-Bench
ENV WORKSPACE_BENCH_EVAL_ROOT=/opt/Workspace-Bench/evaluation
ENV RIP_BENCH_ROOT=/opt/Workspace-Bench
ENV RIP_BENCH_EVAL_ROOT=/opt/Workspace-Bench/evaluation
ENV DEEPAGENTS_ROOT=/opt/Workspace-Bench/deepagents
ENV CODEX_SANDBOX_MODE=danger-full-access

ARG WORKSPACE_BENCH_REF=main

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash \
    ca-certificates \
    curl \
    git \
    python3 \
    python3-pip \
    python3-venv \
    python-is-python3 \
    unzip \
    wget \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m pip install --no-cache-dir --break-system-packages \
    tqdm \
    pyyaml \
    uv \
    huggingface_hub

RUN npm install -g @openai/codex@0.133.0

RUN git clone https://github.com/OpenDataBox/Workspace-Bench.git /opt/Workspace-Bench \
    && cd /opt/Workspace-Bench \
    && git checkout "${WORKSPACE_BENCH_REF}"

COPY worker.sh /usr/local/bin/railway-benchmark-worker
RUN chmod +x /usr/local/bin/railway-benchmark-worker

WORKDIR /opt/Workspace-Bench/evaluation

CMD ["/usr/local/bin/railway-benchmark-worker"]
