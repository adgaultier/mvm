FROM alpine:latest

RUN apk add --no-cache \
    bash \
    ca-certificates \
    curl \
    wget \
    tar

RUN addgroup -S agent && \
    adduser -S -G agent -h /home/agent agent

USER agent

WORKDIR /home/agent
RUN curl -fsSL https://fx.sh/setup.sh | bash
RUN curl -LsSf https://astral.sh/uv/install.sh | sh
ENV PATH="/home/agent/.local/bin:${PATH}"

COPY --chown=agent:agent mvm-agent-mcp /tmp/mvm-agent-mcp

RUN cd /tmp/mvm-agent-mcp && uv tool install .
WORKDIR /home/agent
