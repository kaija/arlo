# syntax=docker/dockerfile:1
# Multi-stage Next.js builder for the arlo web client.

FROM node:22-alpine AS builder

WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci

COPY . .
RUN npm run build

# ---------------------------------------------------------------------------
# Runtime stage
# ---------------------------------------------------------------------------
FROM node:22-alpine AS runner

WORKDIR /app

ENV NODE_ENV=production

# Create non-root user
RUN addgroup -g 10001 -S arlo && adduser -u 10001 -S arlo -G arlo

COPY --from=builder --chown=arlo:arlo /app/.next/standalone ./
COPY --from=builder --chown=arlo:arlo /app/.next/static ./.next/static

USER arlo

EXPOSE 3000

ENV PORT=3000
ENV HOSTNAME=0.0.0.0

CMD ["node", "server.js"]
