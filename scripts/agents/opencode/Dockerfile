FROM docker/sandbox-templates:opencode
RUN  curl -LsSf https://astral.sh/uv/install.sh | sh
COPY mvm-agent-mcp /tmp/mvm-agent-mcp
RUN cd /tmp/mvm-agent-mcp && uv tool install .
