.PHONY: setup infra migrate control-plane node dev-control-plane dev-node test lint frontend demo demo-flow cli

setup:
	cargo fetch
	cd frontend && npm install

infra:
	docker compose up -d

migrate:
	cargo run -p control-plane -- --migrate-only

control-plane:
	bash scripts/with-env.sh cargo run -p control-plane

dev-control-plane: control-plane

node:
	cargo run -p bench-node -- --asset-id robot-01 --control-plane http://localhost:50051

dev-node: node

test:
	DATABASE_URL=$${DATABASE_URL:-postgres://bench:bench@localhost:5432/bench} cargo test --workspace

.PHONY: agent-demo-setup
agent-demo-setup:
	bash scripts/with-env.sh bash scripts/agent-demo-setup.sh

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

frontend:
	cd frontend && npm run dev

cli:
	cargo run -p bench-cli -- --help

demo:
	bash scripts/with-env.sh bash scripts/demo-stack.sh

.PHONY: demo-flow
demo-flow:
	bash scripts/demo.sh
