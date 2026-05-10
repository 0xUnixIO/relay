.DEFAULT_GOAL := help
.PHONY: help dev dev-master dev-node dev-web dev-reset build check fmt lint test clean web-install release release-test seed-dev-node hooks

CARGO ?= cargo
BUN   ?= bun

help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"} /^[a-zA-Z_-]+:.*##/ {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

## ---------- dev ----------

# All dev state lives under .dev/ so the host stays clean.
DEV_DIR        := $(CURDIR)/.dev
DEV_MASTER_PKI := $(DEV_DIR)/master-pki
DEV_NODE_PKI   := $(DEV_DIR)/node-pki

dev: web-install ## Run master + node + web concurrently (Ctrl-C to stop all)
	@echo "==> launching relay-master, relay-node, web (state in .dev/)"
	@mkdir -p $(DEV_MASTER_PKI) $(DEV_NODE_PKI)
	@trap 'echo "==> stopping..."; kill 0' INT TERM; \
	  ( trap - INT TERM EXIT; \
	    RUST_LOG=$${RUST_LOG:-info} \
	    MASTER_PUBLIC_ADDR=$${MASTER_PUBLIC_ADDR:-127.0.0.1} \
	    MASTER_PKI_DIR=$(DEV_MASTER_PKI) \
	    $(CARGO) run -q -p relay-master 2>&1 | sed -u 's/^/[master] /' ) & \
	  ( trap - INT TERM EXIT; \
	    $(MAKE) -s seed-dev-node 2>&1 | sed -u 's/^/[seed]   /' && \
	    set -a; . ./.relay-dev.env; set +a; \
	    RUST_LOG=$${RUST_LOG:-info} \
	    NODE_PKI_DIR=$(DEV_NODE_PKI) \
	    NODE_MASTER_ENDPOINT=https://127.0.0.1:7443 \
	    NODE_MASTER_SERVER_NAME=127.0.0.1 \
	    NODE_CA_CERT_B64=$$(base64 < $(DEV_MASTER_PKI)/ca.crt | tr -d '\n') \
	    $(CARGO) run -q -p relay-node 2>&1 | sed -u 's/^/[node]   /' ) & \
	  ( trap - INT TERM EXIT; \
	    cd web && $(BUN) run dev 2>&1 | sed -u 's/^/[web]    /' ) & \
	  wait

dev-master: ## Run only the master (state in .dev/)
	@mkdir -p $(DEV_MASTER_PKI)
	MASTER_PUBLIC_ADDR=$${MASTER_PUBLIC_ADDR:-127.0.0.1} \
	  MASTER_PKI_DIR=$(DEV_MASTER_PKI) \
	  $(CARGO) run -p relay-master

dev-node: ## Run only the node agent (requires master + seed-dev-node first)
	@mkdir -p $(DEV_NODE_PKI)
	@test -f $(DEV_MASTER_PKI)/ca.crt || { echo "==> master CA not found at $(DEV_MASTER_PKI)/ca.crt — run dev-master first" >&2; exit 1; }
	@test -f .relay-dev.env || { echo "==> .relay-dev.env not found — run 'make seed-dev-node' first" >&2; exit 1; }
	set -a; . ./.relay-dev.env; set +a; \
	  NODE_PKI_DIR=$(DEV_NODE_PKI) \
	  NODE_MASTER_ENDPOINT=https://127.0.0.1:7443 \
	  NODE_MASTER_SERVER_NAME=127.0.0.1 \
	  NODE_CA_CERT_B64=$$(base64 < $(DEV_MASTER_PKI)/ca.crt | tr -d '\n') \
	  $(CARGO) run -p relay-node

dev-web: web-install ## Run only the frontend dev server
	cd web && $(BUN) run dev

PG_CONTAINER := relay-postgres
PG_VOLUME    := relay-pgdata

dev-reset: ## Wipe .dev/ state + recreate Postgres container (DESTRUCTIVE)
	@echo "==> stopping any running relay processes"
	@pkill -f relay-master || true
	@pkill -f relay-node   || true
	@echo "==> removing .dev/, .relay-dev.env"
	@rm -rf .dev .relay-dev.env
	@echo "==> removing postgres container + volume"
	@docker rm -f $(PG_CONTAINER) 2>/dev/null || true
	@docker volume rm $(PG_VOLUME) 2>/dev/null || true
	@echo "==> starting fresh postgres container"
	@docker run -d --name $(PG_CONTAINER) \
	  -e POSTGRES_USER=relay \
	  -e POSTGRES_PASSWORD=relay \
	  -e POSTGRES_DB=relay \
	  -p 127.0.0.1:5432:5432 \
	  --health-cmd "pg_isready -U relay" \
	  --health-interval 2s \
	  --health-timeout 2s \
	  --health-retries 15 \
	  -v $(PG_VOLUME):/var/lib/postgresql/data \
	  postgres:16-alpine >/dev/null
	@printf "==> waiting for postgres"; \
	for i in $$(seq 1 30); do \
	  STATUS=$$(docker inspect --format='{{.State.Health.Status}}' $(PG_CONTAINER) 2>/dev/null); \
	  [ "$$STATUS" = "healthy" ] && echo " ready" && exit 0; \
	  printf '.'; sleep 1; \
	done; echo " timeout" && exit 1
	@echo "==> done. Run 'make dev' to start fresh."

web-install: web/node_modules ## Ensure web deps installed
web/node_modules: web/package.json
	cd web && $(BUN) install
	@touch web/node_modules

## ---------- build / check ----------

build: ## Release build (rust + web)
	$(CARGO) build --release --workspace
	cd web && $(BUN) run build

check: ## cargo check + tsc + vite build
	$(CARGO) check --workspace
	cd web && $(BUN) run typecheck && $(BUN) run build

fmt: ## Format Rust code
	$(CARGO) fmt --all

lint: ## Clippy + frontend lint (lint script optional)
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	-cd web && $(BUN) run lint

test: ## Run Rust tests
	$(CARGO) test --workspace

clean: ## Remove build artifacts
	$(CARGO) clean
	rm -rf web/dist web/node_modules web/.vite web/tsconfig.tsbuildinfo

release: ## Tag v$(version) and push — triggers the release workflow
	@VERSION=$$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2); \
	  TAG="v$${VERSION}"; \
	  echo "==> pushing commits"; \
	  git push origin HEAD; \
	  echo "==> tagging $$TAG"; \
	  git tag "$$TAG" && git push origin "$$TAG"

release-test: ## Tag a pre-release v$(version)-rc.<timestamp> and push (won't bump 'latest')
	@VERSION=$$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2); \
	  STAMP=$$(date -u +%Y%m%d%H%M%S); \
	  TAG="v$${VERSION}-rc.$${STAMP}"; \
	  echo "==> pushing commits"; \
	  git push origin HEAD; \
	  echo "==> tagging $$TAG"; \
	  git tag "$$TAG" && git push origin "$$TAG"

hooks: ## Enable .githooks/ as the local git hooks dir (pre-commit fmt+tsc, pre-push clippy+build)
	@git config core.hooksPath .githooks
	@echo "==> hooks enabled (core.hooksPath=.githooks)"
	@echo "    pre-commit: cargo fmt --check + bun tsc"
	@echo "    pre-push:   cargo clippy + bun run build"
	@echo "    跳过：git commit/push --no-verify"

## ---------- seeding ----------

MASTER_URL ?= http://127.0.0.1:7080
ADMIN_USER ?= admin
ADMIN_PASS ?= admin
DEV_NODE_ID ?= node-dev-1
DEV_NODE_TOKEN ?= dev-enrollment-token

seed-dev-node: ## Register $(DEV_NODE_ID) and write token to .relay-dev.env
	@echo "==> waiting for master at $(MASTER_URL)..."; \
	for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do \
	    curl -fsS $(MASTER_URL)/health >/dev/null 2>&1 && break; \
	    sleep 1; \
	done; \
	if [ -f .relay-dev.env ] && grep -q '^NODE_TOKEN=.\+' .relay-dev.env; then \
	    echo "==> .relay-dev.env exists, reusing existing dev node token"; \
	    exit 0; \
	fi; \
	curl -sS -X POST $(MASTER_URL)/api/v1/auth/bootstrap \
	    -H 'content-type: application/json' \
	    -d '{"username":"$(ADMIN_USER)","password":"$(ADMIN_PASS)"}' >/dev/null 2>&1 || true; \
	LOGIN_RESP=$$(curl -sS -X POST $(MASTER_URL)/api/v1/auth/login \
	    -H 'content-type: application/json' \
	    -d '{"username":"$(ADMIN_USER)","password":"$(ADMIN_PASS)"}'); \
	TOK=$$(echo "$$LOGIN_RESP" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d["token"])' 2>/dev/null); \
	if [ -z "$$TOK" ]; then \
	    echo "==> ERROR: login failed (wrong password or DB not reset?)" >&2; \
	    echo "    response: $$LOGIN_RESP" >&2; \
	    echo "    hint: DROP SCHEMA public CASCADE; CREATE SCHEMA public;" >&2; \
	    exit 1; \
	fi; \
	curl -fsS -X DELETE $(MASTER_URL)/api/v1/nodes/$(DEV_NODE_ID) \
	    -H "authorization: Bearer $$TOK" >/dev/null 2>&1 || true ; \
	RESP=$$(curl -fsS -X POST $(MASTER_URL)/api/v1/nodes \
	    -H "authorization: Bearer $$TOK" \
	    -H 'content-type: application/json' \
	    -d '{"id":"$(DEV_NODE_ID)","tags":["dev"]}') ; \
	ENR=$$(echo "$$RESP" | python3 -c 'import sys,json;print(json.load(sys.stdin)["enrollment_token"])') ; \
	if [ -z "$$ENR" ]; then echo "==> ERROR: failed to seed dev node" >&2; exit 1; fi ; \
	echo "NODE_ID=$(DEV_NODE_ID)" >  .relay-dev.env ; \
	echo "NODE_TOKEN=$$ENR"        >> .relay-dev.env ; \
	echo "==> seeded dev node $(DEV_NODE_ID); token saved to .relay-dev.env" ; \
	TUNNEL_RESP=$$(curl -fsS -X POST $(MASTER_URL)/api/v1/tunnels \
	    -H "authorization: Bearer $$TOK" \
	    -H 'content-type: application/json' \
	    -d '{"name":"dev-tunnel","protocol":"tcp","node_ids":["$(DEV_NODE_ID)"]}') ; \
	TUNNEL_ID=$$(echo "$$TUNNEL_RESP" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])' 2>/dev/null) ; \
	if [ -z "$$TUNNEL_ID" ]; then echo "==> WARNING: failed to seed default tunnel" >&2; exit 0; fi ; \
	echo "==> seeded default tunnel $$TUNNEL_ID" ; \
	curl -fsS -X POST $(MASTER_URL)/api/v1/forwards \
	    -H "authorization: Bearer $$TOK" \
	    -H 'content-type: application/json' \
	    -d "{\"tunnel_id\":\"$$TUNNEL_ID\",\"name\":\"dev-forward\",\"remote_addrs\":[\"127.0.0.1:8000\"]}" >/dev/null 2>&1 \
	    && echo "==> seeded default forward (auto port → 127.0.0.1:8000)" \
	    || echo "==> WARNING: failed to seed default forward" >&2
