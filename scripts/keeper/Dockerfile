# The live keeper: price relays + flow keeper in one loop (live.sh).
# Secrets (set with `fly secrets set`, never baked in): KEEPER_PRIVATE_KEY, PYTH_API_KEY.
FROM node:22-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app/scripts/keeper
COPY scripts/keeper/package.json scripts/keeper/package-lock.json ./
RUN npm ci --ignore-scripts && npm cache clean --force
COPY scripts/keeper/ ./
# live.sh only needs the keeper scripts; nodes/ templates are read by some of them.
COPY nodes/ /app/nodes/
CMD ["./live.sh"]
